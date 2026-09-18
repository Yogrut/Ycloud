use std::fmt;

use serde::{Deserialize, Serialize};

use super::{
    default_admin_login_failures, default_local_mount_id, default_login_block_seconds,
    default_max_archive_bytes, default_max_archive_entries, default_max_upload_batch_bytes,
    default_max_upload_batch_entries, default_max_upload_bytes, default_security_log_max_entries,
    default_security_log_retention_days, default_storage_id, default_true,
    default_web_login_failures, uuid_v4, DEFAULT_STORAGE_ID,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    #[serde(default = "uuid_v4")]
    pub id: String,
    #[serde(default = "default_storage_id")]
    pub storage_id: String,
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
    #[serde(default = "default_storage_id")]
    pub storage_id: String,
    pub path: String,
    pub password_hash: String,
}

/// File-browser permissions granted to one ordinary account for one storage.
/// Administrative configuration APIs are deliberately not represented here.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoragePermission {
    pub storage_id: String,
    #[serde(default)]
    pub browse: bool,
    #[serde(default)]
    pub download: bool,
    #[serde(default)]
    pub upload: bool,
    #[serde(default)]
    pub create_directory: bool,
    #[serde(default)]
    pub rename: bool,
    #[serde(default)]
    pub move_items: bool,
    #[serde(default)]
    pub copy: bool,
    #[serde(default)]
    pub delete: bool,
}

impl StoragePermission {
    pub fn grants_any(&self) -> bool {
        self.browse
            || self.download
            || self.upload
            || self.create_directory
            || self.rename
            || self.move_items
            || self.copy
            || self.delete
    }

    pub fn grants_write(&self) -> bool {
        self.upload
            || self.create_directory
            || self.rename
            || self.move_items
            || self.copy
            || self.delete
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserAccount {
    #[serde(default = "uuid_v4")]
    pub id: String,
    pub username: String,
    pub password_hash: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub permissions: Vec<StoragePermission>,
}

impl fmt::Debug for UserAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserAccount")
            .field("id", &self.id)
            .field("username", &self.username)
            .field("password_hash", &"[REDACTED]")
            .field("enabled", &self.enabled)
            .field("permissions", &self.permissions)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "settings", rename_all = "snake_case")]
pub enum StorageBackendConfig {
    Local(LocalStorageConfig),
    S3(S3StorageConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageInstanceConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_guest_access: bool,
    /// Missing on older configurations: inherit the existing guest access policy.
    #[serde(default)]
    pub allow_guest_download: Option<bool>,
    pub backend: StorageBackendConfig,
}

impl StorageInstanceConfig {
    pub(super) fn primary(backend: StorageBackendConfig) -> Self {
        let name = match &backend {
            StorageBackendConfig::Local(_) => "Local storage",
            StorageBackendConfig::S3(_) => "S3 storage",
        };
        Self {
            id: DEFAULT_STORAGE_ID.into(),
            name: name.into(),
            enabled: true,
            allow_guest_access: true,
            allow_guest_download: None,
            backend,
        }
    }
}

impl Default for StorageBackendConfig {
    fn default() -> Self {
        Self::Local(LocalStorageConfig::default())
    }
}

impl StorageBackendConfig {
    pub fn capacity_limit_bytes(&self) -> Option<u64> {
        match self {
            Self::Local(settings) => settings.capacity_limit_bytes,
            Self::S3(settings) => settings.capacity_limit_bytes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalStorageConfig {
    #[serde(default = "default_local_mount_id")]
    pub mount_id: String,
    #[serde(default)]
    pub capacity_limit_bytes: Option<u64>,
}

impl Default for LocalStorageConfig {
    fn default() -> Self {
        Self {
            mount_id: default_local_mount_id(),
            capacity_limit_bytes: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct S3StorageConfig {
    pub provider: S3Provider,
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub addressing_style: S3AddressingStyle,
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub capacity_limit_bytes: Option<u64>,
}

impl fmt::Debug for S3StorageConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3StorageConfig")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("prefix", &self.prefix)
            .field("addressing_style", &self.addressing_style)
            .field("access_key_id", &"[REDACTED]")
            .field("secret_access_key", &"[REDACTED]")
            .field("capacity_limit_bytes", &self.capacity_limit_bytes)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum S3Provider {
    AlibabaOss,
    TencentCos,
    Minio,
    S3Compatible,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum S3AddressingStyle {
    Path,
    VirtualHosted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub traffic: crate::traffic::TrafficSettings,
    #[serde(default)]
    pub domain_binding: Option<crate::domain_binding::DomainBinding>,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub storage_instances: Vec<StorageInstanceConfig>,
    pub default_storage_id: String,
    #[serde(default)]
    pub pending_storage_instance: Option<StorageInstanceConfig>,
    pub admin_username: String,
    pub admin_password_hash: String,
    #[serde(default)]
    pub admin_totp_secret: Option<String>,
    #[serde(default)]
    pub admin_recovery_code_hashes: Vec<String>,
    #[serde(default)]
    pub user_accounts: Vec<UserAccount>,
    #[serde(default)]
    pub global_web_password_hash: Option<String>,
    #[serde(default)]
    pub folder_locks: Vec<FolderLock>,
    #[serde(default)]
    pub shares: Vec<Share>,
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    #[serde(default = "default_max_upload_batch_bytes")]
    pub max_upload_batch_bytes: u64,
    #[serde(default = "default_max_upload_batch_entries")]
    pub max_upload_batch_entries: usize,
    #[serde(default = "default_max_archive_bytes")]
    pub max_archive_bytes: u64,
    #[serde(default = "default_max_archive_entries")]
    pub max_archive_entries: usize,
    #[serde(default = "default_admin_login_failures")]
    pub admin_login_failures: u32,
    #[serde(default = "default_web_login_failures")]
    pub web_login_failures: u32,
    #[serde(default = "default_login_block_seconds")]
    pub admin_login_block_seconds: u64,
    #[serde(default = "default_login_block_seconds")]
    pub web_login_block_seconds: u64,
    #[serde(default)]
    pub upload_rate_bytes_per_sec: u64,
    #[serde(default)]
    pub download_rate_bytes_per_sec: u64,
    #[serde(default = "default_security_log_retention_days")]
    pub security_log_retention_days: u32,
    #[serde(default = "default_security_log_max_entries")]
    pub security_log_max_entries: usize,
}
