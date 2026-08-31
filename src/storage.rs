use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use tokio::{
    fs::{self, OpenOptions},
    sync::{mpsc, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
};
use uuid::Uuid;

#[cfg(unix)]
use tokio::fs::File;

use crate::{
    error::{AppError, AppResult},
    storage_transaction::{remove_any, TransactionPaths, SYSTEM_DIR},
};

mod atomic_write;
mod path;
mod response;

pub use atomic_write::{AtomicFileWriter, AtomicWriteResult};
pub(crate) use path::is_link_or_reparse_point;
pub use path::ResolvedPath;
use path::{reject_root_or_descendant, require_plain_directory};
pub use response::FileResponseMode;
pub(crate) use response::{attachment_header, content_type_for_mode, parse_range};

#[derive(Clone)]
pub struct StorageService {
    root: Arc<PathBuf>,
    io_gate: Arc<Semaphore>,
    max_upload_bytes: Arc<AtomicU64>,
    max_list_entries: usize,
    disk_reserve_bytes: u64,
    reserved_upload_bytes: Arc<Mutex<u64>>,
    transactions: Arc<TransactionPaths>,
    mutation_gate: Arc<AsyncMutex<()>>,
    trash_notify: mpsc::UnboundedSender<()>,
}

impl StorageService {
    pub async fn new(
        root: PathBuf,
        max_upload_bytes: u64,
        io_concurrency: usize,
        max_list_entries: usize,
        disk_reserve_bytes: u64,
    ) -> AppResult<Self> {
        Self::initialize(
            root,
            max_upload_bytes,
            Arc::new(Semaphore::new(io_concurrency.max(1))),
            max_list_entries,
            disk_reserve_bytes,
            true,
        )
        .await
    }

    pub async fn open_declared(
        root: PathBuf,
        max_upload_bytes: u64,
        io_gate: Arc<Semaphore>,
        max_list_entries: usize,
        disk_reserve_bytes: u64,
    ) -> AppResult<Self> {
        Self::initialize(
            root,
            max_upload_bytes,
            io_gate,
            max_list_entries,
            disk_reserve_bytes,
            false,
        )
        .await
    }

    pub async fn new_with_io_gate(
        root: PathBuf,
        max_upload_bytes: u64,
        io_gate: Arc<Semaphore>,
        max_list_entries: usize,
        disk_reserve_bytes: u64,
    ) -> AppResult<Self> {
        Self::initialize(
            root,
            max_upload_bytes,
            io_gate,
            max_list_entries,
            disk_reserve_bytes,
            true,
        )
        .await
    }

    async fn initialize(
        root: PathBuf,
        max_upload_bytes: u64,
        io_gate: Arc<Semaphore>,
        max_list_entries: usize,
        disk_reserve_bytes: u64,
        create_root: bool,
    ) -> AppResult<Self> {
        if create_root {
            fs::create_dir_all(&root).await.map_err(|error| {
                AppError::with_source("failed to create storage directory", error)
            })?;
        }
        let root_metadata = fs::symlink_metadata(&root)
            .await
            .map_err(|error| AppError::with_source("declared local mount is unavailable", error))?;
        if !root_metadata.is_dir() || is_link_or_reparse_point(&root_metadata) {
            return Err(AppError::Conflict(
                "Declared local mount must be a plain directory".into(),
            ));
        }
        let root = fs::canonicalize(&root)
            .await
            .map_err(|error| AppError::with_source("failed to resolve storage directory", error))?;

        let transactions = TransactionPaths::initialize(&root).await?;
        let (trash_notify, mut trash_events) = mpsc::unbounded_channel();
        let trash = transactions.trash.clone();
        let cleaner_gate = io_gate.clone();
        tokio::spawn(async move {
            while trash_events.recv().await.is_some() {
                let Ok(_permit) = cleaner_gate.acquire().await else {
                    break;
                };
                purge_trash(&trash).await;
            }
        });
        Ok(Self {
            root: Arc::new(root),
            io_gate,
            max_upload_bytes: Arc::new(AtomicU64::new(max_upload_bytes)),
            max_list_entries: max_list_entries.max(1),
            disk_reserve_bytes,
            reserved_upload_bytes: Arc::new(Mutex::new(0)),
            transactions: Arc::new(transactions),
            mutation_gate: Arc::new(AsyncMutex::new(())),
            trash_notify,
        })
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_bytes.load(Ordering::Relaxed)
    }

    pub fn set_max_upload_bytes(&self, max_upload_bytes: u64) {
        self.max_upload_bytes
            .store(max_upload_bytes, Ordering::Relaxed);
    }

    pub fn max_list_entries(&self) -> usize {
        self.max_list_entries
    }

    pub async fn metadata(&self, path: &ResolvedPath) -> AppResult<std::fs::Metadata> {
        fs::metadata(path.absolute())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::with_source("failed to read file metadata", error),
            })
    }

    pub async fn remove(&self, path: &ResolvedPath) -> AppResult<u64> {
        if path.is_root() {
            return Err(AppError::BadRequest(
                "The storage root cannot be removed".into(),
            ));
        }
        let _permit = self.acquire_io().await?;
        let _mutation = self.mutation_gate.lock().await;
        fs::symlink_metadata(path.absolute())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::with_source("failed to inspect path", error),
            })?;
        let removed_size = self.path_size(path).await?;
        self.transactions.stage_delete(path.absolute()).await?;
        let _ = self.trash_notify.send(());
        Ok(removed_size)
    }

    pub async fn create_directory(&self, path: &ResolvedPath) -> AppResult<()> {
        let _permit = self.acquire_io().await?;
        let _mutation = self.mutation_gate.lock().await;
        fs::create_dir(path.absolute())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    AppError::Conflict("Destination already exists".into())
                }
                _ => AppError::with_source("failed to create directory", error),
            })?;
        sync_parent_directory(path.absolute()).await
    }

    pub async fn move_path(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
    ) -> AppResult<()> {
        reject_root_or_descendant(source, destination)?;
        let _permit = self.acquire_io().await?;
        let _mutation = self.mutation_gate.lock().await;
        if fs::try_exists(destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to inspect destination", error))?
        {
            return Err(AppError::Conflict("Destination already exists".into()));
        }
        require_plain_directory(destination.absolute().parent()).await?;
        fs::rename(source.absolute(), destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to move path", error))?;
        sync_parent_directory(source.absolute()).await?;
        sync_parent_directory(destination.absolute()).await
    }

    pub async fn copy_path(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
    ) -> AppResult<()> {
        let expected_size = self.path_size(source).await?;
        self.copy_path_with_expected_size(source, destination, expected_size)
            .await
    }

    pub async fn copy_path_with_expected_size(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
        expected_size: u64,
    ) -> AppResult<()> {
        reject_root_or_descendant(source, destination)?;
        let _permit = self.acquire_io().await?;
        let _mutation = self.mutation_gate.lock().await;
        if fs::try_exists(destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to inspect destination", error))?
        {
            return Err(AppError::Conflict("Destination already exists".into()));
        }

        let metadata = self.metadata(source).await?;
        let copy_size = self.path_size(source).await?;
        if copy_size != expected_size {
            return Err(AppError::Conflict(
                "Source changed while preparing the copy".into(),
            ));
        }
        let _physical_reservation = self.reserve_physical_bytes(copy_size).await?;
        let transaction_id = Uuid::new_v4().to_string();
        let temporary = self.transactions.copy_path(&transaction_id);
        let result = if metadata.is_dir() {
            copy_directory_iterative(source.absolute(), &temporary).await
        } else if metadata.is_file() {
            copy_file_synced(source.absolute(), &temporary).await
        } else {
            Err(AppError::Forbidden)
        };
        if let Err(error) = result {
            let _ = remove_any(&temporary).await;
            return Err(error);
        }
        require_plain_directory(destination.absolute().parent()).await?;
        if fs::try_exists(destination.absolute())
            .await
            .unwrap_or(false)
        {
            let _ = remove_any(&temporary).await;
            return Err(AppError::Conflict("Destination already exists".into()));
        }
        fs::rename(&temporary, destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to publish copied path", error))?;
        sync_parent_directory(destination.absolute()).await
    }

    pub async fn ready(&self) -> bool {
        fs::metadata(self.root())
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
    }

    /// Count only user-visible files. The reserved transaction directory is
    /// excluded because it contains temporary copies, backups and trash that
    /// must not consume the logical user quota twice.
    pub async fn user_data_size(&self) -> AppResult<u64> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || calculate_plain_path_size(&root, true))
            .await
            .map_err(|error| {
                AppError::with_source("failed to inspect local storage usage", error)
            })?
    }

    pub async fn path_size(&self, path: &ResolvedPath) -> AppResult<u64> {
        let absolute = path.absolute.clone();
        tokio::task::spawn_blocking(move || calculate_plain_path_size(&absolute, false))
            .await
            .map_err(|error| AppError::with_source("failed to inspect local path size", error))?
    }

    pub(crate) async fn acquire_io(&self) -> AppResult<OwnedSemaphorePermit> {
        self.io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))
    }
}

