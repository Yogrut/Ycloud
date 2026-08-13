use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

// ── Data types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    #[serde(default = "uuid_v4")]
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub webdav_enabled: bool,
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderLock {
    #[serde(default = "uuid_v4")]
    pub id: String,
    pub path: String,
    pub password_hash: String,
}

fn uuid_v4() -> String {
    Uuid::new_v4().to_string()
}

impl FolderLock {
    pub fn matches(&self, request_path: &str) -> bool {
        path_is_same_or_descendant(request_path, &self.path)
    }
}

/// Returns true when `path` is the same logical storage path as `ancestor`,
/// or is located below it.
pub fn path_is_same_or_descendant(path: &str, ancestor: &str) -> bool {
    let path_components: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    let ancestor_components: Vec<&str> = ancestor
        .trim_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();

    !ancestor_components.is_empty()
        && path_components.len() >= ancestor_components.len()
        && path_components[..ancestor_components.len()] == ancestor_components
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub schema_version: u32,
    pub admin_username: String,
    pub admin_password_hash: String,
    #[serde(default)]
    pub global_web_password_hash: Option<String>,
    #[serde(default)]
    pub folder_locks: Vec<FolderLock>,
    #[serde(default)]
    pub shares: Vec<Share>,
}

#[derive(Clone)]
pub struct Config {
    pub bind_address: IpAddr,
    pub port: u16,
    pub storage_path: PathBuf,
    pub config_path: PathBuf,
    pub max_upload_bytes: u64,
    pub io_concurrency: usize,
    pub max_list_entries: usize,
    pub request_timeout_secs: u64,
    pub upload_timeout_secs: u64,
    pub disk_reserve_bytes: u64,
    pub secure_cookies: bool,
    pub public_base_url: Option<String>,
    pub public_host: Option<String>,
    pub trusted_proxy_ips: HashSet<IpAddr>,
}

pub type SharedConfig = Arc<RwLock<ConfigFile>>;

// ── Serde defaults ────────────────────────────────────────────────

fn default_admin_username() -> String {
    "admin".into()
}

// ── ConfigFile ────────────────────────────────────────────────────

impl Default for ConfigFile {
    fn default() -> Self {
        let first = Uuid::new_v4().simple().to_string();
        let second = Uuid::new_v4().simple().to_string();
        let default_hash = hash_password(&format!("{}{}", &first[..12], &second[..12]));
        Self {
            schema_version: 2,
            admin_username: default_admin_username(),
            admin_password_hash: default_hash.clone(),
            global_web_password_hash: Some(default_hash),
            folder_locks: Vec::new(),
            shares: vec![Share {
                id: uuid_v4(),
                name: "Default".into(),
                path: String::new(),
                username: Some("admin".into()),
                webdav_enabled: false,
                password_hash: None,
                readonly: false,
            }],
        }
    }
}

impl ConfigFile {
    pub fn validate(&self) -> AppResult<()> {
        if self.schema_version != 2 {
            return Err(AppError::BadRequest(
                "Unsupported configuration schema version".into(),
            ));
        }
        let username = self.admin_username.trim();
        if username.is_empty() || username.len() > 128 {
            return Err(AppError::BadRequest(
                "Administrator username must contain 1-128 characters".into(),
            ));
        }
        if PasswordHash::new(&self.admin_password_hash).is_err() {
            return Err(AppError::BadRequest(
                "Administrator password hash is invalid".into(),
            ));
        }
        if self.shares.len() > 1_000 || self.folder_locks.len() > 10_000 {
            return Err(AppError::BadRequest(
                "Configuration exceeds supported limits".into(),
            ));
        }

        let mut share_ids = HashSet::new();
        let mut share_names = HashSet::new();
        for share in &self.shares {
            if share.id.is_empty()
                || share.id.len() > 128
                || !share
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || !share_ids.insert(share.id.as_str())
            {
                return Err(AppError::Conflict("Share IDs must be unique".into()));
            }
            let name = share.name.trim();
            if name.is_empty()
                || name.len() > 128
                || name.contains('/')
                || name.contains('\\')
                || name.contains('\0')
            {
                return Err(AppError::BadRequest("Share name is invalid".into()));
            }
            if !share_names.insert(name.to_lowercase()) {
                return Err(AppError::Conflict("Share names must be unique".into()));
            }
            validate_relative_config_path(&share.path)?;
            if let Some(username) = share.username.as_deref() {
                if username.trim().is_empty() || username.len() > 128 {
                    return Err(AppError::BadRequest(
                        "WebDAV username must contain 1-128 characters".into(),
                    ));
                }
            }
            if share
                .password_hash
                .as_deref()
                .is_some_and(|hash| PasswordHash::new(hash).is_err())
            {
                return Err(AppError::BadRequest(
                    "WebDAV password hash is invalid".into(),
                ));
            }
            if share.webdav_enabled && (share.username.is_none() || share.password_hash.is_none()) {
                return Err(AppError::BadRequest(
                    "Enabled WebDAV shares require a username and password".into(),
                ));
            }
        }

        let mut lock_ids = HashSet::new();
        for lock in &self.folder_locks {
            if lock.id.is_empty()
                || lock.id.len() > 128
                || !lock
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || !lock_ids.insert(lock.id.as_str())
            {
                return Err(AppError::Conflict("Folder lock IDs must be unique".into()));
            }
            if lock.path.trim_matches('/').is_empty() {
                return Err(AppError::BadRequest(
                    "Folder lock path cannot be the storage root".into(),
                ));
            }
            validate_relative_config_path(&lock.path)?;
            if PasswordHash::new(&lock.password_hash).is_err() {
                return Err(AppError::BadRequest(
                    "Folder lock password hash is invalid".into(),
                ));
            }
        }
        Ok(())
    }
}

