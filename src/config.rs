use std::collections::HashSet;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_ARCHIVE_ENTRIES: usize = 1_000;
pub const HARD_MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024 * 1024;
pub const HARD_MAX_ARCHIVE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const HARD_MAX_ARCHIVE_ENTRIES: usize = 5_000;
pub const DEFAULT_ADMIN_LOGIN_FAILURES: u32 = 3;
pub const DEFAULT_WEB_LOGIN_FAILURES: u32 = 5;
pub const DEFAULT_LOGIN_BLOCK_SECONDS: u64 = 60 * 60;
pub const DEFAULT_SECURITY_LOG_RETENTION_DAYS: u32 = 7;
pub const DEFAULT_SECURITY_LOG_MAX_ENTRIES: usize = 5_000;
pub const HARD_MAX_TRANSFER_RATE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MIN_TRANSFER_RATE_BYTES: u64 = 64 * 1024;
const MIN_TRANSFER_BYTES: u64 = 1024 * 1024;
pub const CONFIG_SCHEMA_VERSION: u32 = 3;

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

/// Persisted storage selection. Provider presets share one S3 implementation;
/// they only constrain endpoint and addressing defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "settings", rename_all = "snake_case")]
pub enum StorageBackendConfig {
    Local(LocalStorageConfig),
    S3(S3StorageConfig),
}

impl Default for StorageBackendConfig {
    fn default() -> Self {
        Self::Local(LocalStorageConfig {})
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalStorageConfig {}

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
}

/// Never include the recoverable S3 secret in diagnostics.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub storage_backend: StorageBackendConfig,
    pub admin_username: String,
    pub admin_password_hash: String,
    #[serde(default)]
    pub global_web_password_hash: Option<String>,
    #[serde(default)]
    pub folder_locks: Vec<FolderLock>,
    #[serde(default)]
    pub shares: Vec<Share>,
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
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
    /// Allows direct HTTP access only from loopback, private, and link-local peers.
    /// This is deliberately separate from the strict public HTTPS proxy mode.
    pub allow_lan_http: bool,
    pub public_base_url: Option<String>,
    pub public_host: Option<String>,
    pub trusted_proxy_ips: HashSet<IpAddr>,
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
            storage_backend: StorageBackendConfig::default(),
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
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
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

impl ConfigFile {
    pub fn validate(&self) -> AppResult<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
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
        validate_transfer_limits(
            self.max_upload_bytes,
            self.max_archive_bytes,
            self.max_archive_entries,
        )?;
        validate_login_security_settings(
            self.admin_login_failures,
            self.web_login_failures,
            self.admin_login_block_seconds,
            self.web_login_block_seconds,
            self.security_log_retention_days,
            self.security_log_max_entries,
        )?;
        validate_transfer_rate(self.upload_rate_bytes_per_sec, "上传")?;
        validate_transfer_rate(self.download_rate_bytes_per_sec, "下载")?;
        validate_storage_backend(&self.storage_backend)?;

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
        for share in self.shares.iter().filter(|share| share.webdav_enabled) {
            if self
                .folder_locks
                .iter()
                .any(|lock| paths_overlap(&share.path, &lock.path))
            {
                return Err(AppError::Conflict(
                    "WebDAV 挂载不能与网页文件夹锁的父目录、当前目录或子目录重叠".into(),
                ));
            }
        }
        Ok(())
    }
}

pub fn validate_login_security_settings(
    admin_failures: u32,
    web_failures: u32,
    admin_block_seconds: u64,
    web_block_seconds: u64,
    retention_days: u32,
    max_entries: usize,
) -> AppResult<()> {
    if !(3..=10).contains(&admin_failures) {
        return Err(AppError::BadRequest(
            "管理员登录错误次数必须在 3 到 10 之间".into(),
        ));
    }
    if !(3..=20).contains(&web_failures) {
        return Err(AppError::BadRequest(
            "首页登录错误次数必须在 3 到 20 之间".into(),
        ));
    }
    if !(5 * 60..=24 * 60 * 60).contains(&admin_block_seconds)
        || !(5 * 60..=24 * 60 * 60).contains(&web_block_seconds)
    {
        return Err(AppError::BadRequest(
            "登录封禁时间必须在 5 分钟到 24 小时之间".into(),
        ));
    }
    if ![1, 3, 5, 7, 15, 30].contains(&retention_days) {
        return Err(AppError::BadRequest(
            "日志保存天数只能是 1、3、5、7、15 或 30 天".into(),
        ));
    }
    if !(500..=20_000).contains(&max_entries) {
        return Err(AppError::BadRequest(
            "日志最大条目数必须在 500 到 20000 之间".into(),
        ));
    }
    Ok(())
}

