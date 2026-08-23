use axum::{body::Body, http::HeaderMap, response::Response};
use futures_util::StreamExt;
use std::sync::Arc;

use tokio::{
    fs,
    sync::{RwLock, RwLockWriteGuard},
};

use crate::{
    capacity::{CapacityStatus, CapacityTracker},
    error::{AppError, AppResult},
    s3_backend::S3Backend,
    storage::{is_link_or_reparse_point, FileResponseMode, StorageService},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendEntry {
    pub name: String,
    pub relative: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_unix: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendMetadata {
    pub is_dir: bool,
    pub size: u64,
    pub modified_unix: Option<i64>,
    pub content_type: Option<String>,
    pub version_tag: Option<String>,
}

/// One live storage boundary shared by every HTTP, WebDAV and archive entry
/// point. A backend replacement takes the write side of this lock, so uploads
/// and namespace mutations that already started drain before new traffic can
/// observe the replacement.
#[derive(Clone)]
pub struct StorageBackend {
    active: Arc<RwLock<ActiveStorage>>,
}

#[derive(Clone)]
enum StorageBackendKind {
    Local(StorageService),
    S3(S3Backend),
}

#[derive(Clone)]
struct ActiveStorage {
    kind: StorageBackendKind,
    capacity: CapacityTracker,
}

pub struct StorageReplacementGuard<'a> {
    active: RwLockWriteGuard<'a, ActiveStorage>,
}

impl StorageReplacementGuard<'_> {
    pub fn replace_with_local(
        &mut self,
        storage: StorageService,
        capacity_limit: Option<u64>,
        used: u64,
    ) {
        *self.active = ActiveStorage {
            kind: StorageBackendKind::Local(storage),
            capacity: CapacityTracker::new(capacity_limit, used),
        };
    }

    pub fn replace_with_s3(&mut self, storage: S3Backend, capacity_limit: Option<u64>, used: u64) {
        *self.active = ActiveStorage {
            kind: StorageBackendKind::S3(storage),
            capacity: CapacityTracker::new(capacity_limit, used),
        };
    }
}

impl StorageBackend {
    pub fn local(storage: StorageService) -> Self {
        Self {
            active: Arc::new(RwLock::new(ActiveStorage {
                kind: StorageBackendKind::Local(storage),
                capacity: CapacityTracker::new(None, 0),
            })),
        }
    }

    pub async fn local_configured(
        storage: StorageService,
        capacity_limit: Option<u64>,
    ) -> AppResult<Self> {
        let used = storage.user_data_size().await?;
        Ok(Self {
            active: Arc::new(RwLock::new(ActiveStorage {
                kind: StorageBackendKind::Local(storage),
                capacity: CapacityTracker::new(capacity_limit, used),
            })),
        })
    }

    pub async fn s3_configured(storage: S3Backend, capacity_limit: Option<u64>) -> AppResult<Self> {
        let used = storage.user_data_size().await?;
        Ok(Self {
            active: Arc::new(RwLock::new(ActiveStorage {
                kind: StorageBackendKind::S3(storage),
                capacity: CapacityTracker::new(capacity_limit, used),
            })),
        })
    }

