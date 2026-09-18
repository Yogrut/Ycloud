use std::{
    collections::VecDeque,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;

const STATE_FILE: &str = "security-state.json";
const LEGACY_FILE: &str = "security-events.json";
const EVENTS_FILE: &str = "security-events.jsonl";
const MAX_RESTRICTIONS: usize = 20_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginEntry {
    Admin,
    Account,
    Web,
    WebDav,
}

#[derive(Clone, Copy, Debug)]
pub struct LoginPolicy {
    pub maximum_failures: u32,
    pub block_seconds: i64,
}

impl LoginEntry {
    pub fn fixed_policy(self) -> LoginPolicy {
        match self {
            Self::Admin => LoginPolicy {
                maximum_failures: 3,
                block_seconds: 60 * 60,
            },
            Self::Account => LoginPolicy {
                maximum_failures: 5,
                block_seconds: 60 * 60,
            },
            Self::Web => LoginPolicy {
                maximum_failures: 5,
                block_seconds: 60 * 60,
            },
            Self::WebDav => LoginPolicy {
                maximum_failures: 5,
                block_seconds: 60,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoginRecord {
    pub entry: LoginEntry,
    pub ip: String,
    pub failed_attempts: u32,
    pub blocked_until: Option<i64>,
    pub last_attempt_at: i64,
    pub last_success_at: Option<i64>,
    pub last_result: String,
    pub user_agent: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoginEvent {
    pub id: u64,
    pub entry: LoginEntry,
    pub success: bool,
    pub occurred_at: i64,
    pub ip: String,
    pub result: String,
    pub failed_attempts: u32,
    pub blocked_until: Option<i64>,
    pub user_agent: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoginEventView {
    #[serde(flatten)]
    pub event: LoginEvent,
    pub current_blocked_until: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoginEventPage {
    pub events: Vec<LoginEventView>,
    pub next_cursor: Option<u64>,
    pub total: usize,
    pub page: usize,
}

#[derive(Default)]
pub struct EventQuery<'a> {
    pub success: Option<bool>,
    pub entry: Option<LoginEntry>,
    pub search: Option<&'a str>,
    pub since: i64,
    pub cursor: Option<u64>,
    pub page: usize,
    pub limit: usize,
}

#[derive(Clone, Deserialize, Serialize)]
struct SecurityData {
    #[serde(default = "state_schema_version")]
    schema_version: u32,
    #[serde(default)]
    records: Vec<LoginRecord>,
}

#[derive(Deserialize)]
struct LegacySecurityData {
    #[serde(default)]
    records: Vec<LoginRecord>,
}

fn state_schema_version() -> u32 {
    2
}

struct EventLog {
    events: VecDeque<LoginEvent>,
    next_id: u64,
    disk_entries: usize,
    retention_days: u32,
    max_entries: usize,
}

#[derive(Clone)]
pub struct LoginSecurity {
    state_path: PathBuf,
    events_path: PathBuf,
    data: Arc<Mutex<SecurityData>>,
    event_log: Arc<Mutex<EventLog>>,
}

impl LoginSecurity {
    pub async fn load(
        config_path: &Path,
        retention_days: u32,
        max_entries: usize,
    ) -> anyhow::Result<Self> {
        let directory = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let state_path = directory.join(STATE_FILE);
        let events_path = directory.join(EVENTS_FILE);
        let data = load_state(directory, &state_path).await?;
        if data.schema_version != 2 || data.records.len() > MAX_RESTRICTIONS {
            anyhow::bail!("Login security state has an unsupported format");
        }
        let mut events = load_events(&events_path).await?;
        let next_id = events.back().map_or(1, |event| event.id.saturating_add(1));
        let disk_entries = events.len();
        prune_events(&mut events, retention_days, max_entries);
        let security = Self {
            state_path,
            events_path,
            data: Arc::new(Mutex::new(data)),
            event_log: Arc::new(Mutex::new(EventLog {
                events,
                next_id,
                disk_entries,
                retention_days,
                max_entries,
            })),
        };
        security
            .configure_retention(retention_days, max_entries)
            .await?;
        Ok(security)
    }

    pub async fn configure_retention(
        &self,
        retention_days: u32,
        max_entries: usize,
    ) -> anyhow::Result<()> {
        let mut log = self.event_log.lock().await;
        let settings_changed =
            log.retention_days != retention_days || log.max_entries != max_entries;
        let mut retained = log.events.clone();
        prune_events(&mut retained, retention_days, max_entries);
        if !settings_changed && retained.len() == log.events.len() {
            return Ok(());
        }
        log.events = retained;
        log.retention_days = retention_days;
        log.max_entries = max_entries;
        if let Err(error) = compact_events(&self.events_path, &log.events).await {
            log.disk_entries = usize::MAX;
            return Err(error);
        }
        log.disk_entries = log.events.len();
        Ok(())
    }

    pub async fn is_blocked(&self, entry: LoginEntry, ip: IpAddr) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let mut data = self.data.lock().await;
        let Some(record) = find_mut(&mut data.records, entry, ip) else {
            return Ok(false);
        };
        if record.blocked_until.is_some_and(|until| until > now) {
            return Ok(true);
        }
        if record.blocked_until.take().is_some() {
            record.failed_attempts = 0;
            record.last_result = "限制已到期".into();
            persist_state(&self.state_path, &data).await?;
        }
        Ok(false)
    }

    pub async fn record_failure(
        &self,
        entry: LoginEntry,
        ip: IpAddr,
        user_agent: Option<&str>,
        policy: LoginPolicy,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let event = {
            let mut data = self.data.lock().await;
            ensure_capacity(&mut data.records, entry, ip, now)?;
            let record = get_or_insert(&mut data.records, entry, ip, now);
            record.failed_attempts = record.failed_attempts.saturating_add(1);
            record.last_attempt_at = now;
            record.user_agent = sanitize_user_agent(user_agent);
            if record.failed_attempts >= policy.maximum_failures {
                record.blocked_until = Some(now + policy.block_seconds);
                record.last_result = "凭据错误，已限制".into();
            } else {
                record.last_result = "凭据错误".into();
            }
            let event = event_from_record(record, false, now);
            persist_state(&self.state_path, &data).await?;
            event
        };
        self.append_event(event).await
    }

    pub async fn record_success(
        &self,
        entry: LoginEntry,
        ip: IpAddr,
        user_agent: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let event = {
            let mut data = self.data.lock().await;
            if entry == LoginEntry::WebDav {
                let recent_clean_success =
                    find_mut(&mut data.records, entry, ip).is_some_and(|record| {
                        record.failed_attempts == 0
                            && record.blocked_until.is_none()
                            && record
                                .last_success_at
                                .is_some_and(|last| now.saturating_sub(last) < 60 * 60)
                    });
                if recent_clean_success {
                    return Ok(());
                }
            }
            ensure_capacity(&mut data.records, entry, ip, now)?;
            let record = get_or_insert(&mut data.records, entry, ip, now);
            record.failed_attempts = 0;
            record.blocked_until = None;
            record.last_attempt_at = now;
            record.last_success_at = Some(now);
            record.last_result = "登录成功".into();
            record.user_agent = sanitize_user_agent(user_agent);
            let event = event_from_record(record, true, now);
            persist_state(&self.state_path, &data).await?;
            event
        };
        self.append_event(event).await
    }

    pub async fn query_events(&self, query: EventQuery<'_>) -> LoginEventPage {
        let now = chrono::Utc::now().timestamp();
        let data = self.data.lock().await;
        let log = self.event_log.lock().await;
        let search = query
            .search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);
        let since = query.since.max(now - i64::from(log.retention_days) * 86400);
        let limit = query.limit.clamp(1, 100);
        let requested_page = query.page.max(1);
        let requested_start = requested_page.saturating_sub(1).saturating_mul(limit);
        let mut total = 0_usize;
        let mut selected = Vec::with_capacity(limit.saturating_add(1));
        let mut last_page = VecDeque::with_capacity(limit);
        for event in log.events.iter().rev() {
            let matches = event.occurred_at >= since
                && query.success.is_none_or(|value| event.success == value)
                && query.entry.is_none_or(|value| event.entry == value)
                && search.as_ref().is_none_or(|value| {
                    event.ip.contains(value)
                        || event.result.to_lowercase().contains(value)
                        || event
                            .user_agent
                            .as_deref()
                            .is_some_and(|agent| agent.to_lowercase().contains(value))
                });
            if !matches {
                continue;
            }
            let match_index = total;
            total = total.saturating_add(1);
            if let Some(cursor) = query.cursor {
                if event.id < cursor && selected.len() <= limit {
                    selected.push(event);
                }
            } else {
                if match_index >= requested_start && selected.len() <= limit {
                    selected.push(event);
                }
                if last_page.len() == limit {
                    last_page.pop_front();
                }
                last_page.push_back(event);
            }
        }
        let page = query.page.max(1).min(total.div_ceil(limit).max(1));
        let has_more = if query.cursor.is_some() || page == requested_page {
            selected.len() > limit
        } else {
            let last_page_size = total.saturating_sub(page.saturating_sub(1).saturating_mul(limit));
            let skip = last_page.len().saturating_sub(last_page_size);
            selected = last_page.into_iter().skip(skip).collect();
            false
        };
        selected.truncate(limit);
        let mut events = Vec::with_capacity(limit);
        for event in selected {
            let current_blocked_until = data
                .records
                .iter()
                .find(|record| record.entry == event.entry && record.ip == event.ip)
                .and_then(|record| record.blocked_until)
                .filter(|until| *until > now);
            events.push(LoginEventView {
                event: event.clone(),
                current_blocked_until,
            });
        }
        let next_cursor = if has_more {
            events.last().map(|event| event.event.id)
        } else {
            None
        };
        LoginEventPage {
            events,
            next_cursor,
            total,
            page,
        }
    }

    pub async fn clear_events(&self) -> anyhow::Result<()> {
        let mut log = self.event_log.lock().await;
        let empty = VecDeque::new();
        // Replace both copies, so recovery cannot restore deliberately cleared events.
        compact_events(&self.events_path, &empty).await?;
        log.events.clear();
        log.disk_entries = 0;
        compact_events(&self.events_path, &empty).await?;
        Ok(())
    }

    pub async fn unblock(&self, entry: LoginEntry, ip: IpAddr) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let event = {
            let mut data = self.data.lock().await;
            let Some(record) = find_mut(&mut data.records, entry, ip) else {
                return Ok(false);
            };
            record.failed_attempts = 0;
            record.blocked_until = None;
            record.last_attempt_at = now;
            record.last_result = "管理员已解除限制".into();
            let event = event_from_record(record, false, now);
            persist_state(&self.state_path, &data).await?;
            event
        };
        self.append_event(event).await?;
        Ok(true)
    }

    pub async fn restrict(
        &self,
        entry: LoginEntry,
        ip: IpAddr,
        policy: LoginPolicy,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let event = {
            let mut data = self.data.lock().await;
            ensure_capacity(&mut data.records, entry, ip, now)?;
            let record = get_or_insert(&mut data.records, entry, ip, now);
            record.blocked_until = Some(now + policy.block_seconds);
            record.last_attempt_at = now;
            record.last_result = "管理员已限制".into();
            let event = event_from_record(record, false, now);
            persist_state(&self.state_path, &data).await?;
            event
        };
        self.append_event(event).await
    }

    async fn append_event(&self, mut event: LoginEvent) -> anyhow::Result<()> {
        let mut log = self.event_log.lock().await;
        event.id = log.next_id;
        log.next_id = log.next_id.saturating_add(1);
        append_json_line(&self.events_path, &event).await?;
        log.disk_entries = log.disk_entries.saturating_add(1);
        log.events.push_back(event);
        let retention_days = log.retention_days;
        let max_entries = log.max_entries;
        prune_events(&mut log.events, retention_days, max_entries);
        let slack = 256.min((log.max_entries / 10).max(32));
        if log.disk_entries > log.max_entries.saturating_add(slack) {
            match compact_events(&self.events_path, &log.events).await {
                Ok(()) => log.disk_entries = log.events.len(),
                Err(error) => {
                    log.disk_entries = usize::MAX;
                    tracing::warn!(%error, "security event log compaction will be retried");
                }
            }
        }
        Ok(())
    }
}

fn event_from_record(record: &LoginRecord, success: bool, occurred_at: i64) -> LoginEvent {
    LoginEvent {
        id: 0,
        entry: record.entry,
        success,
        occurred_at,
        ip: record.ip.clone(),
        result: record.last_result.clone(),
        failed_attempts: record.failed_attempts,
        blocked_until: record.blocked_until,
        user_agent: record.user_agent.clone(),
    }
}

fn prune_events(events: &mut VecDeque<LoginEvent>, retention_days: u32, max_entries: usize) {
    let cutoff = chrono::Utc::now().timestamp() - i64::from(retention_days) * 24 * 60 * 60;
    while events
        .front()
        .is_some_and(|event| event.occurred_at < cutoff)
    {
        events.pop_front();
    }
    while events.len() > max_entries {
        events.pop_front();
    }
}

async fn load_state(directory: &Path, state_path: &Path) -> anyhow::Result<SecurityData> {
    let backup = backup_path(state_path);
    let data = if tokio::fs::try_exists(state_path).await? {
        read_state(state_path).await.or_else(|primary| {
            std::fs::read(&backup)
                .with_context(|| format!("Security state and backup are invalid: {primary}"))
                .and_then(|bytes| serde_json::from_slice(&bytes).context("Invalid backup"))
        })?
    } else {
        let legacy_path = directory.join(LEGACY_FILE);
        if tokio::fs::try_exists(&legacy_path).await? {
            let bytes = tokio::fs::read(&legacy_path).await?;
            let legacy: LegacySecurityData =
                serde_json::from_slice(&bytes).context("Legacy login security state is invalid")?;
            SecurityData {
                schema_version: 2,
                records: legacy.records,
            }
        } else {
            SecurityData {
                schema_version: 2,
                records: Vec::new(),
            }
        }
    };
    if !tokio::fs::try_exists(state_path).await? {
        persist_state(state_path, &data).await?;
    }
    Ok(data)
}

async fn load_events(path: &Path) -> anyhow::Result<VecDeque<LoginEvent>> {
    let backup = event_backup_path(path);
    if !tokio::fs::try_exists(path).await? {
        if tokio::fs::try_exists(&backup).await? {
            restore_event_log(path, &backup).await?;
        } else {
            return Ok(VecDeque::new());
        }
    }
    secure_permissions(path).await?;
    match read_events(path).await {
        Ok(events) => Ok(events),
        Err(primary) if tokio::fs::try_exists(&backup).await? => {
            let events = read_events(&backup)
                .await
                .with_context(|| format!("Security event log and backup are invalid: {primary}"))?;
            restore_event_log(path, &backup).await?;
            Ok(events)
        }
        Err(error) => Err(error),
    }
}

async fn read_events(path: &Path) -> anyhow::Result<VecDeque<LoginEvent>> {
    let text = tokio::fs::read_to_string(path).await?;
    let lines: Vec<&str> = text.lines().collect();
    let mut events = VecDeque::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<LoginEvent>(line) {
            Ok(event) => events.push_back(event),
            Err(_) if index + 1 == lines.len() && !text.ends_with('\n') => break,
            Err(error) => return Err(error).context("Security event log is invalid"),
        }
    }
    Ok(events)
}

async fn append_json_line(path: &Path, event: &LoginEvent) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path).await?;
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    file.write_all(&line).await?;
    file.flush().await?;
    secure_permissions(path).await
}

async fn compact_events(path: &Path, events: &VecDeque<LoginEvent>) -> anyhow::Result<()> {
    let temporary = path.with_extension(format!("jsonl.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).await?;
    for event in events {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        file.write_all(&line).await?;
    }
    file.sync_all().await?;
    drop(file);
    let backup = event_backup_path(path);
    if tokio::fs::try_exists(&backup).await? {
        tokio::fs::remove_file(&backup).await?;
    }
    if tokio::fs::try_exists(path).await? {
        tokio::fs::rename(path, &backup).await?;
    }
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        if tokio::fs::try_exists(&backup).await.unwrap_or(false) {
            let _ = tokio::fs::rename(&backup, path).await;
        }
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error).context("Failed to publish compacted security event log");
    }
    secure_permissions(path).await
}

fn event_backup_path(path: &Path) -> PathBuf {
    path.with_extension("jsonl.bak")
}

async fn restore_event_log(path: &Path, backup: &Path) -> anyhow::Result<()> {
    let recovery = path.with_extension(format!("jsonl.{}.recover", Uuid::new_v4()));
    tokio::fs::copy(backup, &recovery).await?;
    let file = OpenOptions::new().write(true).open(&recovery).await?;
    file.sync_all().await?;
    drop(file);
    let corrupt = path.with_extension("jsonl.corrupt");
    if tokio::fs::try_exists(&corrupt).await? {
        tokio::fs::remove_file(&corrupt).await?;
    }
    if tokio::fs::try_exists(path).await? {
        tokio::fs::rename(path, &corrupt).await?;
    }
    if let Err(error) = tokio::fs::rename(&recovery, path).await {
        if tokio::fs::try_exists(&corrupt).await.unwrap_or(false) {
            let _ = tokio::fs::rename(&corrupt, path).await;
        }
        let _ = tokio::fs::remove_file(&recovery).await;
        return Err(error).context("Failed to restore security event log backup");
    }
    if tokio::fs::try_exists(&corrupt).await? {
        tokio::fs::remove_file(&corrupt).await?;
    }
    secure_permissions(path).await
}

fn find_mut(
    records: &mut [LoginRecord],
    entry: LoginEntry,
    ip: IpAddr,
) -> Option<&mut LoginRecord> {
    let ip = ip.to_string();
    records
        .iter_mut()
        .find(|record| record.entry == entry && record.ip == ip)
}

fn get_or_insert(
    records: &mut Vec<LoginRecord>,
    entry: LoginEntry,
    ip: IpAddr,
    now: i64,
) -> &mut LoginRecord {
    let ip = ip.to_string();
    if let Some(index) = records
        .iter()
        .position(|record| record.entry == entry && record.ip == ip)
    {
        return &mut records[index];
    }
    records.push(LoginRecord {
        entry,
        ip,
        failed_attempts: 0,
        blocked_until: None,
        last_attempt_at: now,
        last_success_at: None,
        last_result: String::new(),
        user_agent: None,
    });
    records.last_mut().expect("record was inserted")
}

fn ensure_capacity(
    records: &mut Vec<LoginRecord>,
    entry: LoginEntry,
    ip: IpAddr,
    now: i64,
) -> anyhow::Result<()> {
    let ip = ip.to_string();
    if records.len() >= MAX_RESTRICTIONS
        && !records
            .iter()
            .any(|record| record.entry == entry && record.ip == ip)
    {
        if let Some((index, _)) = records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.blocked_until.is_none_or(|until| until <= now))
            .min_by_key(|(_, record)| record.last_attempt_at)
        {
            records.remove(index);
        } else {
            anyhow::bail!("Login restriction capacity is exhausted");
        }
    }
    Ok(())
}

