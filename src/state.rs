use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{
    auth::{
        AccessTokenStore, PasswordService, SessionStore, SharedAccessTokenStore, SharedSessionStore,
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
    config_updates: Arc<Mutex<()>>,
}

impl AppState {
    pub async fn new(config: Config, config_file: SharedConfig) -> AppResult<Self> {
        let storage = StorageService::new(
            config.storage_path.clone(),
            config.max_upload_bytes,
            config.io_concurrency,
            config.max_list_entries,
        )
        .await?;

        Ok(Self {
            config,
            config_file,
            sessions: Arc::new(SessionStore::new()),
            gate_access: Arc::new(AccessTokenStore::new()),
            folder_access: Arc::new(AccessTokenStore::new()),
            passwords: PasswordService::new(2),
            storage,
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