fn validate_relative_config_path(path: &str) -> AppResult<()> {
    if path.contains('\\')
        || path.contains('\0')
        || path.trim_matches('/').split('/').any(|component| {
            component == ".."
                || component.contains(':')
                || component.eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR)
        })
    {
        return Err(AppError::BadRequest(
            "Configured storage path is invalid".into(),
        ));
    }
    Ok(())
}

// ── Config ────────────────────────────────────────────────────────

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind_address = env_parse("BIND_ADDRESS", IpAddr::from([127, 0, 0, 1]))?;
        let port = env_parse("PORT", 3000_u16)?;
        let storage_path =
            PathBuf::from(std::env::var("STORAGE_PATH").unwrap_or_else(|_| "./storage".into()));
        let config_path =
            PathBuf::from(std::env::var("CONFIG_PATH").unwrap_or_else(|_| "./config.json".into()));
        let max_upload_bytes = env_parse("MAX_UPLOAD_BYTES", 5_u64 * 1024 * 1024 * 1024)?;
        let io_concurrency = env_parse("IO_CONCURRENCY", 4_usize)?;
        let max_list_entries = env_parse("MAX_LIST_ENTRIES", 10_000_usize)?;
        let request_timeout_secs = env_parse("REQUEST_TIMEOUT_SECS", 300_u64)?;
        let upload_timeout_secs = env_parse("UPLOAD_TIMEOUT_SECS", 6_u64 * 60 * 60)?;
        let disk_reserve_bytes = env_parse("DISK_RESERVE_BYTES", 512_u64 * 1024 * 1024)?;
        let secure_cookies = env_parse("SECURE_COOKIES", false)?;
        let (public_base_url, public_host, trusted_proxy_ips) =
            public_proxy_config(bind_address, secure_cookies)?;
        Ok(Self {
            bind_address,
            port,
            storage_path,
            config_path,
            max_upload_bytes,
            io_concurrency,
            max_list_entries,
            request_timeout_secs,
            upload_timeout_secs,
            disk_reserve_bytes,
            secure_cookies,
            public_base_url,
            public_host,
            trusted_proxy_ips,
        })
    }

    pub fn is_public_mode(&self) -> bool {
        self.public_base_url.is_some()
    }
}

