use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tokio::sync::Semaphore;

use crate::{
    archive::ArchiveTicketStore,
    auth::{
        AccessTokenStore, PasswordService, SessionStore, SharedAccessTokenStore, SharedSessionStore,
    },
    config::{
        save_config, Config, ConfigFile, SharedConfig, StorageBackendConfig, StorageInstanceConfig,
    },
    error::{AppError, AppResult},
    login_security::LoginSecurity,
    storage::StorageService,
    storage_backend::{StorageBackend, StorageRegistry},
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
    pub backends: StorageRegistry,
    pub archive_tickets: ArchiveTicketStore,
    pub webdav_gate: Arc<Semaphore>,
    pub login_security: LoginSecurity,
    pub upload_limiter: BandwidthLimiter,
    pub download_limiter: BandwidthLimiter,
    config_updates: Arc<Mutex<()>>,
    local_io_gate: Arc<Semaphore>,
}

impl AppState {
    pub async fn new(config: Config, config_file: SharedConfig) -> AppResult<Self> {
        let persisted = config_file.read().await.clone();
        let max_upload_bytes = persisted.max_upload_bytes;
        if max_upload_bytes > config.max_upload_bytes {
            return Err(AppError::BadRequest(
                "Persisted upload limit exceeds the deployment MAX_UPLOAD_BYTES envelope".into(),
            ));
        }
        let local_io_gate = Arc::new(Semaphore::new(config.io_concurrency.max(1)));
        let backends = StorageRegistry::new();
        for instance in &persisted.storage_instances {
            if !instance.enabled {
                continue;
            }
            match prepare_storage_backend(
                &config,
                &local_io_gate,
                max_upload_bytes,
                &instance.id,
                &instance.backend,
            )
            .await
            {
                Ok(backend) => backends.insert_ready(instance.id.clone(), backend).await,
                Err(error) => {
                    tracing::error!(
                        storage_id = %instance.id,
                        storage_name = %instance.name,
                        %error,
                        "storage instance is unavailable; keeping the service online"
                    );
                    backends.insert_unavailable(instance.id.clone()).await;
                }
            }
        }
        let login_security = LoginSecurity::load(
            &config.config_path,
            persisted.security_log_retention_days,
            persisted.security_log_max_entries,
        )
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
            backends,
            archive_tickets: ArchiveTicketStore::new(),
            webdav_gate: Arc::new(Semaphore::new(8)),
            login_security,
            upload_limiter: BandwidthLimiter::new(persisted.upload_rate_bytes_per_sec),
            download_limiter: BandwidthLimiter::new(persisted.download_rate_bytes_per_sec),
            config_updates: Arc::new(Mutex::new(())),
            local_io_gate,
        })
    }

    pub async fn storage_backend(&self, storage_id: &str) -> AppResult<StorageBackend> {
        self.backends.get(storage_id).await
    }

    pub async fn add_local_storage(
        &self,
        path: String,
        name: String,
        capacity_limit_bytes: Option<u64>,
        enabled: bool,
        allow_guest_access: bool,
    ) -> AppResult<String> {
        let mount_id = self
            .config
            .local_mounts
            .resolve_path(std::path::Path::new(path.trim()))
            .map_err(|_| AppError::BadRequest("本地存储路径无效".into()))?
            .map(|mount| mount.id.clone())
            .ok_or_else(|| {
                AppError::BadRequest(
                    "该路径未通过 STORAGE_PATH 或 LOCAL_STORAGE_MOUNTS 声明".into(),
                )
            })?;
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        if next.storage_instances.iter().any(|instance| {
            matches!(
                &instance.backend,
                StorageBackendConfig::Local(settings) if settings.mount_id == mount_id
            )
        }) {
            return Err(AppError::Conflict("该部署挂载点已经添加为本地存储".into()));
        }
        let storage_id = format!("local-{}", uuid::Uuid::new_v4().simple());
        let instance = StorageInstanceConfig {
            id: storage_id.clone(),
            name: name.trim().to_string(),
            enabled,
            allow_guest_access,
            backend: StorageBackendConfig::Local(crate::config::LocalStorageConfig {
                mount_id,
                capacity_limit_bytes,
            }),
        };
        let backend_config = instance.backend.clone();
        next.storage_instances.push(instance);
        next.validate()?;
        let prepared = if enabled {
            Some(
                prepare_storage_backend(
                    &self.config,
                    &self.local_io_gate,
                    next.max_upload_bytes,
                    &storage_id,
                    &backend_config,
                )
                .await?,
            )
        } else {
            None
        };
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| AppError::with_source("failed to persist local storage", error))?;
        if let Some(prepared) = prepared {
            self.backends
                .insert_ready(storage_id.clone(), prepared)
                .await;
        }
        *self.config_file.write().await = next;
        Ok(storage_id)
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
        if next.max_upload_bytes > self.config.max_upload_bytes {
            return Err(AppError::BadRequest(
                "Persisted upload limit exceeds the deployment MAX_UPLOAD_BYTES envelope".into(),
            ));
        }
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| AppError::with_source("failed to persist configuration", error))?;
        *self.config_file.write().await = next.clone();
        self.backends
            .set_local_max_upload_bytes(next.max_upload_bytes)
            .await;
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
    /// a pending instance. Existing namespaces are not changed by this step.
    pub async fn stage_s3_storage(
        &self,
        name: String,
        settings: crate::config::S3StorageConfig,
        enabled: bool,
        allow_guest_access: bool,
    ) -> AppResult<()> {
        let backend_config = StorageBackendConfig::S3(settings.clone());
        self.config.allows_storage_backend(&backend_config)?;
        let backend = crate::s3_backend::S3Backend::new(&settings, &self.config)?;
        backend.activation_probe().await?;
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        next.pending_storage_instance = Some(StorageInstanceConfig {
            id: format!("storage-{}", uuid::Uuid::new_v4().simple()),
            name,
            enabled,
            allow_guest_access,
            backend: backend_config,
        });
        self.persist_storage_selection(&next).await
    }

    pub async fn update_s3_storage(
        &self,
        storage_id: &str,
        name: String,
        mut settings: crate::config::S3StorageConfig,
    ) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        let instance = next
            .storage_instances
            .iter_mut()
            .find(|instance| instance.id == storage_id)
            .ok_or(AppError::NotFound)?;
        let StorageBackendConfig::S3(previous) = &instance.backend else {
            return Err(AppError::BadRequest("该存储源不是 S3 存储".into()));
        };
        if settings.access_key_id.is_empty() {
            settings.access_key_id = previous.access_key_id.clone();
        }
        if settings.secret_access_key.is_empty() {
            settings.secret_access_key = previous.secret_access_key.clone();
        }
        let backend_config = StorageBackendConfig::S3(settings.clone());
        self.config.allows_storage_backend(&backend_config)?;
        let probe = crate::s3_backend::S3Backend::new(&settings, &self.config)?;
        probe.activation_probe().await?;
        let prepared = prepare_storage_backend(
            &self.config,
            &self.local_io_gate,
            next.max_upload_bytes,
            storage_id,
            &backend_config,
        )
        .await?;
        instance.name = name.trim().to_string();
        instance.backend = backend_config;
        let enabled = instance.enabled;
        next.validate()?;
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| {
                AppError::with_source("failed to persist S3 storage settings", error)
            })?;
        *self.config_file.write().await = next;
        if enabled {
            self.backends
                .insert_ready(storage_id.to_string(), prepared)
                .await;
        } else {
            self.backends.remove(storage_id).await;
        }
        self.archive_tickets.clear().await;
        Ok(())
    }

    pub async fn update_local_storage(
        &self,
        storage_id: &str,
        name: String,
        path: String,
        capacity_limit_bytes: Option<u64>,
    ) -> AppResult<()> {
        let mount_id = self
            .config
            .local_mounts
            .resolve_path(std::path::Path::new(path.trim()))
            .map_err(|_| AppError::BadRequest("本地存储路径无效".into()))?
            .map(|mount| mount.id.clone())
            .ok_or_else(|| {
                AppError::BadRequest(
                    "该路径未通过 STORAGE_PATH 或 LOCAL_STORAGE_MOUNTS 声明".into(),
                )
            })?;
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        let position = next
            .storage_instances
            .iter()
            .position(|instance| {
                instance.id == storage_id
                    && matches!(instance.backend, StorageBackendConfig::Local(_))
            })
            .ok_or(AppError::NotFound)?;
        if next
            .storage_instances
            .iter()
            .enumerate()
            .any(|(index, instance)| {
                index != position
                    && matches!(
                        &instance.backend,
                        StorageBackendConfig::Local(settings) if settings.mount_id == mount_id
                    )
            })
        {
            return Err(AppError::Conflict("该部署挂载点已经添加为本地存储".into()));
        }
        let backend_config = StorageBackendConfig::Local(crate::config::LocalStorageConfig {
            mount_id,
            capacity_limit_bytes,
        });
        let enabled = next.storage_instances[position].enabled;
        let prepared = if enabled {
            Some(
                prepare_storage_backend(
                    &self.config,
                    &self.local_io_gate,
                    next.max_upload_bytes,
                    storage_id,
                    &backend_config,
                )
                .await?,
            )
        } else {
            None
        };
        next.storage_instances[position].name = name.trim().to_string();
        next.storage_instances[position].backend = backend_config;
        next.validate()?;
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| {
                AppError::with_source("failed to persist local storage settings", error)
            })?;
        *self.config_file.write().await = next;
        if let Some(prepared) = prepared {
            self.backends
                .insert_ready(storage_id.to_string(), prepared)
                .await;
        } else {
            self.backends.remove(storage_id).await;
        }
        self.archive_tickets.clear().await;
        Ok(())
    }

    pub async fn discard_pending_storage(&self) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        if next.pending_storage_instance.take().is_none() {
            return Err(AppError::NotFound);
        }
        self.persist_storage_selection(&next).await
    }

    /// Revalidate and recover a staged instance, persist it, then publish it
    /// to the registry. Existing instances and their namespaces are untouched.
    pub async fn activate_pending_storage(&self) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        let pending = next
            .pending_storage_instance
            .clone()
            .ok_or(AppError::NotFound)?;
        self.config.allows_storage_backend(&pending.backend)?;
        let prepared = if pending.enabled {
            Some(
                prepare_storage_backend(
                    &self.config,
                    &self.local_io_gate,
                    next.max_upload_bytes,
                    &pending.id,
                    &pending.backend,
                )
                .await?,
            )
        } else {
            None
        };

        next.storage_instances.push(pending.clone());
        next.pending_storage_instance = None;
        next.validate()?;
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| AppError::with_source("failed to persist storage instance", error))?;
        if let Some(prepared) = prepared {
            self.backends
                .insert_ready(pending.id.clone(), prepared)
                .await;
        }
        *self.config_file.write().await = next;
        tracing::info!(storage_id = %pending.id, "storage instance activation completed");
        Ok(())
    }

    pub async fn update_storage_access(
        &self,
        storage_id: &str,
        enabled: bool,
        allow_guest_access: bool,
    ) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        let instance = next
            .storage_instances
            .iter_mut()
            .find(|instance| instance.id == storage_id)
            .ok_or(AppError::NotFound)?;
        instance.enabled = enabled;
        instance.allow_guest_access = allow_guest_access;
        let backend_config = instance.backend.clone();
        next.validate()?;
        save_config(&self.config.config_path, &next)
            .await
            .map_err(|error| {
                AppError::with_source("failed to persist storage access settings", error)
            })?;
        *self.config_file.write().await = next.clone();
        if enabled {
            match prepare_storage_backend(
                &self.config,
                &self.local_io_gate,
                next.max_upload_bytes,
                storage_id,
                &backend_config,
            )
            .await
            {
                Ok(backend) => {
                    self.backends
                        .insert_ready(storage_id.to_string(), backend)
                        .await
                }
                Err(error) => {
                    tracing::warn!(storage_id, %error, "enabled storage remains in abnormal state");
                    self.backends
                        .insert_unavailable(storage_id.to_string())
                        .await;
                }
            }
        } else {
            self.backends.remove(storage_id).await;
        }
        self.gate_access.clear().await;
        self.folder_access.clear().await;
        self.archive_tickets.clear().await;
        Ok(())
    }

    pub async fn delete_storage(&self, storage_id: &str) -> AppResult<()> {
        let _update_guard = self.config_updates.lock().await;
        let mut next = self.config_file.read().await.clone();
        if next
            .shares
            .iter()
            .any(|share| share.storage_id == storage_id)
            || next
                .folder_locks
                .iter()
                .any(|lock| lock.storage_id == storage_id)
            || next.user_accounts.iter().any(|account| {
                account
                    .permissions
                    .iter()
                    .any(|permission| permission.storage_id == storage_id)
            })
        {
            return Err(AppError::Conflict(
                "该存储仍被 WebDAV、文件夹锁或普通账号权限引用".into(),
            ));
        }
        let before = next.storage_instances.len();
        next.storage_instances.retain(|item| item.id != storage_id);
        if before == next.storage_instances.len() {
            return Err(AppError::NotFound);
        }
        if next.default_storage_id == storage_id {
            next.default_storage_id = next
                .storage_instances
                .first()
                .map(|instance| instance.id.clone())
                .unwrap_or_default();
        }
        self.persist_storage_selection(&next).await?;
        self.backends.remove(storage_id).await;
        self.gate_access.clear().await;
        self.folder_access.clear().await;
        self.archive_tickets.clear().await;
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

async fn prepare_storage_backend(
    config: &Config,
    local_io_gate: &Arc<Semaphore>,
    max_upload_bytes: u64,
    storage_id: &str,
    backend: &StorageBackendConfig,
) -> AppResult<StorageBackend> {
    let ledger_path = capacity_ledger_path(config, storage_id);
    config.allows_storage_backend(backend)?;
    match backend {
        StorageBackendConfig::Local(settings) => {
            let mount = config
                .local_mounts
                .resolve(&settings.mount_id)
                .ok_or_else(|| {
                    AppError::ServiceUnavailable("本地存储引用的部署挂载点当前不可用".into())
                })?;
            let storage = if settings.mount_id == crate::storage_catalog::PRIMARY_LOCAL_MOUNT_ID {
                StorageService::new_with_io_gate(
                    mount.path.clone(),
                    max_upload_bytes,
                    local_io_gate.clone(),
                    config.max_list_entries,
                    config.disk_reserve_bytes,
                )
                .await?
            } else {
                StorageService::open_declared(
                    mount.path.clone(),
                    max_upload_bytes,
                    local_io_gate.clone(),
                    config.max_list_entries,
                    config.disk_reserve_bytes,
                )
                .await?
            };
            StorageBackend::local_configured(storage, settings.capacity_limit_bytes, ledger_path)
                .await
        }
        StorageBackendConfig::S3(settings) => {
            let backend = crate::s3_backend::S3Backend::new(settings, config)?;
            backend.activation_probe().await?;
            let recovered = backend.recover_transactions().await?;
            if recovered > 0 {
                tracing::warn!(recovered, "recovered pending S3 storage transactions");
            }
            StorageBackend::s3_configured(backend, settings.capacity_limit_bytes, ledger_path).await
        }
    }
}

fn capacity_ledger_path(config: &Config, storage_id: &str) -> PathBuf {
    config
        .config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".ycloud-system")
        .join("capacity")
        .join(format!("{storage_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::AppState;
    use crate::config::{
        Config, ConfigFile, LocalStorageConfig, S3AddressingStyle, S3Provider, S3StorageConfig,
        Share, StorageBackendConfig, StorageInstanceConfig,
    };
    use crate::storage_catalog::{DeploymentLocalMount, LocalMountCatalog};
    use axum::body::Body;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn declared_local_mounts_run_as_independent_storage_backends() {
        let root =
            std::env::temp_dir().join(format!("ycloud-local-mounts-{}", uuid::Uuid::new_v4()));
        let primary = root.join("primary");
        let archive = root.join("archive");
        tokio::fs::create_dir_all(&archive).await.unwrap();
        let mut persisted = ConfigFile {
            max_upload_bytes: 1024 * 1024,
            ..ConfigFile::default()
        };
        persisted.storage_instances.push(StorageInstanceConfig {
            id: "archive".into(),
            name: "Archive".into(),
            enabled: true,
            allow_guest_access: false,
            backend: StorageBackendConfig::Local(LocalStorageConfig {
                mount_id: "archive-disk".into(),
                capacity_limit_bytes: None,
            }),
        });
        let state = AppState::new(
            Config {
                bind_address: std::net::IpAddr::from([127, 0, 0, 1]),
                port: 18_473,
                storage_path: primary.clone(),
                local_mounts: LocalMountCatalog::new(
                    primary.clone(),
                    vec![DeploymentLocalMount {
                        id: "archive-disk".into(),
                        name: "Archive disk".into(),
                        path: archive.clone(),
                    }],
                )
                .unwrap(),
                config_path: root.join("config.json"),
                max_upload_bytes: 1024 * 1024,
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
            Arc::new(RwLock::new(persisted)),
        )
        .await
        .unwrap();

        state
            .storage_backend("archive")
            .await
            .unwrap()
            .upload_file("archive.txt", Body::from("A"), Some(1), 1024, None)
            .await
            .unwrap();

        assert!(archive.join("archive.txt").is_file());
        assert!(!primary.join("archive.txt").exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn administrator_can_add_and_reconfigure_declared_local_paths() {
        let root = std::env::temp_dir().join(format!("ycloud-add-local-{}", uuid::Uuid::new_v4()));
        let primary = root.join("primary");
        let archive = root.join("archive");
        let backup = root.join("backup");
        tokio::fs::create_dir_all(&archive).await.unwrap();
        tokio::fs::create_dir_all(&backup).await.unwrap();
        let persisted = ConfigFile {
            max_upload_bytes: 1024 * 1024,
            ..ConfigFile::default()
        };
        let state = AppState::new(
            Config {
                bind_address: std::net::IpAddr::from([127, 0, 0, 1]),
                port: 18_473,
                storage_path: primary.clone(),
                local_mounts: LocalMountCatalog::new(
                    primary,
                    vec![
                        DeploymentLocalMount {
                            id: "archive-disk".into(),
                            name: "Archive disk".into(),
                            path: archive.clone(),
                        },
                        DeploymentLocalMount {
                            id: "backup-disk".into(),
                            name: "Backup disk".into(),
                            path: backup.clone(),
                        },
                    ],
                )
                .unwrap(),
                config_path: root.join("config.json"),
                max_upload_bytes: 1024 * 1024,
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
            Arc::new(RwLock::new(persisted)),
        )
        .await
        .unwrap();

        let storage_id = state
            .add_local_storage(
                archive.to_string_lossy().into_owned(),
                "Archive".into(),
                None,
                true,
                false,
            )
            .await
            .unwrap();
        assert!(state.backends.is_ready(&storage_id).await);
        assert!(state
            .config_file
            .read()
            .await
            .storage_instances
            .iter()
            .any(|instance| {
                matches!(
                    &instance.backend,
                    StorageBackendConfig::Local(settings)
                        if settings.mount_id == "archive-disk"
                )
            }));
        assert!(state
            .add_local_storage(
                archive.to_string_lossy().into_owned(),
                "Duplicate".into(),
                None,
                true,
                false,
            )
            .await
            .is_err());
        assert!(state
            .add_local_storage(
                root.join("not-declared").to_string_lossy().into_owned(),
                "Not declared".into(),
                None,
                true,
                false,
            )
            .await
            .is_err());

        state
            .update_local_storage(
                &storage_id,
                "Backup".into(),
                backup.to_string_lossy().into_owned(),
                Some(8 * 1024 * 1024),
            )
            .await
            .unwrap();
        let persisted = state.config_file.read().await;
        let updated = persisted
            .storage_instances
            .iter()
            .find(|instance| instance.id == storage_id)
            .unwrap();
        assert_eq!(updated.name, "Backup");
        assert!(matches!(
            &updated.backend,
            StorageBackendConfig::Local(settings)
                if settings.mount_id == "backup-disk"
                    && settings.capacity_limit_bytes == Some(8 * 1024 * 1024)
        ));
        drop(persisted);
        assert!(state.backends.is_ready(&storage_id).await);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

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
                local_mounts: crate::storage_catalog::LocalMountCatalog::new(
                    root.join("storage"),
                    Vec::new(),
                )
                .unwrap(),
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
        pending.pending_storage_instance = Some(crate::config::StorageInstanceConfig {
            id: "pending-test".into(),
            name: "Pending test".into(),
            enabled: true,
            allow_guest_access: false,
            backend: StorageBackendConfig::S3(S3StorageConfig {
                provider: S3Provider::AlibabaOss,
                endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
                bucket: "ycloud-test".into(),
                region: "cn-hangzhou".into(),
                prefix: "files/".into(),
                addressing_style: S3AddressingStyle::VirtualHosted,
                access_key_id: "credential-to-remove".into(),
                secret_access_key: "secret-to-remove".into(),
                capacity_limit_bytes: None,
            }),
        });
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

        let original_limit = state.config_file.read().await.max_upload_bytes;
        let deployment_limit = state.config.max_upload_bytes;
        let error = state
            .update_config(move |config| {
                config.max_upload_bytes = deployment_limit + 1;
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            state.config_file.read().await.max_upload_bytes,
            original_limit
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn storage_configuration_cannot_delete_default_or_referenced_instances() {
        let root =
            std::env::temp_dir().join(format!("ycloud-storage-delete-{}", uuid::Uuid::new_v4()));
        let state = AppState::new(
            Config {
                bind_address: std::net::IpAddr::from([127, 0, 0, 1]),
                port: 18_473,
                storage_path: root.join("storage"),
                local_mounts: crate::storage_catalog::LocalMountCatalog::new(
                    root.join("storage"),
                    Vec::new(),
                )
                .unwrap(),
                config_path: root.join("config.json"),
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

        let default_error = state.delete_storage("primary").await.unwrap_err();
        assert_eq!(default_error.status(), axum::http::StatusCode::CONFLICT);

        let mut configured = state.config_file.read().await.clone();
        configured.storage_instances.push(StorageInstanceConfig {
            id: "archive".into(),
            name: "Archive".into(),
            enabled: true,
            allow_guest_access: false,
            backend: StorageBackendConfig::S3(S3StorageConfig {
                provider: S3Provider::TencentCos,
                endpoint: "https://cos.ap-chengdu.myqcloud.com".into(),
                bucket: "ycloud-test-1250000000".into(),
                region: "ap-chengdu".into(),
                prefix: "files/".into(),
                addressing_style: S3AddressingStyle::VirtualHosted,
                access_key_id: "test-access-key".into(),
                secret_access_key: "test-secret-key".into(),
                capacity_limit_bytes: None,
            }),
        });
        configured.shares.push(Share {
            id: "share-1".into(),
            storage_id: "archive".into(),
            name: "archive".into(),
            path: String::new(),
            username: None,
            webdav_enabled: false,
            password_hash: None,
            readonly: true,
        });
        *state.config_file.write().await = configured;

        let referenced_error = state.delete_storage("archive").await.unwrap_err();
        assert_eq!(referenced_error.status(), axum::http::StatusCode::CONFLICT);
        assert!(state
            .config_file
            .read()
            .await
            .storage_instances
            .iter()
            .any(|storage| storage.id == "archive"));

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