    pub async fn begin_replacement(&self) -> StorageReplacementGuard<'_> {
        StorageReplacementGuard {
            active: self.active.write().await,
        }
    }

    pub async fn metadata(&self, relative: &str) -> AppResult<BackendMetadata> {
        let active = self.active.read().await;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                let metadata = storage.metadata(&path).await?;
                Ok(BackendMetadata {
                    is_dir: metadata.is_dir(),
                    size: metadata.len(),
                    modified_unix: metadata.modified().ok().map(|value| {
                        let value: chrono::DateTime<chrono::Utc> = value.into();
                        value.timestamp()
                    }),
                    content_type: None,
                    version_tag: None,
                })
            }
            StorageBackendKind::S3(storage) => {
                let metadata = storage.metadata(relative).await?;
                Ok(BackendMetadata {
                    is_dir: metadata.is_dir,
                    size: metadata.size,
                    modified_unix: metadata.last_modified,
                    content_type: metadata.content_type,
                    version_tag: metadata.etag,
                })
            }
        }
    }

    pub async fn list_directory(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<(Vec<BackendEntry>, bool)> {
        let active = self.active.read().await;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                list_local_directory(storage, relative, max_entries).await
            }
            StorageBackendKind::S3(storage) => {
                let result = storage.list_directory(relative, max_entries).await?;
                Ok((
                    result
                        .entries
                        .into_iter()
                        .map(|entry| BackendEntry {
                            name: entry.name,
                            relative: entry.relative,
                            is_dir: entry.is_dir,
                            size: entry.size,
                            modified_unix: entry.last_modified,
                        })
                        .collect(),
                    result.truncated,
                ))
            }
        }
    }

    pub async fn stream_file(
        &self,
        relative: &str,
        headers: &HeaderMap,
        mode: FileResponseMode,
    ) -> AppResult<Response> {
        let active = self.active.read().await;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                storage.stream_file(&path, headers, mode).await
            }
            StorageBackendKind::S3(storage) => storage.stream_file(relative, headers, mode).await,
        }
    }

    pub async fn upload_file(
        &self,
        relative: &str,
        body: Body,
        expected_bytes: Option<u64>,
        max_upload_bytes: u64,
        content_type: Option<&str>,
    ) -> AppResult<u64> {
        let active = self.active.read().await;
        let capacity = active.capacity.clone();
        if expected_bytes.is_some_and(|bytes| bytes > max_upload_bytes) {
            return Err(AppError::PayloadTooLarge);
        }
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let destination = storage.resolve_for_write(relative).await?;
                let old_size = match storage.metadata(&destination).await {
                    Ok(metadata) if metadata.is_file() => metadata.len(),
                    Ok(_) => return Err(AppError::Conflict("不能用文件覆盖目录".into())),
                    Err(AppError::NotFound) => 0,
                    Err(error) => return Err(error),
                };
                let mut capacity_reservation =
                    capacity.reserve_replacement(old_size, expected_bytes.unwrap_or(0))?;
                let mut writer = match expected_bytes {
                    Some(bytes) => {
                        storage
                            .begin_atomic_write_with_expected(relative, bytes)
                            .await?
                    }
                    None => storage.begin_atomic_write(relative).await?,
                };
                let mut stream = body.into_data_stream();
                let mut received = 0_u64;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk
                        .map_err(|error| AppError::with_source("failed to read upload", error))?;
                    received = received
                        .checked_add(chunk.len() as u64)
                        .ok_or(AppError::PayloadTooLarge)?;
                    capacity_reservation.ensure_new_size(received)?;
                    writer.write_chunk(&chunk).await?;
                }
                let committed = match writer.commit().await {
                    Ok(committed) => committed,
                    Err(error) => {
                        drop(capacity_reservation);
                        reconcile_local_capacity(&capacity, storage).await;
                        return Err(error);
                    }
                };
                capacity_reservation.commit(committed.previous_size, committed.size);
                Ok(committed.size)
            }
            StorageBackendKind::S3(storage) => {
                let content_length = expected_bytes.ok_or_else(|| {
                    AppError::BadRequest(
                        "对象存储上传需要有效的 Content-Length，不能使用未知长度请求体".into(),
                    )
                })?;
                let old_size = match storage.metadata(relative).await {
                    Ok(metadata) if !metadata.is_dir => metadata.size,
                    Ok(_) => return Err(AppError::Conflict("不能用文件覆盖目录".into())),
                    Err(AppError::NotFound) => 0,
                    Err(error) => return Err(error),
                };
                let capacity_reservation =
                    capacity.reserve_replacement(old_size, content_length)?;
                let result = match storage
                    .upload_file(
                        relative,
                        body,
                        content_length,
                        max_upload_bytes,
                        content_type,
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        drop(capacity_reservation);
                        reconcile_s3_capacity(&capacity, storage).await;
                        return Err(error);
                    }
                };
                capacity_reservation.commit(result.previous_size, result.size);
                Ok(result.size)
            }
        }
    }

    pub async fn create_directory(&self, relative: &str) -> AppResult<()> {
        let active = self.active.read().await;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_for_write(relative).await?;
                storage.create_directory(&path).await
            }
            StorageBackendKind::S3(storage) => storage.create_directory(relative).await,
        }
    }

    pub async fn remove(&self, relative: &str) -> AppResult<()> {
        let active = self.active.read().await;
        let capacity = active.capacity.clone();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                let removed_size = match storage.remove(&path).await {
                    Ok(removed_size) => removed_size,
                    Err(error) => {
                        reconcile_local_capacity(&capacity, storage).await;
                        return Err(error);
                    }
                };
                capacity.remove_used(removed_size);
                Ok(())
            }
            StorageBackendKind::S3(storage) => {
                let metadata = storage.metadata(relative).await?;
                let removal = if metadata.is_dir {
                    storage.delete_directory(relative).await
                } else {
                    storage.delete_file(relative).await
                };
                let removed_size = match removal {
                    Ok(removed_size) => removed_size,
                    Err(error) => {
                        reconcile_s3_capacity(&capacity, storage).await;
                        return Err(error);
                    }
                };
                capacity.remove_used(removed_size);
                Ok(())
            }
        }
    }

    pub async fn move_path(&self, source: &str, destination: &str) -> AppResult<()> {
        let active = self.active.read().await;
        let capacity = active.capacity.clone();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let source = storage.resolve_existing(source).await?;
                let destination = storage.resolve_for_write(destination).await?;
                let result = storage.move_path(&source, &destination).await;
                if result.is_err() {
                    reconcile_local_capacity(&capacity, storage).await;
                }
                result
            }
            StorageBackendKind::S3(storage) => {
                let result = if storage.metadata(source).await?.is_dir {
                    storage.move_directory(source, destination).await
                } else {
                    storage.move_file(source, destination).await
                };
                if result.is_err() {
                    reconcile_s3_capacity(&capacity, storage).await;
                }
                result
            }
        }
    }

    pub async fn copy_path(&self, source: &str, destination: &str) -> AppResult<()> {
        let active = self.active.read().await;
        let capacity = active.capacity.clone();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let source = storage.resolve_existing(source).await?;
                let destination = storage.resolve_for_write(destination).await?;
                let copied_size = storage.path_size(&source).await?;
                let reservation = capacity.reserve_replacement(0, copied_size)?;
                let result = storage
                    .copy_path_with_expected_size(&source, &destination, copied_size)
                    .await;
                if let Err(error) = result {
                    drop(reservation);
                    reconcile_local_capacity(&capacity, storage).await;
                    return Err(error);
                }
                reservation.commit(0, copied_size);
                Ok(())
            }
            StorageBackendKind::S3(storage) => {
                let copied_size = storage.path_size(source).await?;
                let reservation = capacity.reserve_replacement(0, copied_size)?;
                let result = if storage.metadata(source).await?.is_dir {
                    storage
                        .copy_directory_with_expected_size(source, destination, copied_size)
                        .await
                } else {
                    storage
                        .copy_file_with_expected_size(source, destination, copied_size)
                        .await
                };
                if let Err(error) = result {
                    drop(reservation);
                    reconcile_s3_capacity(&capacity, storage).await;
                    return Err(error);
                }
                reservation.commit(0, copied_size);
                Ok(())
            }
        }
    }

    pub async fn capacity_status(&self) -> CapacityStatus {
        self.active.read().await.capacity.status()
    }

    pub async fn ready(&self) -> bool {
        let active = self.active.read().await;
        match &active.kind {
            StorageBackendKind::Local(storage) => storage.ready().await,
            StorageBackendKind::S3(storage) => storage.probe().await.is_ok(),
        }
    }
}