fn public_proxy_config(
    bind_address: IpAddr,
    secure_cookies: bool,
) -> anyhow::Result<(Option<String>, Option<String>, HashSet<IpAddr>)> {
    if bind_address.is_loopback() {
        return Ok((None, None, HashSet::new()));
    }
    if !secure_cookies {
        anyhow::bail!("SECURE_COOKIES=true is required when BIND_ADDRESS is not loopback");
    }
    let raw_url = std::env::var("PUBLIC_BASE_URL")
        .context("PUBLIC_BASE_URL=https://your-domain is required for public mode")?;
    let uri: axum::http::Uri = raw_url
        .parse()
        .context("PUBLIC_BASE_URL must be a valid HTTPS origin")?;
    if uri.scheme_str() != Some("https")
        || uri.authority().is_none()
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
    {
        anyhow::bail!("PUBLIC_BASE_URL must be an HTTPS origin without a path or query");
    }
    let host = uri
        .authority()
        .expect("authority checked above")
        .as_str()
        .to_string();
    let origin = format!("https://{host}");
    let raw_proxies = std::env::var("TRUSTED_PROXY_IPS")
        .context("TRUSTED_PROXY_IPS is required for public mode")?;
    let trusted_proxy_ips: HashSet<IpAddr> = raw_proxies
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<IpAddr>()
                .with_context(|| format!("Invalid trusted proxy IP: {value}"))
        })
        .collect::<anyhow::Result<_>>()?;
    if trusted_proxy_ips.is_empty() {
        anyhow::bail!("TRUSTED_PROXY_IPS must contain at least one exact IP address");
    }
    Ok((Some(origin), Some(host), trusted_proxy_ips))
}

fn env_parse<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr + ToString,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("Invalid {name}"))
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .unwrap_or_else(|e| {
            tracing::error!("Argon2 hash failed: {}", e);
            // Return a clearly invalid hash; verification will fail and caller can retry
            format!("__hash_error__{}", Uuid::new_v4())
        })
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .ok()
        .and_then(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .ok()
        })
        .is_some()
}

pub async fn load_config(path: &Path) -> anyhow::Result<ConfigFile> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create configuration directory")?;
        secure_directory_permissions(parent).await?;
    }
    let backup_path = config_backup_path(path);
    if tokio::fs::try_exists(path).await? || tokio::fs::try_exists(&backup_path).await? {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(content) if serde_json::from_str::<serde_json::Value>(&content).is_ok() => content,
            Ok(_) | Err(_) => {
                let backup = tokio::fs::read_to_string(&backup_path)
                    .await
                    .context("Primary config is unavailable and backup recovery failed")?;
                serde_json::from_str::<serde_json::Value>(&backup)
                    .context("Configuration backup is invalid")?;
                tracing::warn!(
                    backup = %backup_path.display(),
                    "recovering configuration from last known good backup"
                );
                tokio::fs::copy(&backup_path, path)
                    .await
                    .context("Failed to restore configuration backup")?;
                secure_file_permissions(path).await?;
                backup
            }
        };

        let mut raw: serde_json::Value =
            serde_json::from_str(&content).context("Failed to parse config.json")?;
        let mut migrated = false;
        let schema_version = raw
            .get("schema_version")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        if schema_version > 2 {
            anyhow::bail!(
                "Configuration schema version {schema_version} is newer than this Ycloud build"
            );
        }
        if schema_version < 2 {
            raw["schema_version"] = serde_json::json!(2);
            migrated = true;
        }
        let config: ConfigFile =
            serde_json::from_value(raw).context("Failed to parse config.json")?;
        config
            .validate()
            .map_err(anyhow::Error::new)
            .context("Invalid config.json")?;
        if migrated {
            save_config(path, &config)
                .await
                .context("Failed to persist migrated configuration")?;
        }
        Ok(config)
    } else {
        let password = std::env::var("INITIAL_ADMIN_PASSWORD")
            .context("INITIAL_ADMIN_PASSWORD is required when creating a new configuration")?;
        if password.len() < 12 || password.len() > 1_024 {
            anyhow::bail!("INITIAL_ADMIN_PASSWORD must contain 12-1024 bytes");
        }
        let password_hash = hash_password(&password);
        let config = ConfigFile {
            schema_version: 2,
            admin_username: default_admin_username(),
            admin_password_hash: password_hash,
            global_web_password_hash: None,
            folder_locks: Vec::new(),
            shares: vec![Share {
                id: uuid_v4(),
                name: "Default".into(),
                path: String::new(),
                username: Some("admin".into()),
                webdav_enabled: false,
                password_hash: None,
                readonly: false,
            }],
        };
        save_config(path, &config).await?;
        Ok(config)
    }
}

