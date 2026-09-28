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
    reserved_bytes: std::sync::Mutex<u64>,
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
                    let status = match saved
                        .as_ref()
                        .map(|value| value.status)
                        .unwrap_or(UploadStatus::Pending)
                    {
                        UploadStatus::Pending => UploadStatus::Cancelled,
                        UploadStatus::InProgress => UploadStatus::Unknown,
                        other => other,
                    };
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
                let retained = expires_unix > now
                    || items
                        .values()
                        .any(|item| matches!(item.status, UploadStatus::Unknown));
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
                reserved = reserved
                    .checked_add(estimate(&manifest, bytes.len())?)
                    .ok_or(AppError::TooManyRequests)?;
                if reserved > MAX_RESERVED_BYTES || batches.len() >= MAX_RETAINED_BATCHES {
                    return Err(AppError::TooManyRequests);
                }
                let remaining = u64::try_from(expires_unix.saturating_sub(now)).unwrap_or(u64::MAX);
                batches.insert(
                    ticket,
                    UploadBatch {
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
                    reserved_bytes: std::sync::Mutex::new(reserved),
                },
                batches,
            ))
        })
        .await
        .map_err(index_error)?
    }

    pub(super) async fn create(&self, ticket: &str, batch: &UploadBatch) -> AppResult<()> {
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
        {
            let mut budget = self
                .reserved_bytes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if budget.saturating_add(reserved) > MAX_RESERVED_BYTES {
                return Err(AppError::TooManyRequests);
            }
            *budget += reserved;
        }
        let root = self.root.clone();
        let ticket = ticket.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let directory = root.join(ticket);
            if directory.exists() {
                return Err(AppError::Conflict(
                    "upload result ticket already exists".into(),
                ));
            }
            create_private_dir(&directory)?;
            let result =
                crate::config::publish_traffic_snapshot(&directory.join("manifest.json"), &bytes)
                    .map_err(index_error);
            if result.is_err() {
                let _ = std::fs::remove_dir_all(&directory);
            }
            result
        })
        .await
        .map_err(index_error);
        if !matches!(result, Ok(Ok(()))) {
            *self
                .reserved_bytes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) -= reserved;
        }
        result?
    }

    pub(super) async fn update(
        &self,
        ticket: &str,
        path: &str,
        status: UploadStatus,
        operation: Option<OperationOutcome>,
        expires_unix: i64,
    ) -> AppResult<()> {
        validate_ticket(ticket)?;
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
        tokio::task::spawn_blocking(move || {
            crate::config::publish_traffic_snapshot(&target, &bytes).map_err(index_error)
        })
        .await
        .map_err(index_error)?
    }

    pub(super) async fn remove(&self, ticket: &str, batch: &UploadBatch) -> AppResult<()> {
        validate_ticket(ticket)?;
        let path = self.root.join(ticket);
        let bytes = read_bounded(&path.join("manifest.json"), MAX_MANIFEST_BYTES)?;
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(index_error)?;
        if manifest.ticket != ticket || manifest.account != batch.account {
            return Err(AppError::Forbidden);
        }
        let reserved = estimate(&manifest, bytes.len())?;
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path).map_err(index_error))
            .await
            .map_err(index_error)??;
        let mut budget = self
            .reserved_bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *budget = budget.saturating_sub(reserved);
        Ok(())
    }
}
