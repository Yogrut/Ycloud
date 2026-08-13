use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;

use crate::{
    archive::ArchiveTicketStore,
    auth::{
        AccessTokenStore, PasswordService, RateLimiter, SessionStore, SharedAccessTokenStore,
        SharedSessionStore,
    },
    config::{save_config, Config, ConfigFile, SharedConfig},
    error::{AppError, AppResult},
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
    pub webdav_failures: Arc<RateLimiter>,
    config_updates: Arc<Mutex<()>>,
}

impl AppState {
    pub async fn new(config: Config, config_file: SharedConfig) -> AppResult<Self> {
        let storage = StorageService::new(
            config.storage_path.clone(),
            config.max_upload_bytes,
            config.io_concurrency,
            config.max_list_entries,
            config.disk_reserve_bytes,
        )
        .await?;

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
            webdav_failures: Arc::new(RateLimiter::new(5, 60)),
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
        *self.config_file.write().await = next;
        Ok(result)
    }
}
