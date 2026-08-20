use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;

use crate::{
    archive::ArchiveTicketStore,
    auth::{
        AccessTokenStore, PasswordService, SessionStore, SharedAccessTokenStore, SharedSessionStore,
    },
    config::{save_config, Config, ConfigFile, SharedConfig},
    error::{AppError, AppResult},
    login_security::LoginSecurity,
    storage::StorageService,
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
    pub archive_tickets: ArchiveTicketStore,
    pub webdav_gate: Arc<Semaphore>,
    pub login_security: LoginSecurity,
    config_updates: Arc<Mutex<()>>,
}

impl AppState {
    pub async fn new(config: Config, config_file: SharedConfig) -> AppResult<Self> {
        let max_upload_bytes = config_file.read().await.max_upload_bytes;
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
        let login_security = LoginSecurity::load(&config.config_path)
            .await
            .map_err(|error| {
                AppError::with_source("failed to load persistent login security state", error)
            })?;

        Ok(Self {
            config,
            config_file,
            sessions: Arc::new(SessionStore::new()),
            gate_access: Arc::new(AccessTokenStore::new()),
            folder_access: Arc::new(AccessTokenStore::new()),
            passwords: PasswordService::new(1),
            storage,
            archive_tickets: ArchiveTicketStore::new(),
            webdav_gate: Arc::new(Semaphore::new(8)),
            login_security,
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
        self.storage.set_max_upload_bytes(next.max_upload_bytes);
        *self.config_file.write().await = next;
        Ok(result)
    }
}
