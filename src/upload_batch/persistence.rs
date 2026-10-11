//! Small, bounded upload ticket index. It does not store file payloads or
//! authentication tokens; a caller still needs a current account credential.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::{
    operation_id, UploadBatch, UploadItem, UploadStatus, MAX_RETAINED_BATCHES,
    MAX_RETAINED_BATCH_ITEMS,
};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState, OperationOutcome},
    storage::StorageService,
};

const VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ITEM_BYTES: u64 = 16 * 1024;
const MAX_RESERVED_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct BatchPersistence {
    root: PathBuf,
    pub(super) reservations: std::sync::Mutex<HashMap<String, u64>>,
    #[cfg(test)]
    pub(super) sync_failure: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(super) remove_failure: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(super) update_pause:
        std::sync::Mutex<Option<(String, std::sync::Arc<tokio::sync::Mutex<()>>)>>,
}

pub(super) struct PreparedBatch {
    bytes: Vec<u8>,
    reserved: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    ticket: String,
    account: String,
    storage_id: String,
    #[serde(default)]
    namespace_id: Option<String>,
    expires_unix: i64,
    items: Vec<ManifestItem>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestItem {
    path: String,
    request_path: String,
    size: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedItem {
    version: u32,
    path: String,
    status: UploadStatus,
    operation: Option<SavedOutcome>,
    expires_unix: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedOutcome {
    commit: CommitState,
    cleanup: CleanupState,
}

impl From<OperationOutcome> for SavedOutcome {
    fn from(value: OperationOutcome) -> Self {
        Self {
            commit: value.commit,
            cleanup: value.cleanup,
        }
    }
}

impl From<SavedOutcome> for OperationOutcome {
    fn from(value: SavedOutcome) -> Self {
        Self::new(value.commit, value.cleanup)
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn index_error(error: impl Into<anyhow::Error>) -> AppError {
    AppError::with_source("upload result index is unavailable", error)
}

fn validate_ticket(ticket: &str) -> AppResult<()> {
    let id = uuid::Uuid::parse_str(ticket).map_err(|_| AppError::Forbidden)?;
    if id.get_version_num() != 4 || id.to_string() != ticket {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

fn valid_namespace(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_private_dir(path: &Path) -> AppResult<()> {
    let meta = std::fs::symlink_metadata(path).map_err(index_error)?;
    if !meta.is_dir() || crate::storage::is_link_or_reparse_point(&meta) {
        return Err(AppError::Conflict(
            "upload result index path is not a private directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(index_error)?;
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> AppResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => validate_private_dir(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(path)
                    .map_err(index_error)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir(path).map_err(index_error)?;
            validate_private_dir(path)
        }
        Err(error) => Err(index_error(error)),
    }
}

fn read_bounded(path: &Path, maximum: u64) -> AppResult<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(index_error)?;
    if !meta.is_file() || crate::storage::is_link_or_reparse_point(&meta) || meta.len() > maximum {
        return Err(AppError::Conflict(
            "invalid upload result index record".into(),
        ));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(index_error)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(index_error)?;
    if bytes.len() as u64 > maximum {
        return Err(AppError::Conflict(
            "upload result index record too large".into(),
        ));
    }
    Ok(bytes)
}

fn estimate(manifest: &Manifest, bytes: usize) -> AppResult<u64> {
    let sidecars = manifest.items.iter().try_fold(0_u64, |total, item| {
        total
            .checked_add(item.path.len() as u64 + 512)
            .ok_or(AppError::TooManyRequests)
    })?;
    (bytes as u64)
        .checked_add(sidecars)
        .ok_or(AppError::TooManyRequests)
}

impl BatchPersistence {
    pub(super) async fn load(
        config_path: &Path,
        ttl: Duration,
    ) -> AppResult<(Self, HashMap<String, UploadBatch>)> {
        let root = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".ycloud-system")
            .join("upload-results");
        tokio::task::spawn_blocking(move || {
            create_private_dir(root.parent().expect("result index parent"))?;
            create_private_dir(&root)?;
            let mut batches = HashMap::new();
            let mut reserved = 0_u64;
            let mut reservations = HashMap::new();
            let mut total_items = 0_usize;
            let now = now_unix();
            for entry in std::fs::read_dir(&root).map_err(index_error)? {
                let entry = entry.map_err(index_error)?;
                let ticket = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| AppError::Forbidden)?;
                validate_ticket(&ticket)?;
                let directory = entry.path();
                validate_private_dir(&directory)?;
                let bytes = match read_bounded(&directory.join("manifest.json"), MAX_MANIFEST_BYTES)
                {
                    Ok(bytes) => bytes,
                    Err(AppError::Internal { .. }) if !directory.join("manifest.json").exists() => {
                        std::fs::remove_dir_all(&directory).map_err(index_error)?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let manifest: Manifest = serde_json::from_slice(&bytes).map_err(index_error)?;
                if manifest.version != VERSION
                    || manifest.ticket != ticket
                    || manifest.items.len() > MAX_RETAINED_BATCH_ITEMS
                    || manifest.account.is_empty()
                    || manifest.storage_id.is_empty()
                    || manifest
                        .namespace_id
                        .as_deref()
                        .is_some_and(|value| !valid_namespace(value))
                {
                    return Err(AppError::Conflict(
                        "invalid upload result index manifest".into(),
                    ));
                }
                let mut items = HashMap::new();
                let mut expires_unix = manifest.expires_unix;
                for record in &manifest.items {
                    if StorageService::normalize_relative(&record.path)? != record.path
                        || items.contains_key(&record.path)
                    {
                        return Err(AppError::Conflict(
                            "invalid upload result index path".into(),
                        ));
                    }
                    let item_path =
                        directory.join(format!("{}.json", operation_id(&ticket, &record.path)));
                    let saved = match std::fs::symlink_metadata(&item_path) {
                        Ok(_) => {
                            let saved: SavedItem =
                                serde_json::from_slice(&read_bounded(&item_path, MAX_ITEM_BYTES)?)
                                    .map_err(index_error)?;
                            if saved.version != VERSION || saved.path != record.path {
                                return Err(AppError::Conflict(
                                    "upload result index item identity mismatch".into(),
                                ));
                            }
                            Some(saved)
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                        Err(error) => return Err(index_error(error)),
                    };
                    if let Some(saved) = &saved {
                        expires_unix = expires_unix.max(saved.expires_unix);
                    }
                    let status = saved
                        .as_ref()
                        .map(|value| value.status)
                        .unwrap_or(UploadStatus::Pending)
                        .after_restart();
                    items.insert(
                        record.path.clone(),
                        UploadItem {
                            request_path: record.request_path.clone(),
                            size: record.size,
                            status,
                            operation: saved.and_then(|value| value.operation.map(Into::into)),
                        },
                    );
                }
                let retained =
                    expires_unix > now || items.values().any(|item| item.status.needs_recovery());
                if !retained {
                    std::fs::remove_dir_all(&directory).map_err(index_error)?;
                    continue;
                }
                total_items = total_items
                    .checked_add(manifest.items.len())
                    .ok_or(AppError::TooManyRequests)?;
                if total_items > MAX_RETAINED_BATCH_ITEMS {
                    return Err(AppError::TooManyRequests);
                }
                let charged = estimate(&manifest, bytes.len())?;
                reserved = reserved
                    .checked_add(charged)
                    .ok_or(AppError::TooManyRequests)?;
                reservations.insert(ticket.clone(), charged);
                if reserved > MAX_RESERVED_BYTES || batches.len() >= MAX_RETAINED_BATCHES {
                    return Err(AppError::TooManyRequests);
                }
                let remaining = u64::try_from(expires_unix.saturating_sub(now)).unwrap_or(u64::MAX);
                batches.insert(
                    ticket,
                    UploadBatch {
                        transition: std::sync::Arc::new(tokio::sync::Mutex::new(())),
                        retirement: super::BatchRetirement::Active,
                        recovery_backend: None,
                        subject: None,
                        account: manifest.account,
                        storage_id: manifest.storage_id,
                        namespace_id: manifest.namespace_id,
                        expires_at: Instant::now()
                            + Duration::from_secs(remaining.min(ttl.as_secs().max(24 * 60 * 60))),
                        expires_unix,
                        items,
                    },
                );
            }
            Ok((
                Self {
                    root,
                    reservations: std::sync::Mutex::new(reservations),
                    #[cfg(test)]
                    sync_failure: Default::default(),
                    #[cfg(test)]
                    remove_failure: Default::default(),
                    #[cfg(test)]
                    update_pause: std::sync::Mutex::new(None),
                },
                batches,
            ))
        })
        .await
        .map_err(index_error)?
    }

    pub(super) fn prepare(ticket: &str, batch: &UploadBatch) -> AppResult<PreparedBatch> {
        validate_ticket(ticket)?;
        if batch
            .namespace_id
            .as_deref()
            .is_some_and(|value| !valid_namespace(value))
        {
            return Err(AppError::BadRequest(
                "invalid upload storage identity".into(),
            ));
        }
        let manifest = Manifest {
            version: VERSION,
            ticket: ticket.into(),
            account: batch.account.clone(),
            storage_id: batch.storage_id.clone(),
            namespace_id: batch.namespace_id.clone(),
            expires_unix: batch.expires_unix,
            items: batch
                .items
                .iter()
                .map(|(path, item)| ManifestItem {
                    path: path.clone(),
                    request_path: item.request_path.clone(),
                    size: item.size,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&manifest).map_err(index_error)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(AppError::TooManyRequests);
        }
        let reserved = estimate(&manifest, bytes.len())?;
        Ok(PreparedBatch { bytes, reserved })
    }

    pub(super) async fn create(&self, ticket: &str, prepared: PreparedBatch) -> AppResult<()> {
        validate_ticket(ticket)?;
        let PreparedBatch { bytes, reserved } = prepared;
        {
            let mut reservations = self
                .reservations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if reservations.contains_key(ticket) {
                return Err(AppError::Conflict(
                    "upload result ticket already reserved".into(),
                ));
            }
            if reservations.values().sum::<u64>().saturating_add(reserved) > MAX_RESERVED_BYTES {
                return Err(AppError::TooManyRequests);
            }
            reservations.insert(ticket.to_owned(), reserved);
        }
        let root = self.root.clone();
        let token = ticket.to_owned();
        #[cfg(test)]
        let sync_failure = self.sync_failure.load(std::sync::atomic::Ordering::Relaxed);
        let result = tokio::task::spawn_blocking(move || {
            let directory = root.join(token);
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                let mut builder = builder;
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
                builder
            };
            if let Err(error) = builder.create(&directory) {
                return (false, Err(index_error(error)));
            }
            let result = validate_private_dir(&directory).and_then(|()| {
                let commit = crate::config::commit::publish(
                    &directory.join("manifest.json"),
                    &bytes,
                    false,
                    false,
                    |_stage| {
                        #[cfg(test)]
                        if sync_failure
                            && _stage == crate::config::commit::CommitStage::SyncPublished
                        {
                            return Err(std::io::Error::other("injected directory sync failure"));
                        }
                        Ok(())
                    },
                )
                .map_err(index_error)?;
                if commit.durability == crate::config::ConfigDurability::Unconfirmed {
                    return Err(index_error(anyhow::anyhow!(
                        "upload index publication durability is unconfirmed"
                    )));
                }
                Ok(())
            });
            (true, result)
        })
        .await
        .map_err(index_error)?;
        if !result.0 {
            self.reservations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(ticket);
        }
        // A created directory remains owned and charged even if publication
        // failed. The batch lifecycle, not this error path, destroys it.
        result.1
    }

    pub(super) async fn update(
        &self,
        ticket: &str,
        path: &str,
        status: UploadStatus,
        operation: Option<OperationOutcome>,
        expires_unix: i64,
    ) -> AppResult<crate::config::ConfigCommit> {
        validate_ticket(ticket)?;
        #[cfg(test)]
        let _pause = {
            let gate = self
                .update_pause
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(paused, _)| paused == ticket)
                .map(|(_, gate)| gate.clone());
            match gate {
                Some(gate) => Some(gate.lock_owned().await),
                None => None,
            }
        };
        let item = SavedItem {
            version: VERSION,
            path: path.into(),
            status,
            operation: operation.map(Into::into),
            expires_unix,
        };
        let bytes = serde_json::to_vec(&item).map_err(index_error)?;
        if bytes.len() as u64 > MAX_ITEM_BYTES {
            return Err(AppError::TooManyRequests);
        }
        let target = self
            .root
            .join(ticket)
            .join(format!("{}.json", operation_id(ticket, path)));
        #[cfg(test)]
        let sync_failure = self.sync_failure.load(std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            crate::config::commit::publish(&target, &bytes, false, false, |_stage| {
                #[cfg(test)]
                if sync_failure && _stage == crate::config::commit::CommitStage::SyncPublished {
                    return Err(std::io::Error::other("injected directory sync failure"));
                }
                Ok(())
            })
            .map_err(index_error)
        })
        .await
        .map_err(index_error)?
    }

    pub(super) async fn remove(&self, ticket: &str) -> AppResult<()> {
        validate_ticket(ticket)?;
        #[cfg(test)]
        if self
            .remove_failure
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(index_error(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected removal failure",
            )));
        }
        if !self
            .reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(ticket)
        {
            return Ok(());
        }
        let root = self.root.clone();
        let path = root.join(ticket);
        tokio::task::spawn_blocking(move || {
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(index_error(error)),
                Ok(_) => {
                    validate_private_dir(&path)?;
                    std::fs::remove_dir_all(&path).map_err(index_error)?;
                }
            }
            #[cfg(unix)]
            std::fs::File::open(root)
                .and_then(|file| file.sync_all())
                .map_err(index_error)?;
            Ok::<(), AppError>(())
        })
        .await
        .map_err(index_error)??;
        self.reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(ticket);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn failed_creation_keeps_owned_disk_budget_until_destroyed() {
        let directory = crate::test_support::TestDirectory::new("index-create-durability");
        let (persistence, _) = BatchPersistence::load(
            &directory.path().join("config.json"),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        let ticket = uuid::Uuid::new_v4().to_string();
        persistence.sync_failure.store(true, Ordering::Relaxed);
        assert!(persistence
            .create(
                &ticket,
                PreparedBatch {
                    bytes: b"{}".to_vec(),
                    reserved: 100
                }
            )
            .await
            .is_err());
        assert_eq!(
            persistence.reservations.lock().unwrap().get(&ticket),
            Some(&100)
        );
        assert!(persistence
            .root
            .join(&ticket)
            .join("manifest.json")
            .exists());
        persistence.remove(&ticket).await.unwrap();
        assert!(persistence.reservations.lock().unwrap().is_empty());
        assert!(!persistence.root.join(ticket).exists());
    }

    #[tokio::test]
    async fn retirement_does_not_need_a_manifest_and_settles_only_once() {
        let directory = crate::test_support::TestDirectory::new("index-partial-retirement");
        let (persistence, _) = BatchPersistence::load(
            &directory.path().join("config.json"),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        for ticket in [&first, &second] {
            persistence
                .create(
                    ticket,
                    PreparedBatch {
                        bytes: b"{}".to_vec(),
                        reserved: 100,
                    },
                )
                .await
                .unwrap();
        }
        tokio::fs::remove_file(persistence.root.join(&first).join("manifest.json"))
            .await
            .unwrap();
        persistence.remove(&first).await.unwrap();
        persistence.remove(&first).await.unwrap();
        assert_eq!(
            persistence
                .reservations
                .lock()
                .unwrap()
                .values()
                .sum::<u64>(),
            100
        );
        // A directory already gone must also settle its still-owned reservation.
        tokio::fs::remove_dir_all(persistence.root.join(&second))
            .await
            .unwrap();
        persistence.remove(&second).await.unwrap();
        assert!(persistence.reservations.lock().unwrap().is_empty());
    }
}
