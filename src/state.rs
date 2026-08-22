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

enum PreparedStorageBackend {
    Local,
    S3(crate::s3_backend::S3Backend),
}

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

        let backend = match &persisted.storage_backend {
            StorageBackendConfig::Local(_) => StorageBackend::local(storage.clone()),
            StorageBackendConfig::S3(settings) => {
                let backend = crate::s3_backend::S3Backend::new(settings, &config)?;
                backend.activation_probe().await?;
                let recovered = backend.recover_transactions().await?;
                if recovered > 0 {
                    tracing::warn!(recovered, "recovered pending S3 storage transactions");
                }
                StorageBackend::s3(backend)
            }
        };
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

    /// Validate all required S3 capabilities before persisting credentials as
    /// a pending backend. The active namespace is not changed by this step.
    pub async fn stage_s3_storage(
        &self,
        settings: crate::config::S3StorageConfig,
    ) -> AppResult<()> {
        let candidate = StorageBackendConfig::S3(settings.clone());
        self.config.allows_storage_backend(&candidate)?;
        let backend = crate::s3_backend::S3Backend::new(&settings, &self.config)?;
        backend.activation_probe().await?;
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        if next.storage_backend == candidate {
            return Err(AppError::Conflict("该存储已经是当前活动存储".into()));
        }
        next.pending_storage_backend = Some(candidate);
        self.persist_storage_selection(&next).await
    }

    pub async fn stage_local_storage(&self) -> AppResult<()> {
        let candidate = StorageBackendConfig::default();
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        if next.storage_backend == candidate {
            return Err(AppError::Conflict("本地存储已经是当前活动存储".into()));
        }
        next.pending_storage_backend = Some(candidate);
        self.persist_storage_selection(&next).await
    }

    pub async fn discard_pending_storage(&self) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        if next.pending_storage_backend.take().is_none() {
            return Err(AppError::NotFound);
        }
        self.persist_storage_selection(&next).await
    }

    /// Revalidate and recover a staged backend, drain active storage writes,
    /// persist the selection, then publish it to new requests. If validation
    /// or persistence fails, the current backend remains untouched.
    pub async fn activate_pending_storage(&self) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        let pending = next
            .pending_storage_backend
            .clone()
            .ok_or(AppError::NotFound)?;
        self.config.allows_storage_backend(&pending)?;

        let prepared = match &pending {
            StorageBackendConfig::Local(_) => PreparedStorageBackend::Local,
            StorageBackendConfig::S3(settings) => {
                let backend = crate::s3_backend::S3Backend::new(settings, &self.config)?;
                backend.activation_probe().await?;
                let recovered = backend.recover_transactions().await?;
                if recovered > 0 {
                    tracing::warn!(recovered, "recovered S3 transactions before activation");
                }
                PreparedStorageBackend::S3(backend)
            }
        };

        next.storage_backend = pending;
        next.pending_storage_backend = None;
        next.validate()?;

        // Taking the write side drains uploads and namespace changes. Reads
        // that already own an OS file or S3 response stream may finish safely.
        let mut replacement = self.backend.begin_replacement().await;
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| {
                AppError::with_source("failed to persist storage activation", error)
            })?;
        match prepared {
            PreparedStorageBackend::Local => {
                replacement.replace_with_local(self.storage.clone());
            }
            PreparedStorageBackend::S3(backend) => replacement.replace_with_s3(backend),
        }
        *self.config_file.write().await = next;
        drop(replacement);
        self.archive_tickets.clear().await;
        tracing::info!("storage backend activation completed");
        Ok(())
    }

    /// Save twice so both the primary config and its recovery backup contain
    /// the current storage credential set. Replacing or discarding a pending
    /// backend must not leave its superseded secret in `config.json.bak`.
    async fn persist_storage_selection(&self, next: &ConfigFile) -> AppResult<()> {
        next.validate()?;
        save_config(&self.config.config_path, next)
            .await
            .map_err(|error| AppError::with_source("failed to persist storage settings", error))?;
        save_config(&self.config.config_path, next)
            .await
            .map_err(|error| {
                AppError::with_source("failed to refresh storage configuration backup", error)
            })?;
        *self.config_file.write().await = next.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AppState;
    use crate::config::{
        Config, ConfigFile, S3AddressingStyle, S3Provider, S3StorageConfig, StorageBackendConfig,
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn discarding_pending_storage_scrubs_credentials_from_primary_and_backup() {
        let root =
            std::env::temp_dir().join(format!("ycloud-storage-state-{}", uuid::Uuid::new_v4()));
        let config_path = root.join("config.json");
        let state = AppState::new(
            Config {
                bind_address: std::net::IpAddr::from([127, 0, 0, 1]),
                port: 18_473,
                storage_path: root.join("storage"),
                config_path: config_path.clone(),
                max_upload_bytes: 5 * 1024 * 1024 * 1024,
                io_concurrency: 2,
                max_list_entries: 100,
                request_timeout_secs: 30,
                upload_timeout_secs: 300,
                disk_reserve_bytes: 0,
                secure_cookies: false,
                allow_lan_http: false,
                public_base_url: None,
                public_host: None,
                trusted_proxy_ips: Default::default(),
                s3_allowed_endpoints: Default::default(),
            },
            Arc::new(RwLock::new(ConfigFile::default())),
        )
        .await
        .unwrap();
        let mut pending = state.config_file.read().await.clone();
        pending.pending_storage_backend = Some(StorageBackendConfig::S3(S3StorageConfig {
            provider: S3Provider::AlibabaOss,
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            bucket: "ycloud-test".into(),
            region: "cn-hangzhou".into(),
            prefix: "files/".into(),
            addressing_style: S3AddressingStyle::VirtualHosted,
            access_key_id: "credential-to-remove".into(),
            secret_access_key: "secret-to-remove".into(),
        }));
        state.persist_storage_selection(&pending).await.unwrap();
        state.discard_pending_storage().await.unwrap();

        let primary = tokio::fs::read_to_string(&config_path).await.unwrap();
        let backup = tokio::fs::read_to_string(config_path.with_extension("json.bak"))
            .await
            .unwrap();
        for content in [&primary, &backup] {
            assert!(!content.contains("credential-to-remove"));
            assert!(!content.contains("secret-to-remove"));
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
