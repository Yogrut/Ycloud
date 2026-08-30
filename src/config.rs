use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    storage_catalog::LocalMountCatalog,
};

mod migration;
mod model;
mod password;
mod persistence;
mod runtime;
mod secret_store;
mod validation;

pub use model::{
    ConfigFile, FolderLock, LocalStorageConfig, S3AddressingStyle, S3Provider, S3StorageConfig,
    Share, StorageBackendConfig, StorageInstanceConfig, StoragePermission, UserAccount,
};
pub use password::{hash_password, verify_password};
pub use persistence::{load_config, remove_initial_credentials, save_config};
pub use runtime::normalize_s3_endpoint;
pub use validation::{
    validate_login_security_settings, validate_storage_backend, validate_transfer_limits,
    validate_transfer_rate,
};

pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_UPLOAD_BATCH_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_UPLOAD_BATCH_ENTRIES: usize = 1_000;
pub const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_ARCHIVE_ENTRIES: usize = 1_000;
pub const DEFAULT_DEPLOYMENT_MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024 * 1024;
pub const DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_BYTES: u64 = 100 * 1024 * 1024 * 1024;
pub const DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_ENTRIES: usize = 10_000;
pub const DEFAULT_DEPLOYMENT_MAX_ARCHIVE_BYTES: u64 = 100 * 1024 * 1024 * 1024;
pub const DEFAULT_DEPLOYMENT_MAX_ARCHIVE_ENTRIES: usize = 100_000;
// Serialization and arithmetic sanity limits. Operational limits are supplied
// by the deployment envelope and must not be confused with these format bounds.
pub const HARD_MAX_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024 * 1024;
pub const HARD_MAX_UPLOAD_BATCH_BYTES: u64 = HARD_MAX_UPLOAD_BYTES;
// The batch manifest is accepted as one bounded JSON request and retained in
// memory until every item completes. Keep this format bound aligned with the
// deployment default and the active batch store instead of advertising a
// value the service cannot safely hold.
pub const HARD_MAX_UPLOAD_BATCH_ENTRIES: usize = 10_000;
pub const HARD_MAX_ARCHIVE_BYTES: u64 = HARD_MAX_UPLOAD_BYTES;
pub const HARD_MAX_ARCHIVE_ENTRIES: usize = 1_000_000;
pub const DEFAULT_ADMIN_LOGIN_FAILURES: u32 = 3;
pub const DEFAULT_WEB_LOGIN_FAILURES: u32 = 5;
pub const DEFAULT_LOGIN_BLOCK_SECONDS: u64 = 60 * 60;
pub const DEFAULT_SECURITY_LOG_RETENTION_DAYS: u32 = 7;
pub const DEFAULT_SECURITY_LOG_MAX_ENTRIES: usize = 5_000;
pub const HARD_MAX_TRANSFER_RATE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MIN_TRANSFER_RATE_BYTES: u64 = 64 * 1024;
const MIN_TRANSFER_BYTES: u64 = 1024 * 1024;
pub const MIN_STORAGE_CAPACITY_BYTES: u64 = 1024 * 1024;
pub const HARD_MAX_STORAGE_CAPACITY_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024 * 1024;
pub const CONFIG_SCHEMA_VERSION: u32 = 11;
pub const DEFAULT_STORAGE_ID: &str = "primary";
pub const MAX_STORAGE_INSTANCES: usize = 16;
pub const MAX_USER_ACCOUNTS: usize = 100;

fn uuid_v4() -> String {
    Uuid::new_v4().to_string()
}

fn default_storage_id() -> String {
    DEFAULT_STORAGE_ID.into()
}

fn default_local_mount_id() -> String {
    DEFAULT_STORAGE_ID.into()
}

const fn default_true() -> bool {
    true
}

