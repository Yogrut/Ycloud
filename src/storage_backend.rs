use axum::{body::Body, http::HeaderMap, response::Response};
use futures_util::StreamExt;
use tokio::fs;

use crate::{
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
}

/// Read-side storage boundary used while local and S3 implementations are
/// migrated in independently verified stages. S3 activation remains blocked
/// until every write, WebDAV and archive entry point uses the same boundary.
#[derive(Clone)]
pub enum StorageBackend {
    Local(StorageService),
    S3(S3Backend),
}

impl StorageBackend {
    pub async fn metadata(&self, relative: &str) -> AppResult<BackendMetadata> {
        match self {
            Self::Local(storage) => {
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
                })
            }
            Self::S3(storage) => {
                let metadata = storage.metadata(relative).await?;
                Ok(BackendMetadata {
                    is_dir: metadata.is_dir,
                    size: metadata.size,
                    modified_unix: metadata.last_modified,
                    content_type: metadata.content_type,
                })
            }
        }
    }

    pub async fn list_directory(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<(Vec<BackendEntry>, bool)> {
        match self {
            Self::Local(storage) => list_local_directory(storage, relative, max_entries).await,
            Self::S3(storage) => {
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
        match self {
            Self::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                storage.stream_file(&path, headers, mode).await
            }
            Self::S3(storage) => storage.stream_file(relative, headers, mode).await,
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
        match self {
            Self::Local(storage) => {
                let mut writer = match expected_bytes {
                    Some(bytes) => {
                        storage
                            .begin_atomic_write_with_expected(relative, bytes)
                            .await?
                    }
                    None => storage.begin_atomic_write(relative).await?,
                };
                let mut stream = body.into_data_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk
                        .map_err(|error| AppError::with_source("failed to read upload", error))?;
                    writer.write_chunk(&chunk).await?;
                }
                writer.commit().await
            }
            Self::S3(storage) => {
                let content_length = expected_bytes.ok_or_else(|| {
                    AppError::BadRequest(
                        "对象存储上传需要有效的 Content-Length，不能使用未知长度请求体".into(),
                    )
                })?;
                storage
                    .upload_file(
                        relative,
                        body,
                        content_length,
                        max_upload_bytes,
                        content_type,
                    )
                    .await
                    .map(|result| result.size)
            }
        }
    }

    pub async fn create_directory(&self, relative: &str) -> AppResult<()> {
        match self {
            Self::Local(storage) => {
                let path = storage.resolve_for_write(relative).await?;
                storage.create_directory(&path).await
            }
            Self::S3(storage) => storage.create_directory(relative).await,
        }
    }

    pub async fn remove(&self, relative: &str) -> AppResult<()> {
        match self {
            Self::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                storage.remove(&path).await
            }
            Self::S3(storage) => {
                let metadata = storage.metadata(relative).await?;
                if metadata.is_dir {
                    storage.delete_empty_directory(relative).await
                } else {
                    storage.delete_file(relative).await
                }
            }
        }
    }

    pub async fn move_path(&self, source: &str, destination: &str) -> AppResult<()> {
        match self {
            Self::Local(storage) => {
                let source = storage.resolve_existing(source).await?;
                let destination = storage.resolve_for_write(destination).await?;
                storage.move_path(&source, &destination).await
            }
            Self::S3(storage) => {
                if storage.metadata(source).await?.is_dir {
                    return Err(AppError::Conflict("对象存储递归目录移动尚未开放".into()));
                }
                storage.move_file(source, destination).await
            }
        }
    }

    pub async fn copy_path(&self, source: &str, destination: &str) -> AppResult<()> {
        match self {
            Self::Local(storage) => {
                let source = storage.resolve_existing(source).await?;
                let destination = storage.resolve_for_write(destination).await?;
                storage.copy_path(&source, &destination).await
            }
            Self::S3(storage) => {
                if storage.metadata(source).await?.is_dir {
                    return Err(AppError::Conflict("对象存储递归目录复制尚未开放".into()));
                }
                storage.copy_file(source, destination).await
            }
        }
    }

    pub async fn ready(&self) -> bool {
        match self {
            Self::Local(storage) => storage.ready().await,
            Self::S3(storage) => storage.probe().await.is_ok(),
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