fn calculate_plain_path_size(path: &Path, skip_reserved_root_entry: bool) -> AppResult<u64> {
    let root_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| AppError::with_source("failed to inspect local storage usage", error))?;
    if is_link_or_reparse_point(&root_metadata) {
        return Err(AppError::Forbidden);
    }
    if root_metadata.is_file() {
        return Ok(root_metadata.len());
    }
    if !root_metadata.is_dir() {
        return Err(AppError::Forbidden);
    }

    let root = path.to_path_buf();
    let mut stack = vec![root.clone()];
    let mut total = 0_u64;
    while let Some(directory) = stack.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| AppError::with_source("failed to read local storage usage", error))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                AppError::with_source("failed to read local storage usage entry", error)
            })?;
            if skip_reserved_root_entry
                && directory == root
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(SYSTEM_DIR)
            {
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
                AppError::with_source("failed to inspect local storage usage entry", error)
            })?;
            if is_link_or_reparse_point(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| AppError::internal("local storage usage exceeds u64"))?;
            }
        }
    }
    Ok(total)
}

async fn copy_directory_iterative(source: &Path, destination: &Path) -> AppResult<()> {
    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((current_source, current_destination)) = pending.pop() {
        fs::create_dir(&current_destination)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    AppError::Conflict("Destination already exists".into())
                }
                _ => AppError::with_source("failed to create copied directory", error),
            })?;
        let mut entries = fs::read_dir(&current_source)
            .await
            .map_err(|error| AppError::with_source("failed to read copied directory", error))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| AppError::with_source("failed to read directory entry", error))?
        {
            let source_path = entry.path();
            let destination_path = current_destination.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path)
                .await
                .map_err(|error| AppError::with_source("failed to inspect copied entry", error))?;
            if is_link_or_reparse_point(&metadata) {
                return Err(AppError::Forbidden);
            }
            if metadata.is_dir() {
                pending.push((source_path, destination_path));
            } else if metadata.is_file() {
                copy_file_synced(&source_path, &destination_path).await?;
            }
        }
    }
    Ok(())
}

