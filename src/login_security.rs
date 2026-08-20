use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;

const SECURITY_FILE: &str = "security-events.json";
const MAX_RECORDS: usize = 500;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginEntry {
    Admin,
    Web,
    WebDav,
}

impl LoginEntry {
    pub fn policy(self) -> (u32, i64) {
        match self {
            Self::Admin => (3, 60 * 60),
            Self::Web => (5, 60 * 60),
            Self::WebDav => (5, 60),
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

#[derive(Clone, Default, Deserialize, Serialize)]
struct SecurityData {
    #[serde(default = "schema_version")]
    schema_version: u32,
    #[serde(default)]
    records: Vec<LoginRecord>,
}

fn schema_version() -> u32 {
    1
}

#[derive(Clone)]
pub struct LoginSecurity {
    path: PathBuf,
    data: Arc<Mutex<SecurityData>>,
}

impl LoginSecurity {
    pub async fn load(config_path: &Path) -> anyhow::Result<Self> {
        let path = config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join(SECURITY_FILE);
        let backup = backup_path(&path);
        let data = if tokio::fs::try_exists(&path).await? {
            read_data(&path).await.or_else(|primary| {
                std::fs::read(&backup)
                    .with_context(|| {
                        format!(
                    "Login security database is invalid and backup recovery failed: {primary}"
                )
                    })
                    .and_then(|bytes| {
                        serde_json::from_slice(&bytes).context("Login security backup is invalid")
                    })
            })?
        } else if tokio::fs::try_exists(&backup).await? {
            read_data(&backup).await?
        } else {
            SecurityData {
                schema_version: 1,
                records: Vec::new(),
            }
        };
        if data.schema_version != 1 || data.records.len() > MAX_RECORDS {
            anyhow::bail!("Login security database has an unsupported format");
        }
        if tokio::fs::try_exists(&path).await? {
            secure_permissions(&path).await?;
        }
        if tokio::fs::try_exists(&backup).await? {
            secure_permissions(&backup).await?;
        }
        Ok(Self {
            path,
            data: Arc::new(Mutex::new(data)),
        })
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
            persist(&self.path, &data).await?;
        }
        Ok(false)
    }

    pub async fn record_failure(
        &self,
        entry: LoginEntry,
        ip: IpAddr,
        user_agent: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let (maximum, lock_seconds) = entry.policy();
        let mut data = self.data.lock().await;
        ensure_capacity(&mut data.records, entry, ip);
        let record = get_or_insert(&mut data.records, entry, ip, now);
        record.failed_attempts = record.failed_attempts.saturating_add(1);
        record.last_attempt_at = now;
        record.user_agent = sanitize_user_agent(user_agent);
        if record.failed_attempts >= maximum {
            record.blocked_until = Some(now + lock_seconds);
            record.last_result = "凭据错误，已限制".into();
        } else {
            record.last_result = "凭据错误".into();
        }
        persist(&self.path, &data).await
    }

    pub async fn record_success(
        &self,
        entry: LoginEntry,
        ip: IpAddr,
        user_agent: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut data = self.data.lock().await;
        // WebDAV may authenticate every file request. Avoid a disk write for every
        // successful operation unless this IP has a failure state to clear.
        if entry == LoginEntry::WebDav {
            let needs_reset = find_mut(&mut data.records, entry, ip)
                .is_some_and(|record| record.failed_attempts > 0 || record.blocked_until.is_some());
            if !needs_reset {
                return Ok(());
            }
        }
        ensure_capacity(&mut data.records, entry, ip);
        let record = get_or_insert(&mut data.records, entry, ip, now);
        record.failed_attempts = 0;
        record.blocked_until = None;
        record.last_attempt_at = now;
        record.last_success_at = Some(now);
        record.last_result = "登录成功".into();
        record.user_agent = sanitize_user_agent(user_agent);
        persist(&self.path, &data).await
    }

    pub async fn snapshot(&self) -> Vec<LoginRecord> {
        let now = chrono::Utc::now().timestamp();
        let mut records = self.data.lock().await.records.clone();
        for record in &mut records {
            if record.blocked_until.is_some_and(|until| until <= now) {
                record.blocked_until = None;
            }
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.last_attempt_at));
        records
    }

    pub async fn unblock(&self, entry: LoginEntry, ip: IpAddr) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let mut data = self.data.lock().await;
        let Some(record) = find_mut(&mut data.records, entry, ip) else {
            return Ok(false);
        };
        record.failed_attempts = 0;
        record.blocked_until = None;
        record.last_attempt_at = now;
        record.last_result = "管理员已解除限制".into();
        persist(&self.path, &data).await?;
        Ok(true)
    }

