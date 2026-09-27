//! Only connection recovery responsibility, not a user-facing task system.
use super::{secret_store::SecretStore, ConfigFile, S3StorageConfig, StorageBackendConfig};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

const CONTEXT: &str = "ycloud:retired-storage:v1";
const MAX_CONNECTIONS: usize = 64;
const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct RetiredStorage {
    config_path: PathBuf,
    entries: Arc<Mutex<Vec<Entry>>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub id: String,
    pub settings: S3StorageConfig,
    #[serde(skip)]
    pub runtime: Option<crate::storage_backend::StorageBackend>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    entries: Vec<Entry>,
}

pub(crate) fn same_namespace(a: &S3StorageConfig, b: &S3StorageConfig) -> bool {
    a.endpoint == b.endpoint && a.bucket == b.bucket && a.prefix == b.prefix
}

fn same_connection(a: &S3StorageConfig, b: &S3StorageConfig) -> bool {
    let mut a = a.clone();
    a.capacity_limit_bytes = b.capacity_limit_bytes;
    a.relay_upload = b.relay_upload;
    a == *b
}

impl RetiredStorage {
    pub async fn load(config_path: &Path) -> anyhow::Result<Self> {
        let path = path(config_path);
        let entries = match tokio::fs::metadata(&path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.is_file() && metadata.len() <= MAX_BYTES,
                    "Invalid retired storage record size"
                );
                let sealed = tokio::fs::read_to_string(&path).await?;
                let plaintext = SecretStore::for_read(config_path)
                    .await?
                    .open(CONTEXT, &sealed)?;
                let record: Record = serde_json::from_str(&plaintext)?;
                anyhow::ensure!(
                    record.version == 1 && record.entries.len() <= MAX_CONNECTIONS,
                    "Invalid retired storage record"
                );
                record.entries
            }
        };
        Ok(Self {
            config_path: config_path.into(),
            entries: Arc::new(Mutex::new(entries)),
        })
    }

    /// Persist the original connection BEFORE publishing a config that loses
    /// it. A failed publication may leave an extra safe recovery reference.
    pub async fn remember_removed(&self, old: &ConfigFile, new: &ConfigFile) -> anyhow::Result<()> {
        let mut entries = self.entries.lock().await;
        let mut candidate = entries.clone();
        for instance in &old.storage_instances {
            let StorageBackendConfig::S3(settings) = &instance.backend else {
                continue;
            };
            let retained = new.storage_instances.iter().any(|next| {
                next.id == instance.id
                    && matches!(&next.backend,
                    StorageBackendConfig::S3(value) if same_connection(settings, value))
            });
            if retained
                || candidate
                    .iter()
                    .any(|entry| entry.id == instance.id && entry.settings == *settings)
            {
                continue;
            }
            anyhow::ensure!(
                candidate.len() < MAX_CONNECTIONS,
                "旧连接清理尚未完成，已达到 64 条上限；本次配置未修改"
            );
            candidate.push(Entry {
                id: instance.id.clone(),
                settings: settings.clone(),
                runtime: None,
            });
        }
        if candidate.len() != entries.len() {
            self.save(&candidate).await?;
            *entries = candidate;
        }
        Ok(())
    }

    pub async fn snapshot(&self) -> Vec<Entry> {
        self.entries.lock().await.clone()
    }

    pub async fn bind_runtime(
        &self,
        id: &str,
        settings: &S3StorageConfig,
        backend: crate::storage_backend::StorageBackend,
    ) {
        for entry in self
            .entries
            .lock()
            .await
            .iter_mut()
            .filter(|entry| entry.id == id && same_connection(&entry.settings, settings))
        {
            if entry.runtime.is_none() {
                entry.runtime = Some(backend.clone());
            }
        }
    }

    pub async fn finish(&self, entry: &Entry) -> anyhow::Result<()> {
        let mut entries = self.entries.lock().await;
        let mut candidate = entries.clone();
        candidate.retain(|value| value.id != entry.id || value.settings != entry.settings);
        self.save(&candidate).await?;
        *entries = candidate;
        Ok(())
    }

    pub async fn activation_allowed(
        &self,
        settings: &S3StorageConfig,
        current: &ConfigFile,
    ) -> bool {
        // An existing namespace is recovered under its live backend's gates.
        // A retired namespace cannot be re-added while its separate worker
        // owns recovery; this prevents adopting a new upload as abandoned.
        if current.storage_instances.iter().any(|item| {
            matches!(&item.backend,
            StorageBackendConfig::S3(value) if same_namespace(settings, value))
        }) {
            return true;
        }
        !self
            .entries
            .lock()
            .await
            .iter()
            .any(|entry| same_namespace(settings, &entry.settings))
    }

    async fn save(&self, entries: &[Entry]) -> anyhow::Result<()> {
        let plaintext = serde_json::to_string(&Record {
            version: 1,
            entries: entries.to_vec(),
        })?;
        let sealed = SecretStore::for_write(&self.config_path)
            .await?
            .seal(CONTEXT, &plaintext)?;
        anyhow::ensure!(
            sealed.len() as u64 <= MAX_BYTES,
            "Retired storage record is too large"
        );
        let path = path(&self.config_path);
        tokio::fs::create_dir_all(path.parent().expect("private record parent")).await?;
        let bytes = sealed.into_bytes();
        tokio::task::spawn_blocking(move || {
            let outcome = super::commit::publish(&path, &bytes, true, false, |_| Ok(()))?;
            anyhow::ensure!(
                outcome.durability == super::ConfigDurability::Confirmed,
                "旧连接恢复记录未确认持久化；配置未修改"
            );
            Ok(())
        })
        .await?
    }
}

