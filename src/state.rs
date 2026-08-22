use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;

use crate::{
    archive::ArchiveTicketStore,
    auth::{
        AccessTokenStore, PasswordService, SessionStore, SharedAccessTokenStore, SharedSessionStore,
    },
    config::{save_config, Config, ConfigFile, SharedConfig, StorageBackendConfig},
    error::{AppError, AppResult},
    login_security::LoginSecurity,
    storage::StorageService,
    storage_backend::StorageBackend,
    transfer_limit::BandwidthLimiter,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub config_file: SharedConfig,
    pub sessions: SharedSessionStore,
    pub gate_access: SharedAccessTokenStore,
    pub folder_access: SharedAccessTokenStore,
    pub passwords: PasswordService,
    pub storage: StorageService,
    pub backend: StorageBackend,
    pub archive_tickets: ArchiveTicketStore,
    pub webdav_gate: Arc<Semaphore>,
    pub login_security: LoginSecurity,
    pub upload_limiter: BandwidthLimiter,
    pub download_limiter: BandwidthLimiter,
    config_updates: Arc<Mutex<()>>,
}

impl AppState {
    pub async fn new(config: Config, config_file: SharedConfig) -> AppResult<Self> {
        let persisted = config_file.read().await.clone();
        config.allows_storage_backend(&persisted.storage_backend)?;
        if matches!(persisted.storage_backend, StorageBackendConfig::S3(_)) {
            return Err(AppError::ServiceUnavailable(
                "S3 存储后端仍在分阶段验收，尚未允许接管真实文件流量".into(),
            ));
        }
        let max_upload_bytes = persisted.max_upload_bytes;
        if max_upload_bytes > config.max_upload_bytes {
            return Err(AppError::BadRequest(
                "Persisted upload limit exceeds the deployment MAX_UPLOAD_BYTES envelope".into(),
            ));
        }
        let storage = StorageService::new(
            config.storage_path.clone(),
            max_upload_bytes,
            config.io_concurrency,
            config.max_list_entries,
            config.disk_reserve_bytes,
        )
        .await?;
        let login_security = LoginSecurity::load(
            &config.config_path,
            persisted.security_log_retention_days,
            persisted.security_log_max_entries,
        )
        .await
        .map_err(|error| {
            AppError::with_source("failed to load persistent login security state", error)
        })?;

        let backend = StorageBackend::Local(storage.clone());
        Ok(Self {
            config,
            config_file,
            sessions: Arc::new(SessionStore::new()),
            gate_access: Arc::new(AccessTokenStore::new()),
            folder_access: Arc::new(AccessTokenStore::new()),
            passwords: PasswordService::new(1),
            storage,
            backend,
            archive_tickets: ArchiveTicketStore::new(),
            webdav_gate: Arc::new(Semaphore::new(8)),
            login_security,
            upload_limiter: BandwidthLimiter::new(persisted.upload_rate_bytes_per_sec),
            download_limiter: BandwidthLimiter::new(persisted.download_rate_bytes_per_sec),
            config_updates: Arc::new(Mutex::new(())),
        })
    }

    /// [稳定] Configuration updates are serialized, persisted first, and only
    /// then published to readers. A failed disk write cannot leave memory and
    /// disk with different configurations.
    pub async fn update_config<T, F>(&self, update: F) -> AppResult<T>
    where
        F: FnOnce(&mut ConfigFile) -> AppResult<T>,
    {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        let result = update(&mut next)?;
        next.validate()?;
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| AppError::with_source("failed to persist configuration", error))?;
        *self.config_file.write().await = next.clone();
        self.storage.set_max_upload_bytes(next.max_upload_bytes);
        self.upload_limiter.set_rate(next.upload_rate_bytes_per_sec);
        self.download_limiter
            .set_rate(next.download_rate_bytes_per_sec);
        if let Err(error) = self
            .login_security
            .configure_retention(
                next.security_log_retention_days,
                next.security_log_max_entries,
            )
            .await
        {
            tracing::warn!(%error, "security event log compaction will be retried later");
        }
        Ok(result)
    }
}