async fn copy_file_synced(source: &Path, destination: &Path) -> AppResult<()> {
    fs::copy(source, destination)
        .await
        .map_err(|error| AppError::with_source("failed to copy file", error))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)
        .await
        .map_err(|error| AppError::with_source("failed to open copied file", error))?;
    file.sync_all()
        .await
        .map_err(|error| AppError::with_source("failed to flush copied file", error))
}

async fn purge_trash(trash: &Path) {
    let mut entries = match fs::read_dir(trash).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(path = %trash.display(), %error, "failed to inspect staged deletions");
            return;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Err(error) = remove_any(&entry.path()).await {
            tracing::warn!(path = %entry.path().display(), %error, "failed to purge staged deletion");
        }
    }
}

#[cfg(unix)]
async fn sync_parent_directory(path: &Path) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal("path has no parent directory"))?;
    let file = File::open(parent)
        .await
        .map_err(|error| AppError::with_source("failed to open parent directory", error))?;
    file.sync_all()
        .await
        .map_err(|error| AppError::with_source("failed to flush parent directory", error))
}

#[cfg(not(unix))]
async fn sync_parent_directory(_path: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{attachment_header, parse_range, AtomicWriteResult, StorageService};
    use crate::storage_transaction::SYSTEM_DIR;
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
    use bytes::Bytes;
    use std::path::Path;

    #[test]
    fn attachment_header_preserves_utf8_file_names() {
        let value = attachment_header(Path::new("游戏音乐.flac"));
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"____.flac\"; filename*=UTF-8''%E6%B8%B8%E6%88%8F%E9%9F%B3%E4%B9%90.flac"
        );
    }

    #[test]
    fn normalizes_safe_relative_paths() {
        assert_eq!(
            StorageService::normalize_relative("//teams/./ops/runbook.md").unwrap(),
            "teams/ops/runbook.md"
        );
        assert!(StorageService::normalize_relative("../secret").is_err());
        assert!(StorageService::normalize_relative("C:/Windows").is_err());
        assert!(StorageService::normalize_relative("folder\\file").is_err());
    }

    #[test]
    fn parses_single_byte_ranges() {
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=10-19"));
        assert_eq!(
            parse_range(&headers, 100),
            Ok(Some((10, 10, StatusCode::PARTIAL_CONTENT)))
        );

        headers.insert(header::RANGE, HeaderValue::from_static("bytes=-20"));
        assert_eq!(
            parse_range(&headers, 100),
            Ok(Some((80, 20, StatusCode::PARTIAL_CONTENT)))
        );

        headers.insert(header::RANGE, HeaderValue::from_static("bytes=100-200"));
        assert_eq!(parse_range(&headers, 100), Err(()));
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=nope"));
        assert_eq!(parse_range(&headers, 100), Err(()));
    }

    #[tokio::test]
    async fn declared_local_mount_must_exist_before_startup() {
        let root =
            std::env::temp_dir().join(format!("ycloud-declared-mount-{}", uuid::Uuid::new_v4()));
        let result = StorageService::open_declared(
            root.clone(),
            16,
            std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
            100,
            0,
        )
        .await;

        assert!(result.is_err());
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn atomic_writer_commits_streamed_chunks() {
        let root = std::env::temp_dir().join(format!("ycloud-storage-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 16, 2, 100, 0)
            .await
            .unwrap();
        tokio::fs::create_dir(root.join("docs")).await.unwrap();
        let mut writer = storage
            .begin_atomic_write("docs/runbook.txt")
            .await
            .unwrap();
        writer
            .write_chunk(&Bytes::from_static(b"stable"))
            .await
            .unwrap();
        writer
            .write_chunk(&Bytes::from_static(b"-write"))
            .await
            .unwrap();
        assert_eq!(
            writer.commit().await.unwrap(),
            AtomicWriteResult {
                size: 12,
                previous_size: 0,
            }
        );
        assert_eq!(
            tokio::fs::read(root.join("docs/runbook.txt"))
                .await
                .unwrap(),
            b"stable-write"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn dropping_atomic_writer_removes_partial_upload() {
        let root = std::env::temp_dir().join(format!("ycloud-drop-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 16, 2, 100, 0)
            .await
            .unwrap();
        let mut writer = storage
            .begin_atomic_write_with_expected("partial.bin", 8)
            .await
            .unwrap();
        writer
            .write_chunk(&Bytes::from_static(b"partial"))
            .await
            .unwrap();
        drop(writer);
        assert!(!root.join("partial.bin").exists());
        let mut entries = tokio::fs::read_dir(root.join(SYSTEM_DIR).join("uploads"))
            .await
            .unwrap();
        assert!(entries.next_entry().await.unwrap().is_none());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn temporary_upload_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("ycloud-private-upload-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 16, 2, 100, 0)
            .await
            .unwrap();
        let writer = storage.begin_atomic_write("private.bin").await.unwrap();
        let mode = tokio::fs::metadata(&writer.temporary)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        drop(writer);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn atomic_writer_rejects_upload_when_disk_reserve_cannot_be_kept() {
        let root = std::env::temp_dir().join(format!("ycloud-space-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 16, 2, 100, u64::MAX)
            .await
            .unwrap();
        let error = match storage
            .begin_atomic_write_with_expected("blocked.bin", 1)
            .await
        {
            Ok(_) => panic!("upload should be rejected when the reserve cannot be kept"),
            Err(error) => error,
        };
        assert_eq!(error.status(), StatusCode::INSUFFICIENT_STORAGE);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn copy_rejects_destination_below_source() {
        let root = std::env::temp_dir().join(format!("ycloud-copy-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 2, 100, 0)
            .await
            .unwrap();
        tokio::fs::create_dir_all(root.join("source"))
            .await
            .unwrap();
        let source = storage.resolve_existing("source").await.unwrap();
        let destination = storage.resolve_for_write("source/child").await.unwrap();
        assert_eq!(
            storage
                .copy_path(&source, &destination)
                .await
                .unwrap_err()
                .status(),
            StatusCode::CONFLICT
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn reserved_storage_area_is_not_addressable() {
        let root = std::env::temp_dir().join(format!("ycloud-system-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 2, 100, 0)
            .await
            .unwrap();
        assert!(StorageService::normalize_relative(".ycloud-system/marker").is_err());
        assert!(storage
            .resolve_existing(".ycloud-system/marker")
            .await
            .is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn upload_never_replaces_a_directory() {
        let root = std::env::temp_dir().join(format!("ycloud-type-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 2, 100, 0)
            .await
            .unwrap();
        tokio::fs::create_dir(root.join("target")).await.unwrap();
        let mut writer = storage
            .begin_atomic_write_with_expected("target", 4)
            .await
            .unwrap();
        writer
            .write_chunk(&Bytes::from_static(b"data"))
            .await
            .unwrap();
        assert_eq!(
            writer.commit().await.unwrap_err().status(),
            StatusCode::CONFLICT
        );
        assert!(tokio::fs::metadata(root.join("target"))
            .await
            .unwrap()
            .is_dir());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
