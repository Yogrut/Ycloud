//! File-content traffic accounting. Bytes are charged durably before being
//! handed to the next stream consumer; network acknowledgements are not measured.
use crate::{
    config::SharedConfig,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Query, State},
    http::HeaderMap,
    response::Response,
    Json,
};
use chrono::{DateTime, Datelike, Months, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{BufRead, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Quota {
    pub enabled: bool,
    /// Zero means unlimited in this direction.
    pub upload: u64,
    pub download: u64,
}
impl Quota {
    pub fn validate(&self) -> AppResult<()> {
        if self.upload > 9_007_199_254_740_991 || self.download > 9_007_199_254_740_991 {
            return Err(AppError::BadRequest("流量额度超出支持范围".into()));
        }
        Ok(())
    }
    fn limit(&self, direction: Direction) -> u64 {
        if !self.enabled {
            return 0;
        }
        match direction {
            Direction::Upload => self.upload,
            Direction::Download => self.download,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CycleUnit {
    Hours,
    Days,
    Months,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResetCycle {
    pub unit: CycleUnit,
    pub every: u32,
    pub anchor: i64,
    pub offset_minutes: i32,
}
impl Default for ResetCycle {
    fn default() -> Self {
        Self {
            unit: CycleUnit::Months,
            every: 1,
            anchor: 1_704_067_200,
            offset_minutes: 0,
        }
    }
}
impl ResetCycle {
    fn next(&self, now: i64) -> i64 {
        if now < self.anchor {
            return self.anchor;
        }
        match self.unit {
            CycleUnit::Hours | CycleUnit::Days => {
                let seconds = i64::from(self.every)
                    * if self.unit == CycleUnit::Hours {
                        3600
                    } else {
                        86400
                    };
                self.anchor + ((now - self.anchor) / seconds + 1) * seconds
            }
            CycleUnit::Months => {
                let offset = i64::from(self.offset_minutes) * 60;
                let anchor =
                    DateTime::from_timestamp(self.anchor + offset, 0).expect("validated anchor");
                let date = DateTime::from_timestamp(now + offset, 0).expect("current date");
                let months = ((date.year() - anchor.year()) * 12 + date.month() as i32
                    - anchor.month() as i32)
                    .max(0) as u32;
                let mut count = months / self.every;
                loop {
                    let next = anchor
                        .checked_add_months(Months::new(count * self.every))
                        .expect("bounded date")
                        .timestamp()
                        - offset;
                    if next > now {
                        return next;
                    }
                    count += 1;
                }
            }
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrafficSettings {
    pub total: Quota,
    pub guest: Quota,
    /// Shared allowance across all ordinary user accounts.
    pub users_total: Quota,
    pub users: BTreeMap<String, Quota>,
    pub cycle: ResetCycle,
}
impl TrafficSettings {
    pub fn validate(&self) -> AppResult<()> {
        self.total.validate()?;
        self.guest.validate()?;
        self.users_total.validate()?;
        if self.users.len() > 1000 || self.users.keys().any(|id| id.len() > 128) {
            return Err(AppError::BadRequest("用户流量配置超出支持范围".into()));
        }
        for quota in self.users.values() {
            quota.validate()?;
        }
        let cycle = &self.cycle;
        if cycle.every == 0
            || cycle.every > 120
            || !(-840..=840).contains(&cycle.offset_minutes)
            || !(946_684_800..=4_102_444_800).contains(&cycle.anchor)
        {
            return Err(AppError::BadRequest("流量重置周期或起始时间无效".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Upload,
    Download,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Usage {
    pub upload: u64,
    pub download: u64,
}
impl Usage {
    fn amount(&self, direction: Direction) -> u64 {
        match direction {
            Direction::Upload => self.upload,
            Direction::Download => self.download,
        }
    }
    fn add(&mut self, direction: Direction, bytes: u64) {
        let value = match direction {
            Direction::Upload => &mut self.upload,
            Direction::Download => &mut self.download,
        };
        *value = value.saturating_add(bytes);
    }
}
#[derive(Clone, Deserialize, Serialize)]
struct Ledger {
    version: u32,
    sequence: u64,
    next_reset: i64,
    cycle: ResetCycle,
    last_time: i64,
    total: Usage,
    guest: Usage,
    #[serde(default)]
    users_total: Usage,
    users: BTreeMap<String, Usage>,
    days: BTreeMap<String, Usage>,
}
impl Ledger {
    fn new(cycle: ResetCycle) -> Self {
        let now = Utc::now().timestamp();
        Self {
            version: 1,
            sequence: 0,
            next_reset: cycle.next(now),
            cycle,
            last_time: now,
            total: Usage::default(),
            guest: Usage::default(),
            users_total: Usage::default(),
            users: BTreeMap::new(),
            days: BTreeMap::new(),
        }
    }
    fn advance(&mut self, now: i64, cycle: &ResetCycle) {
        let now = now.max(self.last_time);
        if now >= self.next_reset {
            self.total = Usage::default();
            self.guest = Usage::default();
            self.users_total = Usage::default();
            self.users.clear();
            self.next_reset = cycle.next(now);
        }
        // Editing the schedule changes the next boundary, never current usage.
        if self.cycle != *cycle {
            self.next_reset = cycle.next(now);
            self.cycle = cycle.clone();
        }
        self.last_time = now;
    }
    fn apply(&mut self, record: &Record) {
        self.advance(record.time, &record.cycle);
        self.total.add(record.direction, record.bytes);
        if record.subject == "guest" {
            self.guest.add(record.direction, record.bytes);
        } else if let Some(id) = record.subject.strip_prefix("user:") {
            self.users_total.add(record.direction, record.bytes);
            self.users
                .entry(id.to_owned())
                .or_default()
                .add(record.direction, record.bytes);
        }
        let date =
            DateTime::from_timestamp(record.time + i64::from(record.cycle.offset_minutes) * 60, 0)
                .expect("validated time");
        self.days
            .entry(date.format("%Y-%m-%d").to_string())
            .or_default()
            .add(record.direction, record.bytes);
        let cutoff = (date - chrono::Duration::days(62))
            .format("%Y-%m-%d")
            .to_string();
        self.days.retain(|day, _| day >= &cutoff);
        self.sequence = record.sequence;
    }
}
#[derive(Deserialize, Serialize)]
struct Record {
    sequence: u64,
    time: i64,
    subject: String,
    direction: Direction,
    bytes: u64,
    cycle: ResetCycle,
}
struct Runtime {
    ledger: Ledger,
    journal: std::fs::File,
    records: usize,
    failed: bool,
}
#[derive(Clone)]
pub struct TrafficStore {
    inner: Arc<Mutex<Runtime>>,
    snapshot: Arc<PathBuf>,
    config: SharedConfig,
}

fn failure(error: impl Into<anyhow::Error>) -> AppError {
    AppError::with_source(
        "Traffic accounting is unavailable; transfers are stopped",
        error,
    )
}
fn exhausted(direction: Direction) -> AppError {
    AppError::TrafficExhausted(match direction {
        Direction::Upload => "上传流量已用尽或剩余流量不足，请等待重置或联系管理员".into(),
        Direction::Download => "下载流量已用尽或剩余流量不足，请等待重置或联系管理员".into(),
    })
}
impl TrafficStore {
    pub async fn load(config_path: &Path, config: SharedConfig) -> AppResult<Self> {
        let snapshot = config_path.with_file_name("traffic-usage.json");
        let journal_path = config_path.with_file_name("traffic-usage.jsonl");
        let current = config.read().await;
        let cycle = current.traffic.cycle.clone();
        let active_users: std::collections::BTreeSet<String> = current
            .user_accounts
            .iter()
            .map(|user| user.id.clone())
            .chain(current.traffic.users.keys().cloned())
            .collect();
        drop(current);
        let snapshot_for_load = snapshot.clone();
        let runtime = tokio::task::spawn_blocking(move || -> anyhow::Result<Runtime> {
            let mut ledger = match std::fs::read(&snapshot_for_load) {
                Ok(bytes) => {
                    anyhow::ensure!(bytes.len() <= 4 * 1024 * 1024, "Traffic snapshot too large");
                    let ledger: Ledger = serde_json::from_slice(&bytes)?;
                    anyhow::ensure!(ledger.version == 1, "Unknown traffic ledger version");
                    TrafficSettings {
                        cycle: ledger.cycle.clone(),
                        ..Default::default()
                    }
                    .validate()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                    ledger
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    anyhow::ensure!(
                        !journal_path.exists(),
                        "Missing traffic snapshot; preserve journal for recovery"
                    );
                    Ledger::new(cycle)
                }
                Err(error) => return Err(error.into()),
            };
            let mut options = std::fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let journal = options.open(&journal_path)?;
            anyhow::ensure!(
                journal.metadata()?.len() <= 16 * 1024 * 1024,
                "Traffic journal requires recovery"
            );
            for line in std::io::BufReader::new(&journal).lines() {
                let record: Record = serde_json::from_str(&line?)?;
                if record.sequence <= ledger.sequence {
                    continue;
                }
                anyhow::ensure!(
                    record.sequence == ledger.sequence + 1,
                    "Traffic journal sequence gap"
                );
                TrafficSettings {
                    cycle: record.cycle.clone(),
                    ..Default::default()
                }
                .validate()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
                anyhow::ensure!(
                    (946_684_800..=7_258_118_400).contains(&record.time),
                    "Invalid traffic timestamp"
                );
                ledger.apply(&record);
            }
            ledger.users.retain(|id, _| active_users.contains(id));
            crate::config::publish_traffic_snapshot(
                &snapshot_for_load,
                &serde_json::to_vec(&ledger)?,
            )?;
            journal.set_len(0)?;
            journal.sync_all()?;
            Ok(Runtime {
                ledger,
                journal,
                records: 0,
                failed: false,
            })
        })
        .await
        .map_err(failure)?
        .map_err(failure)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(runtime)),
            snapshot: Arc::new(snapshot),
            config,
        })
    }
    fn check(
        ledger: &Ledger,
        settings: &TrafficSettings,
        subject: &str,
        direction: Direction,
        bytes: u64,
    ) -> AppResult<()> {
        // Administrator transfers remain metered but bypass byte allowances.
        if subject == "admin" {
            return Ok(());
        }
        let mut quotas = vec![(&settings.total, &ledger.total)];
        if subject == "guest" {
            quotas.push((&settings.guest, &ledger.guest));
        } else if let Some(id) = subject.strip_prefix("user:") {
            quotas.push((&settings.users_total, &ledger.users_total));
            if let Some(quota) = settings.users.get(id) {
                quotas.push((quota, ledger.users.get(id).unwrap_or(&ZERO_USAGE)));
            }
        }
        for (quota, usage) in quotas {
            let limit = quota.limit(direction);
            if limit > 0 && bytes > limit.saturating_sub(usage.amount(direction)) {
                return Err(exhausted(direction));
            }
        }
        Ok(())
    }
    pub async fn preflight(
        &self,
        subject: &str,
        direction: Direction,
        bytes: u64,
    ) -> AppResult<()> {
        let settings = self.config.read().await.traffic.clone();
        let runtime = self.inner.lock().await;
        if runtime.failed {
            return Err(AppError::ServiceUnavailable("流量记账暂不可用".into()));
        }
        let mut ledger = runtime.ledger.clone();
        ledger.advance(Utc::now().timestamp(), &settings.cycle);
        Self::check(&ledger, &settings, subject, direction, bytes)
    }
    async fn charge(&self, subject: String, direction: Direction, bytes: u64) -> AppResult<()> {
        if bytes == 0 {
            return Ok(());
        }
        let store = self.clone();
        // This owner survives cancellation through fsync and memory publication.
        tokio::spawn(async move {
            let mut runtime = store.inner.clone().lock_owned().await;
            let current = store.config.read().await;
            let settings = current.traffic.clone();
            let active_users: std::collections::BTreeSet<String> = current
                .user_accounts
                .iter()
                .map(|user| user.id.clone())
                .chain(settings.users.keys().cloned())
                .collect();
            drop(current);
            tokio::task::spawn_blocking(move || -> AppResult<()> {
                if runtime.failed {
                    return Err(AppError::ServiceUnavailable("流量记账暂不可用".into()));
                }
                runtime
                    .ledger
                    .advance(Utc::now().timestamp(), &settings.cycle);
                Self::check(&runtime.ledger, &settings, &subject, direction, bytes)?;
                let record = Record {
                    sequence: runtime.ledger.sequence + 1,
                    time: runtime.ledger.last_time,
                    subject,
                    direction,
                    bytes,
                    cycle: settings.cycle,
                };
                runtime.failed = true;
                let mut encoded = serde_json::to_vec(&record).map_err(failure)?;
                encoded.push(b'\n');
                runtime.journal.seek(SeekFrom::End(0)).map_err(failure)?;
                runtime.journal.write_all(&encoded).map_err(failure)?;
                runtime.journal.sync_data().map_err(failure)?;
                runtime.ledger.apply(&record);
                runtime
                    .ledger
                    .users
                    .retain(|id, _| active_users.contains(id));
                runtime.records += 1;
                if runtime.records >= 4096 {
                    crate::config::publish_traffic_snapshot(
                        &store.snapshot,
                        &serde_json::to_vec(&runtime.ledger).map_err(failure)?,
                    )
                    .map_err(failure)?;
                    runtime.journal.set_len(0).map_err(failure)?;
                    runtime.journal.sync_all().map_err(failure)?;
                    runtime.records = 0;
                }
                runtime.failed = false;
                Ok(())
            })
            .await
            .map_err(failure)?
        })
        .await
        .map_err(failure)?
    }
    pub fn wrap(&self, body: Body, subject: String, direction: Direction) -> Body {
        self.meter(body, subject, direction).0
    }
    pub fn meter(&self, body: Body, subject: String, direction: Direction) -> (Body, StreamStatus) {
        let store = self.clone();
        let status = StreamStatus::default();
        let failure = status.0.clone();
        // Charge each received frame before polling another one. Do not retain an
        // uncharged multi-frame buffer that can be discarded by cancellation.
        let stream = futures_util::stream::try_unfold(
            (body.into_data_stream(), store, subject, failure),
            move |(mut input, store, subject, failure)| async move {
                let Some(chunk) = input.next().await else {
                    return Ok::<_, std::io::Error>(None);
                };
                let chunk = chunk.map_err(std::io::Error::other)?;
                if let Err(error) = store
                    .charge(subject.clone(), direction, chunk.len() as u64)
                    .await
                {
                    *failure.lock().expect("stream status") = Some(error);
                    return Err(std::io::Error::other(
                        "Traffic accounting stopped this transfer",
                    ));
                }
                Ok(Some((chunk, (input, store, subject, failure))))
            },
        );
        (Body::from_stream(stream), status)
    }
    pub async fn download(&self, response: Response, subject: String) -> AppResult<Response> {
        if !response.status().is_success() {
            return Ok(response);
        }
        let size = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        self.preflight(&subject, Direction::Download, size).await?;
        let (parts, body) = response.into_parts();
        Ok(Response::from_parts(
            parts,
            self.wrap(body, subject, Direction::Download),
        ))
    }
}
#[derive(Default)]
pub struct StreamStatus(Arc<std::sync::Mutex<Option<AppError>>>);
impl StreamStatus {
    /// Backends may translate body errors; retain the actual quota error and
    /// the backend's commit/cleanup outcome instead of reporting client abort.
    pub fn finish<T>(&self, result: AppResult<T>) -> AppResult<T> {
        result.map_err(|backend_error| {
            let Some(error) = self.0.lock().expect("stream status").take() else {
                return backend_error;
            };
            match backend_error.operation() {
                Some(outcome) => error.with_operation(outcome.commit, outcome.cleanup),
                None => error,
            }
        })
    }
}
const ZERO_USAGE: Usage = Usage {
    upload: 0,
    download: 0,
};
pub async fn browser_subject(state: &AppState, headers: &HeaderMap) -> String {
    match crate::auth::current_principal(state, headers).await {
        Some(crate::auth::SessionPrincipal::User(id)) => format!("user:{id}"),
        Some(crate::auth::SessionPrincipal::Administrator) => "admin".into(),
        None => "guest".into(),
    }
}
#[derive(Deserialize)]
pub struct TrafficQuery {
    pub start: Option<String>,
    pub end: Option<String>,
}
#[derive(Serialize)]
pub struct TrafficView {
    pub settings: TrafficSettings,
    pub total: Usage,
    pub guest: Usage,
    pub users_total: Usage,
    pub users: BTreeMap<String, Usage>,
    pub next_reset: i64,
    pub days: BTreeMap<String, Usage>,
}
pub async fn info(
    State(state): State<AppState>,
    Query(query): Query<TrafficQuery>,
) -> AppResult<Json<TrafficView>> {
    let settings = state.config_file.read().await.traffic.clone();
    let runtime = state.traffic.inner.lock().await;
    let mut ledger = runtime.ledger.clone();
    ledger.advance(Utc::now().timestamp(), &settings.cycle);
    let today = DateTime::from_timestamp(
        Utc::now().timestamp() + i64::from(settings.cycle.offset_minutes) * 60,
        0,
    )
    .unwrap()
    .date_naive();
    let parse = |text: &str| {
        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map_err(|_| AppError::BadRequest("日期格式无效".into()))
    };
    let start = query
        .start
        .as_deref()
        .map(parse)
        .transpose()?
        .unwrap_or(today.with_day(1).unwrap());
    let end = query
        .end
        .as_deref()
        .map(parse)
        .transpose()?
        .unwrap_or(today);
    if start > end
        || end > today
        || (end - start).num_days() > 30
        || (today - start).num_days() > 62
    {
        return Err(AppError::BadRequest(
            "请选择保留期内、不超过 31 天的日期范围".into(),
        ));
    }
    let mut days = BTreeMap::new();
    let mut day = start;
    while day <= end {
        let key = day.format("%Y-%m-%d").to_string();
        days.insert(
            key.clone(),
            ledger.days.get(&key).cloned().unwrap_or_default(),
        );
        day = day.succ_opt().unwrap();
    }
    Ok(Json(TrafficView {
        settings,
        total: ledger.total,
        guest: ledger.guest,
        users_total: ledger.users_total,
        users: ledger.users,
        next_reset: ledger.next_reset,
        days,
    }))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettings {
    pub total: Quota,
    pub guest: Quota,
    #[serde(default)]
    pub users_total: Option<Quota>,
    pub cycle: ResetCycle,
}
pub async fn update(
    State(state): State<AppState>,
    Json(body): Json<UpdateSettings>,
) -> AppResult<Json<serde_json::Value>> {
    state
        .update_config(move |config| {
            config.traffic.total = body.total;
            config.traffic.guest = body.guest;
            if let Some(quota) = body.users_total {
                config.traffic.users_total = quota;
            }
            config.traffic.cycle = body.cycle;
            Ok(())
        })
        .await?;
    Ok(Json(serde_json::json!({"success": true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ConfigFile, test_support::TestDirectory};
    use tokio::sync::RwLock;

    fn quota(upload: u64, download: u64) -> Quota {
        Quota {
            enabled: true,
            upload,
            download,
        }
    }
    fn timestamp(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value).unwrap().timestamp()
    }
    async fn fixture(settings: TrafficSettings) -> (TestDirectory, TrafficStore, SharedConfig) {
        let directory = TestDirectory::new("traffic");
        let config = Arc::new(RwLock::new(ConfigFile {
            traffic: settings,
            ..Default::default()
        }));
        let store = TrafficStore::load(&directory.path().join("config.json"), config.clone())
            .await
            .unwrap();
        (directory, store, config)
    }

    #[test]
    fn validates_cycle_and_quota_without_overflow() {
        let mut settings = TrafficSettings::default();
        settings.cycle.offset_minutes = i32::MIN;
        assert!(settings.validate().is_err());
        settings.cycle.offset_minutes = 480;
        settings.cycle.every = 0;
        assert!(settings.validate().is_err());
        settings.cycle.every = 1;
        settings.total.upload = u64::MAX;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn monthly_boundary_clamps_without_drifting() {
        let cycle = ResetCycle {
            anchor: timestamp("2024-01-31T18:00:00+08:00"),
            offset_minutes: 480,
            ..Default::default()
        };
        let february = timestamp("2024-02-29T18:00:00+08:00");
        assert_eq!(cycle.next(cycle.anchor), february);
        assert_eq!(cycle.next(february), timestamp("2024-03-31T18:00:00+08:00"));
        assert_eq!(cycle.next(cycle.anchor - 1), cycle.anchor);
    }

    #[test]
    fn reset_and_schedule_edits_preserve_history() {
        let cycle = ResetCycle::default();
        let mut ledger = Ledger::new(cycle.clone());
        let now = ledger.last_time;
        ledger.apply(&Record {
            sequence: 1,
            time: now,
            subject: "guest".into(),
            direction: Direction::Download,
            bytes: 20,
            cycle: cycle.clone(),
        });
        let changed = ResetCycle {
            unit: CycleUnit::Hours,
            every: 2,
            ..cycle
        };
        ledger.advance(now, &changed);
        assert_eq!(ledger.total.download, 20);
        ledger.advance(now - 86400, &changed);
        assert_eq!(ledger.total.download, 20);
        ledger.advance(ledger.next_reset, &changed);
        assert_eq!(ledger.total.download, 0);
        assert_eq!(ledger.guest.download, 0);
        assert_eq!(ledger.days.values().map(|d| d.download).sum::<u64>(), 20);
    }

    #[tokio::test]
    async fn concurrent_guests_share_a_hard_cap() {
        let (_directory, store, _) = fixture(TrafficSettings {
            guest: quota(0, 100),
            ..Default::default()
        })
        .await;
        let (a, b) = tokio::join!(
            store.charge("guest".into(), Direction::Download, 60),
            store.charge("guest".into(), Direction::Download, 60)
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let ledger = &store.inner.lock().await.ledger;
        assert_eq!(ledger.total.download, 60);
        assert_eq!(ledger.guest.download, 60);
    }

    #[tokio::test]
    async fn users_are_independent_and_admin_webdav_count_only_once() {
        let settings = TrafficSettings {
            total: quota(100, 100),
            users: BTreeMap::from([("a".into(), quota(10, 20)), ("b".into(), quota(10, 20))]),
            ..Default::default()
        };
        let (_directory, store, _) = fixture(settings).await;
        store
            .charge("user:a".into(), Direction::Download, 20)
            .await
            .unwrap();
        assert!(store
            .charge("user:a".into(), Direction::Download, 1)
            .await
            .is_err());
        store
            .charge("user:b".into(), Direction::Download, 20)
            .await
            .unwrap();
        store
            .charge("admin".into(), Direction::Download, 30)
            .await
            .unwrap();
        store
            .charge("webdav".into(), Direction::Download, 30)
            .await
            .unwrap();
        assert!(store
            .charge("guest".into(), Direction::Download, 1)
            .await
            .is_err());
        store
            .charge("user:a".into(), Direction::Upload, 10)
            .await
            .unwrap();
        let ledger = &store.inner.lock().await.ledger;
        assert_eq!(ledger.total.download, 100);
        assert_eq!(ledger.total.upload, 10);
        assert_eq!(ledger.guest.download, 0);
    }

    #[tokio::test]
    async fn ordinary_users_share_an_additional_total_allowance() {
        let settings = TrafficSettings {
            users_total: quota(0, 30),
            users: BTreeMap::from([("a".into(), quota(0, 100)), ("b".into(), quota(0, 100))]),
            ..Default::default()
        };
        let (_directory, store, _) = fixture(settings).await;
        store
            .charge("user:a".into(), Direction::Download, 20)
            .await
            .unwrap();
        store
            .charge("user:b".into(), Direction::Download, 10)
            .await
            .unwrap();
        assert!(store
            .charge("user:a".into(), Direction::Download, 1)
            .await
            .is_err());
        store
            .charge("guest".into(), Direction::Download, 5)
            .await
            .unwrap();
        let ledger = &store.inner.lock().await.ledger;
        assert_eq!(ledger.users_total.download, 30);
        assert_eq!(ledger.total.download, 35);
    }

    #[tokio::test]
    async fn disabled_limits_still_count_and_restart_replays_once() {
        let (directory, store, config) = fixture(TrafficSettings::default()).await;
        store
            .charge("guest".into(), Direction::Upload, 17)
            .await
            .unwrap();
        drop(store);
        let path = directory.path().join("config.json");
        let store = TrafficStore::load(&path, config.clone()).await.unwrap();
        assert_eq!(store.inner.lock().await.ledger.total.upload, 17);
        drop(store);
        let store = TrafficStore::load(&path, config.clone()).await.unwrap();
        assert_eq!(store.inner.lock().await.ledger.total.upload, 17);
        config.write().await.traffic.total = quota(17, 0);
        assert!(store
            .preflight("guest", Direction::Upload, 1)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn body_failure_keeps_quota_and_cleanup_semantics() {
        let (_directory, store, _) = fixture(TrafficSettings {
            total: quota(3, 0),
            ..Default::default()
        })
        .await;
        let (body, status) = store.meter(Body::from("four"), "webdav".into(), Direction::Upload);
        assert!(axum::body::to_bytes(body, 100).await.is_err());
        let error = status
            .finish::<()>(Err(AppError::ClientClosedRequest.with_operation(
                crate::error::CommitState::NotCommitted,
                crate::error::CleanupState::Pending,
            )))
            .unwrap_err();
        assert_eq!(error.code(), "traffic_exhausted");
        assert_eq!(
            error.operation().unwrap().cleanup,
            crate::error::CleanupState::Pending
        );
        assert_eq!(store.inner.lock().await.ledger.total.upload, 0);
    }

    #[tokio::test]
    async fn compaction_and_deleted_accounts_keep_totals() {
        let settings = TrafficSettings {
            users: BTreeMap::from([("a".into(), quota(0, 0))]),
            ..Default::default()
        };
        let (directory, store, config) = fixture(settings).await;
        store
            .charge("user:a".into(), Direction::Download, 7)
            .await
            .unwrap();
        config.write().await.traffic.users.clear();
        store.inner.lock().await.records = 4095;
        store
            .charge("admin".into(), Direction::Download, 3)
            .await
            .unwrap();
        assert!(store.inner.lock().await.ledger.users.is_empty());
        drop(store);
        let store = TrafficStore::load(&directory.path().join("config.json"), config)
            .await
            .unwrap();
        let ledger = &store.inner.lock().await.ledger;
        assert_eq!(ledger.total.download, 10);
        assert_eq!(ledger.users_total.download, 7);
    }

    #[tokio::test]
    async fn administrator_bypasses_allowances_but_remains_metered() {
        let (_directory, store, _) = fixture(TrafficSettings {
            total: quota(1, 1),
            ..Default::default()
        })
        .await;
        store
            .charge("admin".into(), Direction::Upload, 20)
            .await
            .unwrap();
        store
            .charge("admin".into(), Direction::Download, 30)
            .await
            .unwrap();
        assert!(store
            .preflight("admin", Direction::Download, 999)
            .await
            .is_ok());
        assert!(store
            .preflight("guest", Direction::Download, 1)
            .await
            .is_err());
        assert!(store
            .preflight("webdav", Direction::Upload, 1)
            .await
            .is_err());
        let ledger = &store.inner.lock().await.ledger;
        assert_eq!(ledger.total.upload, 20);
        assert_eq!(ledger.total.download, 30);
        assert_eq!(ledger.guest.download, 0);
    }

    #[tokio::test]
    async fn dropping_a_download_before_body_poll_does_not_charge() {
        let (_directory, store, _) = fixture(TrafficSettings::default()).await;
        let response = Response::builder()
            .header("content-length", "3")
            .body(Body::from("abc"))
            .unwrap();
        drop(store.download(response, "guest".into()).await.unwrap());
        assert_eq!(store.inner.lock().await.ledger.total.download, 0);
    }

    #[tokio::test]
    async fn first_small_frame_is_charged_without_waiting_for_eof() {
        let (_directory, store, _) = fixture(TrafficSettings::default()).await;
        let source = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"abc"))
        })
        .chain(futures_util::stream::pending());
        let mut output = store
            .wrap(Body::from_stream(source), "guest".into(), Direction::Upload)
            .into_data_stream();
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), output.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&chunk[..], b"abc");
        drop(output);
        assert_eq!(store.inner.lock().await.ledger.total.upload, 3);
    }
}