impl FolderLock {
    pub fn matches(&self, storage_id: &str, request_path: &str) -> bool {
        self.storage_id == storage_id && path_is_same_or_descendant(request_path, &self.path)
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

/// WebDAV and browser folder-lock namespaces must never overlap. This includes
/// either direction and treats the storage root as an ancestor of every path.
pub fn paths_overlap(left: &str, right: &str) -> bool {
    let left: Vec<&str> = left
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let right: Vec<&str> = right
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let shared = left.len().min(right.len());
    left[..shared]
        .iter()
        .zip(&right[..shared])
        .all(|(a, b)| path_component_eq(a, b))
}

#[cfg(windows)]
fn path_component_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(not(windows))]
fn path_component_eq(left: &str, right: &str) -> bool {
    left == right
}

#[derive(Clone)]
pub struct Config {
    pub bind_address: IpAddr,
    pub port: u16,
    pub storage_path: PathBuf,
    pub local_mounts: LocalMountCatalog,
    pub config_path: PathBuf,
    pub max_upload_bytes: u64,
    pub max_upload_batch_bytes: u64,
    pub max_upload_batch_entries: usize,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
    pub io_concurrency: usize,
    pub max_list_entries: usize,
    pub request_timeout_secs: u64,
    pub upload_timeout_secs: u64,
    pub disk_reserve_bytes: u64,
    pub secure_cookies: bool,
    /// Allows direct HTTP access only from loopback, private, and link-local peers.
    /// This is deliberately separate from the strict public HTTPS proxy mode.
    pub allow_lan_http: bool,
    pub public_base_url: Option<String>,
    pub public_host: Option<String>,
    pub trusted_proxy_ips: HashSet<IpAddr>,
    /// Additional exact Host authorities accepted only in local/LAN mode.
    /// Public proxy mode always uses `public_host` exclusively.
    pub allowed_hosts: HashSet<String>,
    /// Exact origins that administrators may use for MinIO/RustFS or generic
    /// S3 endpoints. Official Alibaba and Tencent endpoints are constrained by
    /// their provider presets instead.
    pub s3_allowed_endpoints: HashSet<String>,
}

pub type SharedConfig = Arc<RwLock<ConfigFile>>;

const INITIAL_CREDENTIALS_FILE: &str = "initial-credentials.json";

#[derive(Debug, Serialize, Deserialize)]
struct InitialCredentials {
    admin_username: String,
    admin_password: String,
    web_access_password: String,
}

// ── Serde defaults ────────────────────────────────────────────────

fn default_admin_username() -> String {
    "admin".into()
}

const fn default_max_upload_bytes() -> u64 {
    DEFAULT_MAX_UPLOAD_BYTES
}

const fn default_max_upload_batch_bytes() -> u64 {
    DEFAULT_MAX_UPLOAD_BATCH_BYTES
}

const fn default_max_upload_batch_entries() -> usize {
    DEFAULT_MAX_UPLOAD_BATCH_ENTRIES
}

const fn default_max_archive_bytes() -> u64 {
    DEFAULT_MAX_ARCHIVE_BYTES
}

const fn default_max_archive_entries() -> usize {
    DEFAULT_MAX_ARCHIVE_ENTRIES
}

const fn default_admin_login_failures() -> u32 {
    DEFAULT_ADMIN_LOGIN_FAILURES
}

const fn default_web_login_failures() -> u32 {
    DEFAULT_WEB_LOGIN_FAILURES
}

const fn default_login_block_seconds() -> u64 {
    DEFAULT_LOGIN_BLOCK_SECONDS
}

const fn default_security_log_retention_days() -> u32 {
    DEFAULT_SECURITY_LOG_RETENTION_DAYS
}

const fn default_security_log_max_entries() -> usize {
    DEFAULT_SECURITY_LOG_MAX_ENTRIES
}

// ── ConfigFile ────────────────────────────────────────────────────

impl Default for ConfigFile {
    fn default() -> Self {
        let first = Uuid::new_v4().simple().to_string();
        let second = Uuid::new_v4().simple().to_string();
        let default_hash = hash_password(&format!("{}{}", &first[..12], &second[..12]));
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            storage_instances: vec![StorageInstanceConfig::primary(
                StorageBackendConfig::default(),
            )],
            default_storage_id: default_storage_id(),
            pending_storage_instance: None,
            admin_username: default_admin_username(),
            admin_password_hash: default_hash.clone(),
            admin_totp_secret: None,
            admin_recovery_code_hashes: Vec::new(),
            user_accounts: Vec::new(),
            global_web_password_hash: Some(default_hash),
            folder_locks: Vec::new(),
            shares: vec![Share {
                id: uuid_v4(),
                storage_id: default_storage_id(),
                name: "Default".into(),
                path: String::new(),
                username: Some("admin".into()),
                webdav_enabled: false,
                password_hash: None,
                readonly: false,
            }],
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_upload_batch_bytes: DEFAULT_MAX_UPLOAD_BATCH_BYTES,
            max_upload_batch_entries: DEFAULT_MAX_UPLOAD_BATCH_ENTRIES,
            max_archive_bytes: DEFAULT_MAX_ARCHIVE_BYTES,
            max_archive_entries: DEFAULT_MAX_ARCHIVE_ENTRIES,
            admin_login_failures: DEFAULT_ADMIN_LOGIN_FAILURES,
            web_login_failures: DEFAULT_WEB_LOGIN_FAILURES,
            admin_login_block_seconds: DEFAULT_LOGIN_BLOCK_SECONDS,
            web_login_block_seconds: DEFAULT_LOGIN_BLOCK_SECONDS,
            upload_rate_bytes_per_sec: 0,
            download_rate_bytes_per_sec: 0,
            security_log_retention_days: DEFAULT_SECURITY_LOG_RETENTION_DAYS,
            security_log_max_entries: DEFAULT_SECURITY_LOG_MAX_ENTRIES,
        }
    }
}