pub fn validate_transfer_rate(bytes_per_second: u64, label: &str) -> AppResult<()> {
    if bytes_per_second != 0
        && !(MIN_TRANSFER_RATE_BYTES..=HARD_MAX_TRANSFER_RATE_BYTES).contains(&bytes_per_second)
    {
        return Err(AppError::BadRequest(
            format!("{label}限速必须为 0，或在 64 KiB/s 到 1 GiB/s 之间").into(),
        ));
    }
    Ok(())
}

pub fn validate_transfer_limits(
    max_upload_bytes: u64,
    max_archive_bytes: u64,
    max_archive_entries: usize,
) -> AppResult<()> {
    if !(MIN_TRANSFER_BYTES..=HARD_MAX_UPLOAD_BYTES).contains(&max_upload_bytes) {
        return Err(AppError::BadRequest(
            "单文件上传上限必须在 1 MiB 到 100 GiB 之间".into(),
        ));
    }
    if !(MIN_TRANSFER_BYTES..=HARD_MAX_ARCHIVE_BYTES).contains(&max_archive_bytes) {
        return Err(AppError::BadRequest(
            "打包源文件总大小上限必须在 1 MiB 到 10 GiB 之间".into(),
        ));
    }
    if !(1..=HARD_MAX_ARCHIVE_ENTRIES).contains(&max_archive_entries) {
        return Err(AppError::BadRequest(
            "打包条目数量上限必须在 1 到 5000 之间".into(),
        ));
    }
    Ok(())
}

pub fn validate_storage_backend(backend: &StorageBackendConfig) -> AppResult<()> {
    let StorageBackendConfig::S3(settings) = backend else {
        return Ok(());
    };

    let endpoint = settings.endpoint.trim();
    if endpoint != settings.endpoint || endpoint.is_empty() || endpoint.len() > 2_048 {
        return Err(AppError::BadRequest(
            "S3 Endpoint 必须是长度不超过 2048 字符的完整地址".into(),
        ));
    }
    let uri: axum::http::Uri = endpoint
        .parse()
        .map_err(|_| AppError::BadRequest("S3 Endpoint 地址无效".into()))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| AppError::BadRequest("S3 Endpoint 必须包含 http:// 或 https://".into()))?;
    if !matches!(scheme, "http" | "https")
        || uri.authority().is_none()
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return Err(AppError::BadRequest(
            "S3 Endpoint 只能是无凭据、无路径和无查询参数的 HTTP(S) 地址".into(),
        ));
    }

    let host = uri
        .authority()
        .expect("authority checked above")
        .host()
        .to_ascii_lowercase();
    match settings.provider {
        S3Provider::AlibabaOss => {
            if scheme != "https"
                || !host.ends_with(".aliyuncs.com")
                || settings.addressing_style != S3AddressingStyle::VirtualHosted
            {
                return Err(AppError::BadRequest(
                    "阿里云 OSS 必须使用 HTTPS 官方 Endpoint 和虚拟主机寻址".into(),
                ));
            }
        }
        S3Provider::TencentCos => {
            if scheme != "https"
                || !host.ends_with(".myqcloud.com")
                || settings.addressing_style != S3AddressingStyle::VirtualHosted
            {
                return Err(AppError::BadRequest(
                    "腾讯云 COS 必须使用 HTTPS 官方 Endpoint 和虚拟主机寻址".into(),
                ));
            }
        }
        S3Provider::Minio | S3Provider::S3Compatible => {}
    }

    validate_s3_bucket(&settings.bucket)?;
    if settings.region.is_empty()
        || settings.region.len() > 64
        || !settings
            .region
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AppError::BadRequest("S3 Region 格式无效".into()));
    }
    validate_s3_prefix(&settings.prefix)?;
    if settings.access_key_id.is_empty()
        || settings.access_key_id.len() > 256
        || settings.access_key_id.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest("S3 Access Key ID 格式无效".into()));
    }
    if settings.secret_access_key.is_empty()
        || settings.secret_access_key.len() > 4_096
        || settings.secret_access_key.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest("S3 Secret Access Key 格式无效".into()));
    }
    Ok(())
}

