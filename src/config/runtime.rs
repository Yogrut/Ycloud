use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::Context;

use super::{
    AppError, AppResult, Config, LocalMountCatalog, S3Provider, StorageBackendConfig,
    DEFAULT_DEPLOYMENT_MAX_ARCHIVE_BYTES, DEFAULT_DEPLOYMENT_MAX_ARCHIVE_ENTRIES,
    DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_BYTES, DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_ENTRIES,
    DEFAULT_DEPLOYMENT_MAX_UPLOAD_BYTES, HARD_MAX_ARCHIVE_BYTES, HARD_MAX_ARCHIVE_ENTRIES,
    HARD_MAX_UPLOAD_BATCH_BYTES, HARD_MAX_UPLOAD_BATCH_ENTRIES, HARD_MAX_UPLOAD_BYTES,
};

impl Config {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let bind_address = env_parse("BIND_ADDRESS", IpAddr::from([127, 0, 0, 1]))?;
        let port = env_parse("PORT", 18_473_u16)?;
        let storage_path =
            PathBuf::from(std::env::var("STORAGE_PATH").unwrap_or_else(|_| "./storage".into()));
        let additional_local_mounts = std::env::var("LOCAL_STORAGE_MOUNTS").ok();
        let local_mounts =
            LocalMountCatalog::from_json(storage_path.clone(), additional_local_mounts.as_deref())?;
        let config_path =
            PathBuf::from(std::env::var("CONFIG_PATH").unwrap_or_else(|_| "./config.json".into()));
        // This is an absolute transport envelope. The administrator-facing,
        // persisted upload limit is enforced by StorageService and can be
        // changed without exposing concurrency/resource protection controls.
        let max_upload_bytes = env_parse("MAX_UPLOAD_BYTES", DEFAULT_DEPLOYMENT_MAX_UPLOAD_BYTES)?;
        let max_upload_batch_bytes = env_parse(
            "MAX_UPLOAD_BATCH_BYTES",
            DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_BYTES,
        )?;
        let max_upload_batch_entries = env_parse(
            "MAX_UPLOAD_BATCH_ENTRIES",
            DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_ENTRIES,
        )?;
        let max_archive_bytes =
            env_parse("MAX_ARCHIVE_BYTES", DEFAULT_DEPLOYMENT_MAX_ARCHIVE_BYTES)?;
        let max_archive_entries = env_parse(
            "MAX_ARCHIVE_ENTRIES",
            DEFAULT_DEPLOYMENT_MAX_ARCHIVE_ENTRIES,
        )?;
        validate_deployment_envelope(
            max_upload_bytes,
            max_upload_batch_bytes,
            max_upload_batch_entries,
            max_archive_bytes,
            max_archive_entries,
        )?;
        let io_concurrency = env_parse("IO_CONCURRENCY", 4_usize)?;
        let max_list_entries = env_parse("MAX_LIST_ENTRIES", 10_000_usize)?;
        let request_timeout_secs = env_parse("REQUEST_TIMEOUT_SECS", 300_u64)?;
        let upload_timeout_secs = env_parse("UPLOAD_TIMEOUT_SECS", 6_u64 * 60 * 60)?;
        let disk_reserve_bytes = env_parse("DISK_RESERVE_BYTES", 512_u64 * 1024 * 1024)?;
        let allowed_hosts = allowed_hosts()?;
        let s3_allowed_endpoints = s3_allowed_endpoints()?;
        Ok(Self {
            bind_address,
            port,
            storage_path,
            local_mounts,
            config_path,
            max_upload_bytes,
            max_upload_batch_bytes,
            max_upload_batch_entries,
            max_archive_bytes,
            max_archive_entries,
            io_concurrency,
            max_list_entries,
            request_timeout_secs,
            upload_timeout_secs,
            disk_reserve_bytes,
            secure_cookies: false,
            public_base_url: None,
            public_host: None,
            allowed_hosts,
            s3_allowed_endpoints,
            // Bootstrap replaces this only after the persisted configuration
            // has been validated. This avoids creating a replacement master
            // key before an existing encrypted configuration is opened.
            transaction_auth_key: [0; 32],
        })
    }

    pub(crate) async fn initialize_transaction_auth_key(&mut self) -> anyhow::Result<()> {
        self.transaction_auth_key = super::secret_store::SecretStore::for_write(&self.config_path)
            .await?
            .derive_key("ycloud:s3-transaction-auth:v1");
        Ok(())
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

fn validate_deployment_envelope(
    upload_bytes: u64,
    batch_bytes: u64,
    batch_entries: usize,
    archive_bytes: u64,
    archive_entries: usize,
) -> anyhow::Result<()> {
    if upload_bytes == 0 || upload_bytes > HARD_MAX_UPLOAD_BYTES {
        anyhow::bail!("MAX_UPLOAD_BYTES is outside the supported format range");
    }
    if batch_bytes < upload_bytes || batch_bytes > HARD_MAX_UPLOAD_BATCH_BYTES {
        anyhow::bail!("MAX_UPLOAD_BATCH_BYTES must be at least MAX_UPLOAD_BYTES");
    }
    if batch_entries == 0 || batch_entries > HARD_MAX_UPLOAD_BATCH_ENTRIES {
        anyhow::bail!("MAX_UPLOAD_BATCH_ENTRIES is outside the supported format range");
    }
    if archive_bytes == 0 || archive_bytes > HARD_MAX_ARCHIVE_BYTES {
        anyhow::bail!("MAX_ARCHIVE_BYTES is outside the supported format range");
    }
    if archive_entries == 0 || archive_entries > HARD_MAX_ARCHIVE_ENTRIES {
        anyhow::bail!("MAX_ARCHIVE_ENTRIES is outside the supported format range");
    }
    Ok(())
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

fn allowed_hosts() -> anyhow::Result<HashSet<String>> {
    std::env::var("ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let authority = value
                .parse::<axum::http::uri::Authority>()
                .with_context(|| format!("Invalid ALLOWED_HOSTS authority: {value}"))?;
            if authority.as_str().contains('@') {
                anyhow::bail!("ALLOWED_HOSTS cannot contain user information: {value}");
            }
            Ok(authority.as_str().to_ascii_lowercase())
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