fn path(config: &Path) -> PathBuf {
    config
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(".ycloud-system")
        .join("retired-storage.enc")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{S3AddressingStyle, S3Provider, StorageInstanceConfig};

    fn settings() -> S3StorageConfig {
        S3StorageConfig {
            provider: S3Provider::S3Compatible,
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "fixture-bucket".into(),
            region: "us-east-1".into(),
            prefix: "isolated/".into(),
            addressing_style: S3AddressingStyle::Path,
            access_key_id: "fixture-access-key".into(),
            secret_access_key: "fixture-secret-key".into(),
            capacity_limit_bytes: None,
            relay_upload: false,
        }
    }

    #[tokio::test]
    async fn retired_connection_survives_reload_encrypted_and_is_removed_only_after_confirmation() {
        let directory = crate::test_support::TestDirectory::new("retired-storage");
        let config_path = directory.path().join("config.json");
        let mut old = super::super::load_config(&config_path).await.unwrap();
        let settings = settings();
        old.storage_instances.push(StorageInstanceConfig {
            id: "storage-test".into(),
            name: "test".into(),
            enabled: true,
            allow_guest_access: false,
            allow_guest_download: Some(false),
            backend: StorageBackendConfig::S3(settings.clone()),
        });
        let mut next = old.clone();
        next.storage_instances.clear();
        let store = RetiredStorage::load(&config_path).await.unwrap();
        store.remember_removed(&old, &next).await.unwrap();
        store.remember_removed(&old, &next).await.unwrap();
        let bytes = tokio::fs::read_to_string(path(&config_path)).await.unwrap();
        assert!(!bytes.contains(&settings.secret_access_key));
        assert!(!bytes.contains(&settings.access_key_id));
        assert!(!bytes.contains(&settings.endpoint));
        let restarted = RetiredStorage::load(&config_path).await.unwrap();
        assert_eq!(restarted.snapshot().await.len(), 1);
        assert!(!restarted.activation_allowed(&settings, &next).await);
        assert!(restarted.activation_allowed(&settings, &old).await);
        restarted
            .finish(&restarted.snapshot().await[0])
            .await
            .unwrap();
        assert!(RetiredStorage::load(&config_path)
            .await
            .unwrap()
            .snapshot()
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn capacity_and_relay_changes_do_not_create_retired_connections() {
        let directory = crate::test_support::TestDirectory::new("retired-storage-policy");
        let config_path = directory.path().join("config.json");
        let mut old = super::super::load_config(&config_path).await.unwrap();
        old.storage_instances.push(StorageInstanceConfig {
            id: "storage-test".into(),
            name: "test".into(),
            enabled: true,
            allow_guest_access: false,
            allow_guest_download: Some(false),
            backend: StorageBackendConfig::S3(settings()),
        });
        let mut next = old.clone();
        let StorageBackendConfig::S3(changed) = &mut next.storage_instances[0].backend else {
            panic!()
        };
        changed.capacity_limit_bytes = Some(1024);
        changed.relay_upload = true;
        let store = RetiredStorage::load(&config_path).await.unwrap();
        store.remember_removed(&old, &next).await.unwrap();
        assert!(store.snapshot().await.is_empty());
    }
}