fn validate_s3_bucket(bucket: &str) -> AppResult<()> {
    let bytes = bucket.as_bytes();
    let valid = (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'.' | b'-')
        })
        && !bucket.contains("..")
        && !bucket.contains(".-")
        && !bucket.contains("-.")
        && bucket.parse::<IpAddr>().is_err();
    if !valid {
        return Err(AppError::BadRequest(
            "S3 Bucket 必须符合 3-63 位 DNS 兼容命名规则".into(),
        ));
    }
    Ok(())
}

fn validate_s3_prefix(prefix: &str) -> AppResult<()> {
    if prefix.len() > 1_024
        || prefix.starts_with('/')
        || prefix.contains("//")
        || prefix.contains('\\')
        || prefix.contains('\0')
        || prefix.chars().any(char::is_control)
        || (!prefix.is_empty() && !prefix.ends_with('/'))
        || prefix
            .trim_end_matches('/')
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(AppError::BadRequest(
            "S3 Prefix 必须为空或使用以 / 结尾的安全相对路径".into(),
        ));
    }
    Ok(())
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
        let port = env_parse("PORT", 18_473_u16)?;
        let storage_path =
            PathBuf::from(std::env::var("STORAGE_PATH").unwrap_or_else(|_| "./storage".into()));
        let config_path =
            PathBuf::from(std::env::var("CONFIG_PATH").unwrap_or_else(|_| "./config.json".into()));
        // This is an absolute transport envelope. The administrator-facing,
        // persisted upload limit is enforced by StorageService and can be
        // changed without exposing concurrency/resource protection controls.
        let max_upload_bytes = env_parse("MAX_UPLOAD_BYTES", HARD_MAX_UPLOAD_BYTES)?;
        let io_concurrency = env_parse("IO_CONCURRENCY", 4_usize)?;
        let max_list_entries = env_parse("MAX_LIST_ENTRIES", 10_000_usize)?;
        let request_timeout_secs = env_parse("REQUEST_TIMEOUT_SECS", 300_u64)?;
        let upload_timeout_secs = env_parse("UPLOAD_TIMEOUT_SECS", 6_u64 * 60 * 60)?;
        let disk_reserve_bytes = env_parse("DISK_RESERVE_BYTES", 512_u64 * 1024 * 1024)?;
        let secure_cookies = env_parse("SECURE_COOKIES", false)?;
        let allow_lan_http = env_parse("ALLOW_LAN_HTTP", false)?;
        let (public_base_url, public_host, trusted_proxy_ips) =
            public_proxy_config(bind_address, secure_cookies, allow_lan_http)?;
        let s3_allowed_endpoints = s3_allowed_endpoints()?;
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
            allow_lan_http,
            public_base_url,
            public_host,
            trusted_proxy_ips,
            s3_allowed_endpoints,
        })
    }

    pub fn is_public_mode(&self) -> bool {
        self.public_base_url.is_some()
    }

    pub fn allows_storage_backend(&self, backend: &StorageBackendConfig) -> AppResult<()> {
        let StorageBackendConfig::S3(settings) = backend else {
            return Ok(());
        };
        if matches!(
            settings.provider,
            S3Provider::AlibabaOss | S3Provider::TencentCos
        ) {
            return Ok(());
        }
        let endpoint = normalize_s3_endpoint(&settings.endpoint)?;
        if self.s3_allowed_endpoints.contains(&endpoint) {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

fn s3_allowed_endpoints() -> anyhow::Result<HashSet<String>> {
    std::env::var("S3_ALLOWED_ENDPOINTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            normalize_s3_endpoint(value)
                .map_err(|_| anyhow::anyhow!("Invalid S3_ALLOWED_ENDPOINTS origin: {value}"))
        })
        .collect()
}

pub fn normalize_s3_endpoint(endpoint: &str) -> AppResult<String> {
    let uri: axum::http::Uri = endpoint
        .parse()
        .map_err(|_| AppError::BadRequest("S3 Endpoint 地址无效".into()))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| AppError::BadRequest("S3 Endpoint 必须包含 http:// 或 https://".into()))?;
    let authority = uri
        .authority()
        .ok_or_else(|| AppError::BadRequest("S3 Endpoint 缺少主机".into()))?;
    if !matches!(scheme, "http" | "https")
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || authority.as_str().contains('@')
    {
        return Err(AppError::BadRequest(
            "S3 Endpoint 只能是无凭据、无路径和无查询参数的 HTTP(S) 地址".into(),
        ));
    }
    Ok(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.as_str().to_ascii_lowercase()
    ))
}