async fn reconcile_local_capacity(capacity: &CapacityTracker, storage: &StorageService) {
    match storage.user_data_size().await {
        Ok(used) => capacity.reconcile(used),
        Err(error) => {
            tracing::error!(%error, "failed to reconcile local capacity after a storage mutation error")
        }
    }
}

async fn reconcile_s3_capacity(capacity: &CapacityTracker, storage: &S3Backend) {
    match storage.user_data_size().await {
        Ok(used) => capacity.reconcile(used),
        Err(error) => {
            tracing::error!(%error, "failed to reconcile S3 capacity after a storage mutation error")
        }
    }
}

async fn list_local_directory(
    storage: &StorageService,
    relative: &str,
    max_entries: usize,
) -> AppResult<(Vec<BackendEntry>, bool)> {
    let directory = storage.resolve_existing(relative).await?;
    if !storage.metadata(&directory).await?.is_dir() {
        return Err(AppError::NotFound);
    }
    let mut read_dir = fs::read_dir(directory.absolute())
        .await
        .map_err(|error| AppError::with_source("failed to list directory", error))?;
    let mut entries = Vec::new();
    while entries.len() < max_entries {
        let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|error| AppError::with_source("failed to read directory entry", error))?
        else {
            break;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()).await else {
            tracing::warn!(path = %entry.path().display(), "skipping unreadable directory entry");
            continue;
        };
        if is_link_or_reparse_point(&metadata) {
            tracing::warn!(path = %entry.path().display(), "skipping symbolic link in storage directory");
            continue;
        }
        let entry_relative = if relative.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", relative.trim_end_matches('/'), name)
        };
        entries.push(BackendEntry {
            name,
            relative: entry_relative,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified_unix: metadata.modified().ok().map(|value| {
                let value: chrono::DateTime<chrono::Utc> = value.into();
                value.timestamp()
            }),
        });
    }
    let truncated = entries.len() == max_entries
        && read_dir
            .next_entry()
            .await
            .map_err(|error| AppError::with_source("failed to read directory entry", error))?
            .is_some();
    Ok((entries, truncated))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

    use super::StorageBackend;
    use crate::storage::StorageService;

    #[tokio::test]
    async fn local_capacity_counts_growth_copy_and_delete() {
        let root =
            std::env::temp_dir().join(format!("ycloud-backend-capacity-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 1, 100, 0)
            .await
            .unwrap();
        tokio::fs::write(root.join("existing.bin"), b"1234")
            .await
            .unwrap();
        let backend = StorageBackend::local_configured(storage, Some(5))
            .await
            .unwrap();

        let rejected = backend
            .upload_file("new.bin", Body::from("12"), Some(2), 1024, None)
            .await
            .unwrap_err();
        assert_eq!(
            rejected.status(),
            axum::http::StatusCode::INSUFFICIENT_STORAGE
        );

        backend
            .upload_file("existing.bin", Body::from("12345"), Some(5), 1024, None)
            .await
            .unwrap();
        assert_eq!(backend.capacity_status().await.used, 5);

        backend.remove("existing.bin").await.unwrap();
        backend
            .upload_file("new.bin", Body::from("12"), Some(2), 1024, None)
            .await
            .unwrap();
        backend.copy_path("new.bin", "copy.bin").await.unwrap();
        assert_eq!(backend.capacity_status().await.used, 4);
        assert!(backend.copy_path("new.bin", "third.bin").await.is_err());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