pub async fn save_config(path: &Path, config: &ConfigFile) -> anyhow::Result<()> {
    config
        .validate()
        .map_err(anyhow::Error::new)
        .context("Refusing to save invalid configuration")?;
    let json = serde_json::to_string_pretty(config)?;
    let temporary = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let backup = config_backup_path(path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .await
        .context("Failed to create temporary configuration")?;
    let prepare_result = async {
        file.write_all(json.as_bytes())
            .await
            .context("Failed to write temporary configuration")?;
        file.sync_all()
            .await
            .context("Failed to flush temporary configuration")
    }
    .await;
    drop(file);
    if let Err(error) = prepare_result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }

    let commit_result: anyhow::Result<()> = async {
        if tokio::fs::try_exists(path).await? {
            if tokio::fs::try_exists(&backup).await? {
                tokio::fs::remove_file(&backup).await?;
            }
            tokio::fs::rename(path, &backup)
                .await
                .context("Failed to rotate configuration backup")?;
            secure_file_permissions(&backup).await?;
        }
        tokio::fs::rename(&temporary, path)
            .await
            .context("Failed to commit configuration")?;
        secure_file_permissions(path).await
    }
    .await;
    if let Err(error) = commit_result {
        if !tokio::fs::try_exists(path).await.unwrap_or(false)
            && tokio::fs::try_exists(&backup).await.unwrap_or(false)
        {
            let _ = tokio::fs::rename(&backup, path).await;
        }
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
async fn secure_directory_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .context("Failed to restrict configuration directory permissions")
}

#[cfg(not(unix))]
async fn secure_directory_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn secure_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .context("Failed to restrict configuration file permissions")
}

#[cfg(not(unix))]
async fn secure_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn config_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

trait SecureOpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(unix)]
impl SecureOpenOptions for OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptionsExt::mode(self, mode)
    }
}

#[cfg(not(unix))]
impl SecureOpenOptions for OpenOptions {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        config_backup_path, hash_password, load_config, path_is_same_or_descendant, save_config,
        ConfigFile, FolderLock,
    };

    #[test]
    fn path_relationship_is_component_aware() {
        assert!(path_is_same_or_descendant("photos/private", "photos"));
        assert!(path_is_same_or_descendant(
            "/photos/private/2026/",
            "/photos/private"
        ));
        assert!(!path_is_same_or_descendant("photos-old", "photos"));
        assert!(!path_is_same_or_descendant("photos", ""));
    }

    #[test]
    fn folder_lock_protects_descendants() {
        let lock = FolderLock {
            id: "lock-id".into(),
            path: "photos/private".into(),
            password_hash: "unused".into(),
        };

        assert!(lock.matches("photos/private"));
        assert!(lock.matches("photos/private/child/file.txt"));
        assert!(!lock.matches("photos/public"));
        assert!(!lock.matches("photos/private-old"));
    }

    #[test]
    fn enabled_webdav_requires_credentials() {
        let mut config = ConfigFile::default();
        config.shares[0].webdav_enabled = true;
        assert!(config.validate().is_err());
        config.shares[0].password_hash = Some(hash_password("test-password"));
        assert!(config.validate().is_ok());
    }

    #[tokio::test]
    async fn config_save_keeps_last_known_good_backup() {
        let directory =
            std::env::temp_dir().join(format!("ycloud-config-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");
        let mut config = ConfigFile {
            schema_version: 2,
            admin_username: "first-admin".into(),
            admin_password_hash: hash_password("test-password"),
            global_web_password_hash: None,
            folder_locks: Vec::new(),
            shares: Vec::new(),
        };
        save_config(&path, &config).await.unwrap();
        config.admin_username = "second-admin".into();
        save_config(&path, &config).await.unwrap();

        let active: ConfigFile =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        let backup: ConfigFile =
            serde_json::from_slice(&tokio::fs::read(config_backup_path(&path)).await.unwrap())
                .unwrap();
        assert_eq!(active.admin_username, "second-admin");
        assert_eq!(backup.admin_username, "first-admin");
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn stable_config_migrates_to_schema_v2() {
        let directory =
            std::env::temp_dir().join(format!("ycloud-migration-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");
        let stable = serde_json::json!({
            "admin_username": "admin",
            "admin_password_hash": hash_password("test-password"),
            "shares": [{
                "name": "Legacy",
                "path": "",
                "webdav_enabled": false,
                "readonly": false
            }]
        });
        tokio::fs::write(&path, serde_json::to_vec(&stable).unwrap())
            .await
            .unwrap();

        let migrated = load_config(&path).await.unwrap();
        assert_eq!(migrated.shares.len(), 1);
        assert!(!migrated.shares[0].id.is_empty());

        let persisted: ConfigFile =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(persisted.shares[0].id, migrated.shares[0].id);
        assert_eq!(persisted.schema_version, 2);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}

// ── Password helpers ──────────────────────────────────────────────