    pub async fn restrict(&self, entry: LoginEntry, ip: IpAddr) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let (_, lock_seconds) = entry.policy();
        let mut data = self.data.lock().await;
        ensure_capacity(&mut data.records, entry, ip);
        let record = get_or_insert(&mut data.records, entry, ip, now);
        record.blocked_until = Some(now + lock_seconds);
        record.last_attempt_at = now;
        record.last_result = "管理员已限制".into();
        persist(&self.path, &data).await
    }
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
        last_result: "".into(),
        user_agent: None,
    });
    records.last_mut().expect("record was inserted")
}

fn ensure_capacity(records: &mut Vec<LoginRecord>, entry: LoginEntry, ip: IpAddr) {
    let ip = ip.to_string();
    if records.len() >= MAX_RECORDS && !records.iter().any(|r| r.entry == entry && r.ip == ip) {
        if let Some((index, _)) = records
            .iter()
            .enumerate()
            .min_by_key(|(_, record)| record.last_attempt_at)
        {
            records.remove(index);
        }
    }
}

fn sanitize_user_agent(value: Option<&str>) -> Option<String> {
    value
        .map(|text| {
            text.chars()
                .filter(|ch| !ch.is_control())
                .take(256)
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
}

async fn read_data(path: &Path) -> anyhow::Result<SecurityData> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("Login security database is invalid")
}

async fn persist(path: &Path, data: &SecurityData) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(data)?;
    let temporary = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .await
        .context("Failed to create login security temporary file")?;
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
        return Err(error).context("Failed to publish login security database");
    }
    secure_permissions(path).await?;
    Ok(())
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
    async fn limits_are_persistent_and_can_be_unblocked() {
        let root = std::env::temp_dir().join(format!("ycloud-login-security-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let config = root.join("config.json");
        let tracker = LoginSecurity::load(&config).await.unwrap();
        let ip: IpAddr = "2001:db8::1234".parse().unwrap();
        for _ in 0..3 {
            tracker
                .record_failure(LoginEntry::Admin, ip, Some("test-agent"))
                .await
                .unwrap();
        }
        assert!(tracker.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        let reloaded = LoginSecurity::load(&config).await.unwrap();
        assert!(reloaded.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        assert!(reloaded.unblock(LoginEntry::Admin, ip).await.unwrap());
        assert!(!reloaded.is_blocked(LoginEntry::Admin, ip).await.unwrap());
        reloaded
            .record_failure(LoginEntry::Web, ip, None)
            .await
            .unwrap();
        reloaded
            .record_success(LoginEntry::Web, ip, None)
            .await
            .unwrap();
        let record = reloaded
            .snapshot()
            .await
            .into_iter()
            .find(|record| record.entry == LoginEntry::Web)
            .unwrap();
        assert_eq!(record.failed_attempts, 0);
        assert!(record.last_success_at.is_some());
        reloaded.restrict(LoginEntry::Web, ip).await.unwrap();
        assert!(reloaded.is_blocked(LoginEntry::Web, ip).await.unwrap());
        let reloaded = LoginSecurity::load(&config).await.unwrap();
        let restricted = reloaded
            .snapshot()
            .await
            .into_iter()
            .find(|record| record.entry == LoginEntry::Web)
            .unwrap();
        assert_eq!(restricted.last_result, "管理员已限制");
        assert!(restricted.blocked_until.is_some());
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