fn public_proxy_config(
    bind_address: IpAddr,
    secure_cookies: bool,
    allow_lan_http: bool,
) -> anyhow::Result<(Option<String>, Option<String>, HashSet<IpAddr>)> {
    if bind_address.is_loopback() {
        return Ok((None, None, HashSet::new()));
    }
    if allow_lan_http {
        if secure_cookies {
            anyhow::bail!(
                "SECURE_COOKIES must be false for direct LAN HTTP; use public HTTPS proxy mode instead"
            );
        }
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
        if schema_version > CONFIG_SCHEMA_VERSION as u64 {
            anyhow::bail!(
                "Configuration schema version {schema_version} is newer than this Ycloud build"
            );
        }
        if schema_version < 2 {
            raw["schema_version"] = serde_json::json!(2);
            migrated = true;
        }
        if schema_version < CONFIG_SCHEMA_VERSION as u64 {
            raw["schema_version"] = serde_json::json!(CONFIG_SCHEMA_VERSION);
            if raw.get("storage_backend").is_none() {
                raw["storage_backend"] = serde_json::json!({
                    "type": "local",
                    "settings": {}
                });
            }
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
        warn_if_initial_credentials_remain(path).await?;
        Ok(config)
    } else {
        let credentials_path = initial_credentials_path(path);
        let credentials = load_or_create_initial_credentials(&credentials_path).await?;
        let config = ConfigFile {
            schema_version: CONFIG_SCHEMA_VERSION,
            storage_backend: StorageBackendConfig::default(),
            admin_username: credentials.admin_username.clone(),
            admin_password_hash: hash_password(&credentials.admin_password),
            global_web_password_hash: Some(hash_password(&credentials.web_access_password)),
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
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
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
        };
        save_config(path, &config).await?;
        tracing::warn!(
            path = %credentials_path.display(),
            "initial administrator and web access credentials were generated; read this file and change both passwords after signing in"
        );
        Ok(config)
    }
}

async fn warn_if_initial_credentials_remain(config_path: &Path) -> anyhow::Result<()> {
    let path = initial_credentials_path(config_path);
    if tokio::fs::try_exists(&path).await? {
        tracing::warn!(
            path = %path.display(),
            "initial plaintext credentials still exist; change both passwords in the administrator page"
        );
    }
    Ok(())
}

fn initial_credentials_path(config_path: &Path) -> PathBuf {
    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join(INITIAL_CREDENTIALS_FILE)
}

async fn load_or_create_initial_credentials(path: &Path) -> anyhow::Result<InitialCredentials> {
    if tokio::fs::try_exists(path).await? {
        let content = tokio::fs::read(path)
            .await
            .context("Failed to read initial credentials file")?;
        let credentials: InitialCredentials =
            serde_json::from_slice(&content).context("Initial credentials file is invalid")?;
        validate_initial_credentials(&credentials)?;
        secure_file_permissions(path).await?;
        return Ok(credentials);
    }

    let credentials = InitialCredentials {
        admin_username: default_admin_username(),
        admin_password: random_password(9),
        web_access_password: random_password(6),
    };
    let json = serde_json::to_vec_pretty(&credentials)?;
    let temporary = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .await
        .context("Failed to create temporary initial credentials file")?;
    let prepare_result = async {
        file.write_all(&json)
            .await
            .context("Failed to write initial credentials file")?;
        file.sync_all()
            .await
            .context("Failed to flush initial credentials file")
    }
    .await;
    drop(file);
    if let Err(error) = prepare_result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error).context("Failed to publish initial credentials file");
    }
    secure_file_permissions(path).await?;
    Ok(credentials)
}