fn sanitize_user_agent(value: Option<&str>) -> Option<String> {
    value
        .map(|text| {
            text.chars()
                .filter(|character| !character.is_control())
                .take(256)
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
}

async fn read_state(path: &Path) -> anyhow::Result<SecurityData> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("Login security state is invalid")
}

async fn persist_state(path: &Path, data: &SecurityData) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(data)?;
    let temporary = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).await?;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    drop(file);
    let backup = backup_path(path);
    if tokio::fs::try_exists(&backup).await? {
        tokio::fs::remove_file(&backup).await?;
    }
    if tokio::fs::try_exists(path).await? {
        tokio::fs::rename(path, &backup).await?;
    }
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        if tokio::fs::try_exists(&backup).await.unwrap_or(false) {
            let _ = tokio::fs::rename(&backup, path).await;
        }
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error).context("Failed to publish login security state");
    }
    secure_permissions(path).await
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(unix)]
async fn secure_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| format!("Failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
async fn secure_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_capacity_query_counts_and_pages_in_memory() {
        let root = std::env::temp_dir().join(format!("ycloud-log-capacity-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let tracker = LoginSecurity::load(&root.join("config.json"), 7, 20_000)
            .await
            .unwrap();
        {
            let mut log = tracker.event_log.lock().await;
            for id in 1..=20_000 {
                log.events.push_back(LoginEvent {
                    id,
                    entry: LoginEntry::Admin,
                    success: id % 2 == 0,
                    occurred_at: chrono::Utc::now().timestamp(),
                    ip: "192.0.2.1".into(),
                    result: "登录成功".into(),
                    failed_attempts: 0,
                    blocked_until: None,
                    user_agent: Some("Mozilla/5.0 Test Browser".into()),
                });
            }
        }
        let started = std::time::Instant::now();
        let page = tracker
            .query_events(EventQuery {
                success: Some(true),
                search: Some("browser"),
                page: 100,
                limit: 100,
                ..Default::default()
            })
            .await;
        eprintln!("20,000-entry filtered log query: {:?}", started.elapsed());
        assert_eq!(page.total, 10_000);
        assert_eq!(page.page, 100);
        assert_eq!(page.events.len(), 100);
        assert!(page.next_cursor.is_none());
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "manual D1 performance baseline"]
    async fn retained_log_query_performance_baseline() {
        let root = std::env::temp_dir().join(format!("ycloud-log-bench-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let tracker = LoginSecurity::load(&root.join("config.json"), 7, 20_000)
            .await
            .unwrap();
        {
            let mut log = tracker.event_log.lock().await;
            for id in 1..=20_000 {
                log.events.push_back(LoginEvent {
                    id,
                    entry: if id % 3 == 0 {
                        LoginEntry::WebDav
                    } else {
                        LoginEntry::Admin
                    },
                    success: id % 2 == 0,
                    occurred_at: chrono::Utc::now().timestamp(),
                    ip: format!("192.0.2.{}", id % 251),
                    result: if id % 2 == 0 {
                        "登录成功".into()
                    } else {
                        "凭据无效".into()
                    },
                    failed_attempts: (id % 5) as u32,
                    blocked_until: None,
                    user_agent: Some(format!("Baseline Browser {}", id % 100)),
                });
            }
        }

        let started = std::time::Instant::now();
        let first = tracker
            .query_events(EventQuery {
                success: Some(false),
                search: Some("browser 4"),
                page: 10,
                limit: 100,
                ..Default::default()
            })
            .await;
        let first_elapsed = started.elapsed();
        std::hint::black_box(first);

        let repeated_started = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(
                tracker
                    .query_events(EventQuery {
                        success: Some(false),
                        search: Some("browser 4"),
                        page: 10,
                        limit: 100,
                        ..Default::default()
                    })
                    .await,
            );
        }
        let repeated_elapsed = repeated_started.elapsed();

        let concurrent_started = std::time::Instant::now();
        let mut queries = Vec::new();
        for _ in 0..4 {
            let tracker = tracker.clone();
            queries.push(tokio::spawn(async move {
                tracker
                    .query_events(EventQuery {
                        success: Some(false),
                        search: Some("browser 4"),
                        page: 10,
                        limit: 100,
                        ..Default::default()
                    })
                    .await
            }));
        }
        for query in queries {
            std::hint::black_box(query.await.unwrap());
        }
        eprintln!(
            "login_log_baseline entries=20000 first={first_elapsed:?} repeated_20={repeated_elapsed:?} concurrent_4={:?}",
            concurrent_started.elapsed()
        );
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn filtered_totals_pages_and_clear_preserve_restrictions() {
        let root = std::env::temp_dir().join(format!("ycloud-log-pages-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let config = root.join("config.json");
        let tracker = LoginSecurity::load(&config, 7, 500).await.unwrap();
        let ip: IpAddr = "192.0.2.23".parse().unwrap();
        for _ in 0..3 {
            tracker
                .record_failure(
                    LoginEntry::Admin,
                    ip,
                    Some("Test Browser"),
                    LoginEntry::Admin.fixed_policy(),
                )
                .await
                .unwrap();
        }
        tracker
            .record_success(LoginEntry::Web, ip, Some("Other Browser"))
            .await
            .unwrap();
        let page = tracker
            .query_events(EventQuery {
                success: Some(false),
                search: Some("TEST"),
                page: 2,
                limit: 2,
                ..Default::default()
            })
            .await;
        assert_eq!(page.total, 3);
        assert_eq!(page.page, 2);
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.id, 1);
        assert!(page.next_cursor.is_none());
        let clamped = tracker
            .query_events(EventQuery {
                success: Some(false),
                search: Some("TEST"),
                page: usize::MAX,
                limit: 2,
                ..Default::default()
            })
            .await;
        assert_eq!(clamped.total, 3);
        assert_eq!(clamped.page, 2);
        assert_eq!(clamped.events.len(), 1);
        assert_eq!(clamped.events[0].event.id, 1);
        assert!(clamped.next_cursor.is_none());
        let empty = tracker
            .query_events(EventQuery {
                search: Some("no match"),
                page: usize::MAX,
                limit: 20,
                ..Default::default()
            })
            .await;
        assert_eq!(empty.total, 0);
        assert_eq!(empty.page, 1);
        tracker.clear_events().await.unwrap();
        assert!(tracker.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        let reloaded = LoginSecurity::load(&config, 7, 500).await.unwrap();
        assert_eq!(
            reloaded
                .query_events(EventQuery {
                    limit: 20,
                    ..Default::default()
                })
                .await
                .total,
            0
        );
        assert!(reloaded.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        assert!(tokio::fs::read(event_backup_path(&root.join(EVENTS_FILE)))
            .await
            .unwrap()
            .is_empty());
        tracker
            .record_success(LoginEntry::Web, ip, None)
            .await
            .unwrap();
        assert_eq!(
            tracker
                .query_events(EventQuery {
                    limit: 20,
                    ..Default::default()
                })
                .await
                .total,
            1
        );
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn restrictions_and_event_pages_are_persistent() {
        let root = std::env::temp_dir().join(format!("ycloud-login-security-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let config = root.join("config.json");
        let tracker = LoginSecurity::load(&config, 7, 500).await.unwrap();
        let ip: IpAddr = "2001:db8::1234".parse().unwrap();
        for _ in 0..3 {
            tracker
                .record_failure(
                    LoginEntry::Admin,
                    ip,
                    Some("test-agent"),
                    LoginEntry::Admin.fixed_policy(),
                )
                .await
                .unwrap();
        }
        assert!(tracker.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        let page = tracker
            .query_events(EventQuery {
                success: Some(false),
                entry: Some(LoginEntry::Admin),
                limit: 2,
                ..Default::default()
            })
            .await;
        assert_eq!(page.events.len(), 2);
        assert!(page.next_cursor.is_some());

        let reloaded = LoginSecurity::load(&config, 7, 500).await.unwrap();
        assert!(reloaded.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        assert!(reloaded.unblock(LoginEntry::Admin, ip).await.unwrap());
        assert!(!reloaded.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn event_log_recovers_from_the_last_valid_compaction_backup() {
        let root = std::env::temp_dir().join(format!("ycloud-event-recovery-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let config = root.join("config.json");
        let tracker = LoginSecurity::load(&config, 7, 500).await.unwrap();
        let ip: IpAddr = "192.0.2.55".parse().unwrap();
        tracker
            .record_success(LoginEntry::Web, ip, Some("recovery-test"))
            .await
            .unwrap();
        tracker.configure_retention(7, 501).await.unwrap();
        tokio::fs::write(root.join(EVENTS_FILE), b"invalid event log\n")
            .await
            .unwrap();

        let reloaded = LoginSecurity::load(&config, 7, 501).await.unwrap();
        let page = reloaded
            .query_events(EventQuery {
                success: Some(true),
                entry: Some(LoginEntry::Web),
                limit: 20,
                ..Default::default()
            })
            .await;
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].event.ip, ip.to_string());
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
