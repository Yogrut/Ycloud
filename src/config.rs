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
    #[serde(default = "default_admin_username")]
    pub admin_username: String,
    #[serde(default = "default_admin_hash")]
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
}

pub type SharedConfig = Arc<RwLock<ConfigFile>>;

// ── Serde defaults ────────────────────────────────────────────────

fn default_admin_username() -> String {
    "admin".into()
}

fn default_admin_hash() -> String {
    let password = initial_admin_password();
    tracing::warn!(
        username = "admin",
        password = %password,
        "generated a replacement administrator password"
    );
    hash_password(&password)
}

// ── ConfigFile ────────────────────────────────────────────────────

impl Default for ConfigFile {
    fn default() -> Self {
        let initial_password = initial_admin_password();
        tracing::warn!(
            username = "admin",
            password = %initial_password,
            "generated initial credentials; change them after first login"
        );
        let default_hash = hash_password(&initial_password);
        Self {
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

fn initial_admin_password() -> String {
    std::env::var("INITIAL_ADMIN_PASSWORD").unwrap_or_else(|_| {
        let first = Uuid::new_v4().simple().to_string();
        let second = Uuid::new_v4().simple().to_string();
        format!("{}{}", &first[..12], &second[..12])
    })
}

impl ConfigFile {
    pub fn validate(&self) -> AppResult<()> {
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
        || path
            .trim_matches('/')
            .split('/')
            .any(|component| component == ".." || component.contains(':'))
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
        let bind_address = env_parse("BIND_ADDRESS", IpAddr::from([0, 0, 0, 0]))?;
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
        })
    }
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
                backup
            }
        };

        // Migration: old HashMap format → new Vec<FolderLock>
        let mut raw: serde_json::Value =
            serde_json::from_str(&content).context("Failed to parse config.json")?;
        let mut migrated = false;
        if let Some(locks) = raw.get_mut("folder_locks") {
            if locks.is_object() {
                let map: std::collections::HashMap<String, String> =
                    serde_json::from_value(locks.clone()).unwrap_or_default();
                let mut new_locks: Vec<serde_json::Value> = Vec::new();
                for (p, h) in map {
                    new_locks.push(serde_json::json!({
                        "id": Uuid::new_v4().to_string(),
                        "path": p,
                        "password_hash": h
                    }));
                }
                *locks = serde_json::json!(new_locks);
                migrated = true;
            }
        }

        if let Some(shares) = raw.get_mut("shares").and_then(|value| value.as_array_mut()) {
            for share in shares {
                if let Some(object) = share.as_object_mut() {
                    if let Some(legacy_enabled) = object.remove("enabled") {
                        migrated = true;
                        if legacy_enabled.as_bool() == Some(false) {
                            object.insert("webdav_enabled".into(), serde_json::json!(false));
                        }
                    }
                    if object.remove("web_password_hash").is_some() {
                        migrated = true;
                    }
                    let webdav_enabled = object
                        .get("webdav_enabled")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false);
                    let has_username = object
                        .get("username")
                        .and_then(|value| value.as_str())
                        .is_some_and(|value| !value.trim().is_empty());
                    let has_password = object
                        .get("password_hash")
                        .is_some_and(|value| value.is_string());
                    if webdav_enabled && (!has_username || !has_password) {
                        object.insert("webdav_enabled".into(), serde_json::json!(false));
                        migrated = true;
                        tracing::warn!(
                            "disabled a legacy WebDAV mount without complete credentials"
                        );
                    }
                }
                if share.get("id").and_then(|value| value.as_str()).is_none() {
                    share["id"] = serde_json::json!(Uuid::new_v4().to_string());
                    migrated = true;
                }
            }
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
        let config = ConfigFile::default();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create configuration directory")?;
        }
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
        }
        tokio::fs::rename(&temporary, path)
            .await
            .context("Failed to commit configuration")
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

fn config_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
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
    async fn legacy_shares_receive_persisted_stable_ids() {
        let directory =
            std::env::temp_dir().join(format!("ycloud-migration-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");
        let legacy = serde_json::json!({
            "admin_username": "admin",
            "admin_password_hash": hash_password("test-password"),
            "shares": [{
                "name": "Legacy",
                "path": "",
                "enabled": true,
                "webdav_enabled": false,
                "readonly": false
            }]
        });
        tokio::fs::write(&path, serde_json::to_vec(&legacy).unwrap())
            .await
            .unwrap();

        let migrated = load_config(&path).await.unwrap();
        assert_eq!(migrated.shares.len(), 1);
        assert!(!migrated.shares[0].id.is_empty());

        let persisted: ConfigFile =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(persisted.shares[0].id, migrated.shares[0].id);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}

// ── Password helpers ──────────────────────────────────────────────