fn validate_initial_credentials(credentials: &InitialCredentials) -> anyhow::Result<()> {
    if credentials.admin_username.trim().is_empty()
        || credentials.admin_username.chars().count() > 128
        || credentials.admin_password.chars().count() < 12
        || credentials.web_access_password.chars().count() < 8
        || credentials.admin_password.len() > 4_096
        || credentials.web_access_password.len() > 4_096
    {
        anyhow::bail!("Initial credentials file does not satisfy the password policy");
    }
    Ok(())
}

fn random_password(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

pub async fn remove_initial_credentials(config_path: &Path) -> anyhow::Result<bool> {
    let path = initial_credentials_path(config_path);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to remove initial credentials file {}",
                path.display()
            )
        }),
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
        config_backup_path, hash_password, initial_credentials_path, load_config,
        normalize_s3_endpoint, path_is_same_or_descendant, paths_overlap,
        remove_initial_credentials, save_config, verify_password, Config, ConfigFile, FolderLock,
        InitialCredentials, LocalStorageConfig, S3AddressingStyle, S3Provider, S3StorageConfig,
        StorageBackendConfig, CONFIG_SCHEMA_VERSION, DEFAULT_MAX_ARCHIVE_BYTES,
        DEFAULT_MAX_ARCHIVE_ENTRIES, DEFAULT_MAX_UPLOAD_BYTES, HARD_MAX_TRANSFER_RATE_BYTES,
        MIN_TRANSFER_RATE_BYTES,
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

    #[test]
    fn transfer_limits_reject_values_outside_safe_envelope() {
        let mut config = ConfigFile {
            max_upload_bytes: 0,
            ..ConfigFile::default()
        };
        assert!(config.validate().is_err());
        config.max_upload_bytes = DEFAULT_MAX_UPLOAD_BYTES;
        config.max_archive_bytes = 10 * 1024 * 1024 * 1024 + 1;
        assert!(config.validate().is_err());
        config.max_archive_bytes = DEFAULT_MAX_ARCHIVE_BYTES;
        config.max_archive_entries = 5_001;
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
        assert_eq!(migrated.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(migrated.max_archive_bytes, DEFAULT_MAX_ARCHIVE_BYTES);
        assert_eq!(migrated.max_archive_entries, DEFAULT_MAX_ARCHIVE_ENTRIES);

        let persisted: ConfigFile =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(persisted.shares[0].id, migrated.shares[0].id);
        assert_eq!(persisted.schema_version, CONFIG_SCHEMA_VERSION);
        assert_eq!(
            persisted.storage_backend,
            StorageBackendConfig::Local(LocalStorageConfig {})
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
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
        };
        let mut config = ConfigFile {
            storage_backend: StorageBackendConfig::S3(settings.clone()),
            ..ConfigFile::default()
        };
        assert!(config.validate().is_ok());

        let debug = format!("{:?}", config.storage_backend);
        assert!(!debug.contains("example-access-key"));
        assert!(!debug.contains("example-secret-key"));

        config.storage_backend = StorageBackendConfig::S3(S3StorageConfig {
            prefix: "../escape/".into(),
            ..settings.clone()
        });
        assert!(config.validate().is_err());

        config.storage_backend = StorageBackendConfig::S3(S3StorageConfig {
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
            config_path: "./config.json".into(),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
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
}

// ── Password helpers ──────────────────────────────────────────────