#[cfg(test)]
use persistence::{config_backup_path, initial_credentials_path};

#[cfg(test)]
mod tests {
    use super::{
        config_backup_path, hash_password, initial_credentials_path, load_config,
        normalize_s3_endpoint, path_is_same_or_descendant, paths_overlap,
        remove_initial_credentials, save_config, verify_password, Config, ConfigFile, FolderLock,
        InitialCredentials, LocalStorageConfig, S3AddressingStyle, S3Provider, S3StorageConfig,
        StorageBackendConfig, StorageInstanceConfig, StoragePermission, UserAccount,
        CONFIG_SCHEMA_VERSION, DEFAULT_MAX_ARCHIVE_BYTES, DEFAULT_MAX_ARCHIVE_ENTRIES,
        DEFAULT_MAX_UPLOAD_BATCH_BYTES, DEFAULT_MAX_UPLOAD_BATCH_ENTRIES, DEFAULT_MAX_UPLOAD_BYTES,
        DEFAULT_STORAGE_ID, HARD_MAX_ARCHIVE_BYTES, HARD_MAX_ARCHIVE_ENTRIES,
        HARD_MAX_TRANSFER_RATE_BYTES, HARD_MAX_UPLOAD_BATCH_ENTRIES, MIN_TRANSFER_RATE_BYTES,
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
    fn webdav_and_folder_lock_paths_cannot_overlap_in_either_direction() {
        assert!(paths_overlap("", "private"));
        assert!(paths_overlap("games", "games/locked"));
        assert!(paths_overlap("games/locked/child", "games/locked"));
        assert!(!paths_overlap("games", "documents"));
        assert!(!paths_overlap("test", "test-two"));
    }

    #[test]
    fn folder_lock_protects_descendants() {
        let lock = FolderLock {
            id: "lock-id".into(),
            storage_id: DEFAULT_STORAGE_ID.into(),
            path: "photos/private".into(),
            password_hash: "unused".into(),
        };

        assert!(lock.matches(DEFAULT_STORAGE_ID, "photos/private"));
        assert!(lock.matches(DEFAULT_STORAGE_ID, "photos/private/child/file.txt"));
        assert!(!lock.matches(DEFAULT_STORAGE_ID, "photos/public"));
        assert!(!lock.matches(DEFAULT_STORAGE_ID, "photos/private-old"));
        assert!(!lock.matches("another", "photos/private"));
    }

    #[test]
    fn enabled_webdav_requires_credentials() {
        let mut config = ConfigFile::default();
        config.shares[0].webdav_enabled = true;
        assert!(config.validate().is_err());
        config.shares[0].password_hash = Some(hash_password("test-password"));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn transfer_limits_reject_values_outside_safe_envelope() {
        let mut config = ConfigFile {
            max_upload_bytes: 0,
            ..ConfigFile::default()
        };
        assert!(config.validate().is_err());
        config.max_upload_bytes = DEFAULT_MAX_UPLOAD_BYTES;
        config.max_upload_batch_bytes = DEFAULT_MAX_UPLOAD_BYTES - 1;
        assert!(config.validate().is_err());
        config.max_upload_batch_bytes = DEFAULT_MAX_UPLOAD_BATCH_BYTES;
        config.max_upload_batch_entries = HARD_MAX_UPLOAD_BATCH_ENTRIES + 1;
        assert!(config.validate().is_err());
        config.max_upload_batch_entries = DEFAULT_MAX_UPLOAD_BATCH_ENTRIES;
        config.max_archive_bytes = HARD_MAX_ARCHIVE_BYTES + 1;
        assert!(config.validate().is_err());
        config.max_archive_bytes = DEFAULT_MAX_ARCHIVE_BYTES;
        config.max_archive_entries = HARD_MAX_ARCHIVE_ENTRIES + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn adjustable_security_and_rate_limits_keep_hard_boundaries() {
        let mut config = ConfigFile {
            admin_login_failures: 2,
            ..ConfigFile::default()
        };
        assert!(config.validate().is_err());
        config.admin_login_failures = 3;
        config.web_login_failures = 21;
        assert!(config.validate().is_err());
        config.web_login_failures = 5;
        config.admin_login_block_seconds = 299;
        assert!(config.validate().is_err());
        config.admin_login_block_seconds = 300;
        config.security_log_retention_days = 2;
        assert!(config.validate().is_err());
        config.security_log_retention_days = 7;
        config.security_log_max_entries = 20_001;
        assert!(config.validate().is_err());
        config.security_log_max_entries = 5_000;
        config.upload_rate_bytes_per_sec = MIN_TRANSFER_RATE_BYTES - 1;
        assert!(config.validate().is_err());
        config.upload_rate_bytes_per_sec = MIN_TRANSFER_RATE_BYTES;
        config.download_rate_bytes_per_sec = HARD_MAX_TRANSFER_RATE_BYTES + 1;
        assert!(config.validate().is_err());
        config.download_rate_bytes_per_sec = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn ordinary_accounts_require_unique_names_and_storage_scoped_permissions() {
        let mut config = ConfigFile::default();
        config.user_accounts.push(UserAccount {
            id: "reader".into(),
            username: "reader".into(),
            password_hash: hash_password("ordinary-user-password"),
            enabled: true,
            permissions: vec![StoragePermission {
                storage_id: DEFAULT_STORAGE_ID.into(),
                browse: true,
                download: true,
                upload: false,
                create_directory: false,
                rename: false,
                move_items: false,
                copy: false,
                delete: false,
            }],
        });
        assert!(config.validate().is_ok());

        config.user_accounts[0].username = config.admin_username.clone();
        assert!(config.validate().is_err());
        config.user_accounts[0].username = "reader".into();
        config.user_accounts[0].permissions[0].browse = false;
        assert!(config.validate().is_err());
        config.user_accounts[0].permissions[0].browse = true;
        config.user_accounts[0].permissions[0].storage_id = "missing-storage".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn local_storage_instances_require_distinct_deployment_mounts() {
        let mut config = ConfigFile::default();
        config.storage_instances.push(StorageInstanceConfig {
            id: "archive".into(),
            name: "Archive disk".into(),
            enabled: true,
            allow_guest_access: false,
            backend: StorageBackendConfig::Local(LocalStorageConfig {
                mount_id: "archive-disk".into(),
                capacity_limit_bytes: None,
            }),
        });

        assert!(config.validate().is_ok());

        config.storage_instances[1].backend = StorageBackendConfig::Local(LocalStorageConfig {
            mount_id: "primary".into(),
            capacity_limit_bytes: None,
        });
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn config_save_keeps_last_known_good_backup() {
        let directory =
            std::env::temp_dir().join(format!("ycloud-config-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");
        let mut config = ConfigFile {
            schema_version: CONFIG_SCHEMA_VERSION,
            admin_username: "first-admin".into(),
            admin_password_hash: hash_password("test-password"),
            global_web_password_hash: None,
            folder_locks: Vec::new(),
            shares: Vec::new(),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_archive_bytes: DEFAULT_MAX_ARCHIVE_BYTES,
            max_archive_entries: DEFAULT_MAX_ARCHIVE_ENTRIES,
            ..ConfigFile::default()
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
    async fn stable_config_migrates_to_current_schema() {
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
        assert_eq!(migrated.shares[0].storage_id, DEFAULT_STORAGE_ID);
        assert_eq!(migrated.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(migrated.max_archive_bytes, DEFAULT_MAX_ARCHIVE_BYTES);
        assert_eq!(migrated.max_archive_entries, DEFAULT_MAX_ARCHIVE_ENTRIES);

        let persisted: ConfigFile =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(persisted.shares[0].id, migrated.shares[0].id);
        assert_eq!(persisted.shares[0].storage_id, DEFAULT_STORAGE_ID);
        assert_eq!(persisted.schema_version, CONFIG_SCHEMA_VERSION);
        assert_eq!(persisted.storage_instances.len(), 1);
        assert_eq!(persisted.storage_instances[0].id, DEFAULT_STORAGE_ID);
        assert_eq!(
            persisted.storage_instances[0].backend,
            StorageBackendConfig::Local(LocalStorageConfig::default())
        );
        assert_eq!(persisted.pending_storage_instance, None);
        assert!(persisted.user_accounts.is_empty());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn active_s3_with_pending_local_migrates_to_one_local_instance() {
        let directory = std::env::temp_dir().join(format!(
            "ycloud-storage-v6-migration-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");
        let mut raw = serde_json::to_value(ConfigFile::default()).unwrap();
        raw["schema_version"] = serde_json::json!(5);
        raw["storage_backend"] = serde_json::to_value(StorageBackendConfig::S3(S3StorageConfig {
            provider: S3Provider::Minio,
            endpoint: "http://10.126.0.2:9000".into(),
            bucket: "ycloud-files".into(),
            region: "us-east-1".into(),
            prefix: "files/".into(),
            addressing_style: S3AddressingStyle::Path,
            access_key_id: "migration-access-key".into(),
            secret_access_key: "migration-secret-key".into(),
            capacity_limit_bytes: None,
        }))
        .unwrap();
        raw["pending_storage_backend"] =
            serde_json::to_value(StorageBackendConfig::Local(LocalStorageConfig {
                capacity_limit_bytes: Some(800 * 1024 * 1024),
                ..LocalStorageConfig::default()
            }))
            .unwrap();
        let object = raw.as_object_mut().unwrap();
        object.remove("storage_instances");
        object.remove("default_storage_id");
        object.remove("pending_storage_instance");
        tokio::fs::write(&path, serde_json::to_vec(&raw).unwrap())
            .await
            .unwrap();

        let migrated = load_config(&path).await.unwrap();
        assert_eq!(migrated.storage_instances.len(), 2);
        assert_eq!(
            migrated
                .storage_instances
                .iter()
                .filter(|instance| matches!(instance.backend, StorageBackendConfig::Local(_)))
                .count(),
            1
        );
        let local = migrated
            .storage_instances
            .iter()
            .find(|instance| matches!(instance.backend, StorageBackendConfig::Local(_)))
            .unwrap();
        assert_eq!(
            local.backend.capacity_limit_bytes(),
            Some(800 * 1024 * 1024)
        );
        assert!(migrated.pending_storage_instance.is_none());
        assert_eq!(migrated.default_storage_id, DEFAULT_STORAGE_ID);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[test]
    fn pending_storage_must_differ_from_active_storage() {
        let mut config = ConfigFile {
            pending_storage_instance: Some(StorageInstanceConfig {
                id: "pending-local".into(),
                name: "Second local".into(),
                enabled: true,
                allow_guest_access: false,
                backend: StorageBackendConfig::Local(LocalStorageConfig::default()),
            }),
            ..ConfigFile::default()
        };
        assert!(config.validate().is_err());

        let pending = S3StorageConfig {
            provider: S3Provider::Minio,
            endpoint: "http://10.126.0.2:9000".into(),
            bucket: "ycloud-files".into(),
            region: "us-east-1".into(),
            prefix: "files/".into(),
            addressing_style: S3AddressingStyle::Path,
            access_key_id: "example-access-key".into(),
            secret_access_key: "example-secret-key".into(),
            capacity_limit_bytes: None,
        };
        config.pending_storage_instance = Some(StorageInstanceConfig {
            id: "pending-s3".into(),
            name: "Remote storage".into(),
            enabled: true,
            allow_guest_access: false,
            backend: StorageBackendConfig::S3(pending),
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    fn storage_capacity_limit_has_safe_numeric_bounds() {
        let mut config = ConfigFile {
            ..ConfigFile::default()
        };
        config.storage_instances[0].backend = StorageBackendConfig::Local(LocalStorageConfig {
            capacity_limit_bytes: Some(super::MIN_STORAGE_CAPACITY_BYTES - 1),
            ..LocalStorageConfig::default()
        });
        assert!(config.validate().is_err());
        config.storage_instances[0].backend = StorageBackendConfig::Local(LocalStorageConfig {
            capacity_limit_bytes: Some(super::MIN_STORAGE_CAPACITY_BYTES),
            ..LocalStorageConfig::default()
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    fn s3_storage_config_is_strict_and_redacts_credentials() {
        let settings = S3StorageConfig {
            provider: S3Provider::Minio,
            endpoint: "http://10.126.0.2:9000".into(),
            bucket: "ycloud-files".into(),
            region: "us-east-1".into(),
            prefix: "files/".into(),
            addressing_style: S3AddressingStyle::Path,
            access_key_id: "example-access-key".into(),
            secret_access_key: "example-secret-key".into(),
            capacity_limit_bytes: None,
        };
        let mut config = ConfigFile {
            storage_instances: vec![StorageInstanceConfig {
                id: DEFAULT_STORAGE_ID.into(),
                name: "S3 storage".into(),
                enabled: true,
                allow_guest_access: true,
                backend: StorageBackendConfig::S3(settings.clone()),
            }],
            ..ConfigFile::default()
        };
        assert!(config.validate().is_ok());

        let debug = format!("{:?}", config.storage_instances[0].backend);
        assert!(!debug.contains("example-access-key"));
        assert!(!debug.contains("example-secret-key"));

        config.storage_instances[0].backend = StorageBackendConfig::S3(S3StorageConfig {
            prefix: "../escape/".into(),
            ..settings.clone()
        });
        assert!(config.validate().is_err());

        config.storage_instances[0].backend = StorageBackendConfig::S3(S3StorageConfig {
            provider: S3Provider::AlibabaOss,
            endpoint: "http://oss-cn-hangzhou.aliyuncs.com".into(),
            addressing_style: S3AddressingStyle::Path,
            ..settings
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn custom_s3_endpoint_cannot_expand_deployment_allowlist() {
        let endpoint = normalize_s3_endpoint("HTTP://10.126.0.2:9000/").unwrap();
        assert_eq!(endpoint, "http://10.126.0.2:9000");
        let runtime = Config {
            bind_address: std::net::IpAddr::from([127, 0, 0, 1]),
            port: 18_473,
            storage_path: "./storage".into(),
            local_mounts: crate::storage_catalog::LocalMountCatalog::new(
                "./storage".into(),
                Vec::new(),
            )
            .unwrap(),
            config_path: "./config.json".into(),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_upload_batch_bytes: super::DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_BYTES,
            max_upload_batch_entries: super::DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_ENTRIES,
            max_archive_bytes: super::DEFAULT_DEPLOYMENT_MAX_ARCHIVE_BYTES,
            max_archive_entries: super::DEFAULT_DEPLOYMENT_MAX_ARCHIVE_ENTRIES,
            io_concurrency: 4,
            max_list_entries: 10_000,
            request_timeout_secs: 300,
            upload_timeout_secs: 21_600,
            disk_reserve_bytes: 512 * 1024 * 1024,
            secure_cookies: false,
            allow_lan_http: false,
            public_base_url: None,
            public_host: None,
            trusted_proxy_ips: Default::default(),
            allowed_hosts: Default::default(),
            s3_allowed_endpoints: [endpoint].into_iter().collect(),
        };
        let settings = S3StorageConfig {
            provider: S3Provider::Minio,
            endpoint: "http://10.126.0.2:9000".into(),
            bucket: "ycloud-files".into(),
            region: "us-east-1".into(),
            prefix: "files/".into(),
            addressing_style: S3AddressingStyle::Path,
            access_key_id: "example-access-key".into(),
            secret_access_key: "example-secret-key".into(),
            capacity_limit_bytes: None,
        };
        assert!(runtime
            .allows_storage_backend(&StorageBackendConfig::S3(settings.clone()))
            .is_ok());
        assert!(runtime
            .allows_storage_backend(&StorageBackendConfig::S3(S3StorageConfig {
                endpoint: "http://10.126.0.3:9000".into(),
                ..settings
            }))
            .is_err());
    }

    #[test]
    fn official_s3_endpoints_must_match_the_selected_region() {
        let base = S3StorageConfig {
            provider: S3Provider::TencentCos,
            endpoint: "https://cos.ap-chengdu.myqcloud.com".into(),
            bucket: "ycloud-files".into(),
            region: "ap-chengdu".into(),
            prefix: "files/".into(),
            addressing_style: S3AddressingStyle::VirtualHosted,
            access_key_id: "example-access-key".into(),
            secret_access_key: "example-secret-key".into(),
            capacity_limit_bytes: None,
        };
        let configured = |settings| ConfigFile {
            storage_instances: vec![StorageInstanceConfig {
                id: DEFAULT_STORAGE_ID.into(),
                name: "Official S3".into(),
                enabled: true,
                allow_guest_access: true,
                backend: StorageBackendConfig::S3(settings),
            }],
            ..ConfigFile::default()
        };

        assert!(configured(base.clone()).validate().is_ok());
        assert!(configured(S3StorageConfig {
            endpoint: "https://cos.ap-guangzhou.myqcloud.com".into(),
            ..base.clone()
        })
        .validate()
        .is_err());
        assert!(configured(S3StorageConfig {
            endpoint: "https://cos.ap-chengdu.myqcloud.com:8443".into(),
            ..base.clone()
        })
        .validate()
        .is_err());
        assert!(configured(S3StorageConfig {
            provider: S3Provider::AlibabaOss,
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            region: "cn-hangzhou".into(),
            ..base
        })
        .validate()
        .is_ok());
    }

    #[tokio::test]
    async fn first_start_generates_recoverable_independent_credentials() {
        let directory =
            std::env::temp_dir().join(format!("ycloud-first-start-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");

        let first = load_config(&path).await.unwrap();
        let credentials_path = initial_credentials_path(&path);
        let credentials: InitialCredentials =
            serde_json::from_slice(&tokio::fs::read(&credentials_path).await.unwrap()).unwrap();
        assert_eq!(credentials.admin_username, "admin");
        assert_ne!(credentials.admin_password, credentials.web_access_password);
        assert_eq!(credentials.admin_password.chars().count(), 12);
        assert_eq!(credentials.web_access_password.chars().count(), 8);
        assert!(verify_password(
            &first.admin_password_hash,
            &credentials.admin_password
        ));
        assert!(verify_password(
            first.global_web_password_hash.as_deref().unwrap(),
            &credentials.web_access_password
        ));

        tokio::fs::remove_file(&path).await.unwrap();
        let recovered = load_config(&path).await.unwrap();
        assert!(verify_password(
            &recovered.admin_password_hash,
            &credentials.admin_password
        ));
        assert!(verify_password(
            recovered.global_web_password_hash.as_deref().unwrap(),
            &credentials.web_access_password
        ));
        assert!(remove_initial_credentials(&path).await.unwrap());
        assert!(!tokio::fs::try_exists(credentials_path).await.unwrap());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn managed_master_key_encrypts_totp_secret_at_rest() {
        let directory =
            std::env::temp_dir().join(format!("ycloud-totp-secret-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("config.json");
        let secret = crate::totp::generate_secret();
        let config = ConfigFile {
            admin_totp_secret: Some(secret.clone()),
            admin_recovery_code_hashes: vec![hash_password("ABCDE23456")],
            ..ConfigFile::default()
        };

        save_config(&path, &config).await.unwrap();
        let persisted = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!persisted.contains(&secret));
        assert!(persisted.contains("enc:v2:"));
        assert!(directory
            .join(".ycloud-system")
            .join("secrets")
            .join("master.key")
            .is_file());

        let loaded = load_config(&path).await.unwrap();
        assert_eq!(loaded.admin_totp_secret.as_deref(), Some(secret.as_str()));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}

// ── Password helpers ──────────────────────────────────────────────
