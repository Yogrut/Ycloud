use std::{
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};

use axum::{
    body::Body,
    http::{
        header::{self, HeaderMap, HeaderValue},
        Response, StatusCode,
    },
};
use bytes::Bytes;
use futures_util::Stream;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore},
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use crate::{
    error::{AppError, AppResult},
    storage_transaction::{remove_any, TransactionPaths, SYSTEM_DIR},
};

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
    trash_notify: Arc<Notify>,
}

#[derive(Clone, Debug)]
pub struct ResolvedPath {
    relative: String,
    absolute: PathBuf,
}

impl ResolvedPath {
    pub fn relative(&self) -> &str {
        &self.relative
    }

    pub fn absolute(&self) -> &Path {
        &self.absolute
    }

    pub fn is_root(&self) -> bool {
        self.relative.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FileResponseMode {
    Attachment,
    Preview,
    WebDav,
}

impl StorageService {
    pub async fn new(
        root: PathBuf,
        max_upload_bytes: u64,
        io_concurrency: usize,
        max_list_entries: usize,
        disk_reserve_bytes: u64,
    ) -> AppResult<Self> {
        fs::create_dir_all(&root)
            .await
            .map_err(|error| AppError::with_source("failed to create storage directory", error))?;
        let root = fs::canonicalize(&root)
            .await
            .map_err(|error| AppError::with_source("failed to resolve storage directory", error))?;

        let transactions = TransactionPaths::initialize(&root).await?;
        let trash_notify = Arc::new(Notify::new());
        let cleaner_notify = trash_notify.clone();
        let trash = transactions.trash.clone();
        tokio::spawn(async move {
            loop {
                cleaner_notify.notified().await;
                purge_trash(&trash).await;
            }
        });
        Ok(Self {
            root: Arc::new(root),
            io_gate: Arc::new(Semaphore::new(io_concurrency.max(1))),
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

    pub fn normalize_relative(path: &str) -> AppResult<String> {
        if path.contains('\\') || path.contains('\0') {
            return Err(AppError::BadRequest("Invalid storage path".into()));
        }

        let mut components = Vec::new();
        for component in path.trim_matches('/').split('/') {
            match component {
                "" | "." => {}
                ".." => return Err(AppError::BadRequest("Path traversal is not allowed".into())),
                value if value.contains(':') || value.eq_ignore_ascii_case(SYSTEM_DIR) => {
                    return Err(AppError::BadRequest("Invalid storage path".into()));
                }
                value => components.push(value),
            }
        }
        Ok(components.join("/"))
    }

    pub async fn resolve_existing(&self, path: &str) -> AppResult<ResolvedPath> {
        let resolved = self.resolve_for_write(path).await?;
        let canonical = fs::canonicalize(&resolved.absolute)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::with_source("failed to resolve storage path", error),
            })?;
        if !canonical.starts_with(self.root()) {
            return Err(AppError::Forbidden);
        }
        Ok(resolved)
    }

    pub async fn resolve_for_write(&self, path: &str) -> AppResult<ResolvedPath> {
        let relative = Self::normalize_relative(path)?;
        let absolute = relative
            .split('/')
            .filter(|component| !component.is_empty())
            .fold(self.root().to_path_buf(), |current, component| {
                current.join(component)
            });

        let mut existing_ancestor = absolute.clone();
        loop {
            match fs::symlink_metadata(&existing_ancestor).await {
                Ok(metadata) => {
                    if is_link_or_reparse_point(&metadata) {
                        return Err(AppError::Forbidden);
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !existing_ancestor.pop() {
                        return Err(AppError::Forbidden);
                    }
                }
                Err(error) => {
                    return Err(AppError::with_source(
                        "failed to validate storage path",
                        error,
                    ));
                }
            }
        }
        let canonical_ancestor = fs::canonicalize(&existing_ancestor)
            .await
            .map_err(|error| AppError::with_source("failed to validate storage path", error))?;
        if !canonical_ancestor.starts_with(self.root()) {
            return Err(AppError::Forbidden);
        }

        Ok(ResolvedPath { relative, absolute })
    }

    pub async fn metadata(&self, path: &ResolvedPath) -> AppResult<std::fs::Metadata> {
        fs::metadata(path.absolute())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::with_source("failed to read file metadata", error),
            })
    }

    pub async fn begin_atomic_write(&self, path: &str) -> AppResult<AtomicFileWriter> {
        self.begin_atomic_write_internal(path, None).await
    }

    pub async fn begin_atomic_write_with_expected(
        &self,
        path: &str,
        expected_bytes: u64,
    ) -> AppResult<AtomicFileWriter> {
        self.begin_atomic_write_internal(path, Some(expected_bytes))
            .await
    }

    async fn begin_atomic_write_internal(
        &self,
        path: &str,
        expected_bytes: Option<u64>,
    ) -> AppResult<AtomicFileWriter> {
        let max_upload_bytes = self.max_upload_bytes();
        if expected_bytes.is_some_and(|bytes| bytes > max_upload_bytes) {
            return Err(AppError::PayloadTooLarge);
        }
        let destination = self.resolve_for_write(path).await?;
        if destination.is_root() {
            return Err(AppError::BadRequest(
                "The storage root cannot be overwritten".into(),
            ));
        }
        let parent = destination
            .absolute()
            .parent()
            .ok_or_else(|| AppError::BadRequest("Invalid destination path".into()))?;
        let parent_metadata =
            fs::symlink_metadata(parent)
                .await
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => {
                        AppError::BadRequest("Destination directory does not exist".into())
                    }
                    _ => AppError::with_source("failed to inspect destination directory", error),
                })?;
        if !parent_metadata.is_dir() || is_link_or_reparse_point(&parent_metadata) {
            return Err(AppError::Forbidden);
        }

        // Re-check after directory creation so a concurrently swapped symlink
        // cannot silently redirect the final write outside the storage root.
        self.resolve_for_write(destination.relative()).await?;

        let root = self.root.clone();
        let available = tokio::task::spawn_blocking(move || fs4::available_space(root.as_path()))
            .await
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?;
        let initially_reserved = expected_bytes.unwrap_or(0);
        let required = initially_reserved.saturating_add(self.disk_reserve_bytes);
        {
            let mut reserved = self
                .reserved_upload_bytes
                .lock()
                .map_err(|_| AppError::internal("upload reservation state is unavailable"))?;
            if available < required.saturating_add(*reserved) {
                return Err(AppError::InsufficientStorage);
            }
            *reserved = reserved.saturating_add(initially_reserved);
        }
        let reservation = UploadReservation {
            reserved_upload_bytes: self.reserved_upload_bytes.clone(),
            remaining: initially_reserved,
            root: self.root.clone(),
            disk_reserve_bytes: self.disk_reserve_bytes,
        };

        let transaction_id = Uuid::new_v4().to_string();
        let temporary = self.transactions.upload_path(&transaction_id);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
            .map_err(|error| AppError::with_source("failed to create temporary file", error))?;
        let permit = self
            .io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;

        Ok(AtomicFileWriter {
            destination: destination.absolute,
            temporary,
            file: Some(file),
            bytes_written: 0,
            expected_bytes,
            max_bytes: max_upload_bytes,
            committed: false,
            _permit: permit,
            reservation,
            relative: destination.relative,
            transactions: self.transactions.clone(),
            mutation_gate: self.mutation_gate.clone(),
        })
    }

    pub async fn stream_file(
        &self,
        path: &ResolvedPath,
        request_headers: &HeaderMap,
        mode: FileResponseMode,
    ) -> AppResult<Response<Body>> {
        let metadata = self.metadata(path).await?;
        if !metadata.is_file() {
            return Err(AppError::NotFound);
        }

        let total_length = metadata.len();
        let range = parse_range(request_headers, total_length);
        let (start, length, status) = match range {
            Ok(Some(range)) => range,
            Ok(None) => (0, total_length, StatusCode::OK),
            Err(()) => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total_length}"))
                    .header(header::CONTENT_LENGTH, 0)
                    .body(Body::empty())
                    .map_err(|error| {
                        AppError::with_source("failed to build range response", error)
                    });
            }
        };
        let mut file = File::open(path.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to open file", error))?;
        if start > 0 {
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|error| AppError::with_source("failed to seek file", error))?;
        }

        let permit = self
            .io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;
        let reader = file.take(length);
        let stream = PermitStream {
            inner: ReaderStream::new(reader),
            _permit: permit,
        };
        let mut response = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");

        let guessed_mime = mime_guess::from_path(path.absolute()).first_or_octet_stream();
        let (content_type, force_attachment) = content_type_for_mode(&guessed_mime, mode);
        response = response.header(header::CONTENT_TYPE, content_type);
        if matches!(mode, FileResponseMode::Attachment) || force_attachment {
            response = response.header(
                header::CONTENT_DISPOSITION,
                attachment_header(path.absolute()),
            );
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            response = response.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total_length}"),
            );
        }

        response
            .body(Body::from_stream(stream))
            .map_err(|error| AppError::with_source("failed to build file response", error))
    }

    pub async fn remove(&self, path: &ResolvedPath) -> AppResult<()> {
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
        self.transactions.stage_delete(path.absolute()).await?;
        self.trash_notify.notify_one();
        Ok(())
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

    pub(crate) async fn acquire_io(&self) -> AppResult<OwnedSemaphorePermit> {
        self.io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))
    }
}

pub struct AtomicFileWriter {
    destination: PathBuf,
    temporary: PathBuf,
    file: Option<File>,
    bytes_written: u64,
    expected_bytes: Option<u64>,
    max_bytes: u64,
    committed: bool,
    _permit: OwnedSemaphorePermit,
    reservation: UploadReservation,
    relative: String,
    transactions: Arc<TransactionPaths>,
    mutation_gate: Arc<AsyncMutex<()>>,
}

impl AtomicFileWriter {
    pub async fn write_chunk(&mut self, chunk: &Bytes) -> AppResult<()> {
        let new_size = self.bytes_written.saturating_add(chunk.len() as u64);
        if new_size > self.max_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        self.reservation.ensure(chunk.len() as u64).await?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.write_all(chunk)
            .await
            .map_err(|error| AppError::with_source("failed to write upload", error))?;
        self.bytes_written = new_size;
        self.reservation.consume(chunk.len() as u64);
        Ok(())
    }

    pub async fn commit(mut self) -> AppResult<u64> {
        if self
            .expected_bytes
            .is_some_and(|expected| expected != self.bytes_written)
        {
            return Err(AppError::BadRequest(
                "Uploaded size does not match Content-Length".into(),
            ));
        }
        let file = self
            .file
            .take()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.sync_all()
            .await
            .map_err(|error| AppError::with_source("failed to flush upload", error))?;
        drop(file);
        let _mutation = self.mutation_gate.lock().await;
        self.transactions
            .commit_file(&self.relative, &self.temporary, &self.destination)
            .await?;
        self.committed = true;
        Ok(self.bytes_written)
    }
}

impl Drop for AtomicFileWriter {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

struct UploadReservation {
    reserved_upload_bytes: Arc<Mutex<u64>>,
    remaining: u64,
    root: Arc<PathBuf>,
    disk_reserve_bytes: u64,
}

impl UploadReservation {
    async fn ensure(&mut self, bytes: u64) -> AppResult<()> {
        if self.remaining >= bytes {
            return Ok(());
        }
        const CHUNK: u64 = 8 * 1024 * 1024;
        let shortage = bytes - self.remaining;
        let additional = shortage.saturating_add(CHUNK - 1) / CHUNK * CHUNK;
        let root = self.root.clone();
        let available = tokio::task::spawn_blocking(move || fs4::available_space(root.as_path()))
            .await
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?;
        let mut reserved = self
            .reserved_upload_bytes
            .lock()
            .map_err(|_| AppError::internal("upload reservation state is unavailable"))?;
        if available
            < self
                .disk_reserve_bytes
                .saturating_add(*reserved)
                .saturating_add(additional)
        {
            return Err(AppError::InsufficientStorage);
        }
        *reserved = reserved.saturating_add(additional);
        self.remaining = self.remaining.saturating_add(additional);
        Ok(())
    }

    fn consume(&mut self, bytes: u64) {
        let consumed = bytes.min(self.remaining);
        if let Ok(mut reserved) = self.reserved_upload_bytes.lock() {
            *reserved = reserved.saturating_sub(consumed);
        }
        self.remaining -= consumed;
    }
}

impl Drop for UploadReservation {
    fn drop(&mut self) {
        if let Ok(mut reserved) = self.reserved_upload_bytes.lock() {
            *reserved = reserved.saturating_sub(self.remaining);
        }
    }
}

struct PermitStream<S> {
    inner: S,
    _permit: OwnedSemaphorePermit,
}

impl<S> Stream for PermitStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(context)
    }
}

fn reject_root_or_descendant(source: &ResolvedPath, destination: &ResolvedPath) -> AppResult<()> {
    if source.is_root() {
        return Err(AppError::BadRequest(
            "The storage root cannot be moved or copied".into(),
        ));
    }
    if destination.absolute().starts_with(source.absolute()) {
        return Err(AppError::Conflict(
            "Destination cannot be inside the source".into(),
        ));
    }
    Ok(())
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

async fn require_plain_directory(parent: Option<&Path>) -> AppResult<()> {
    let parent = parent.ok_or_else(|| AppError::BadRequest("Invalid destination path".into()))?;
    let metadata = fs::symlink_metadata(parent)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                AppError::BadRequest("Destination directory does not exist".into())
            }
            _ => AppError::with_source("failed to inspect destination directory", error),
        })?;
    if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

pub(crate) fn is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn parse_range(
    headers: &HeaderMap,
    total_length: u64,
) -> Result<Option<(u64, u64, StatusCode)>, ()> {
    let Some(header_value) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    let raw = header_value.to_str().map_err(|_| ())?;
    let value = raw.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') || total_length == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let suffix = suffix.min(total_length);
        (total_length.saturating_sub(suffix), total_length - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= total_length {
            return Err(());
        }
        let end = if end.is_empty() {
            total_length - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total_length - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(Some((start, end - start + 1, StatusCode::PARTIAL_CONTENT)))
}

fn content_type_for_mode(
    guessed: &mime_guess::Mime,
    mode: FileResponseMode,
) -> (HeaderValue, bool) {
    let guessed_string = guessed.to_string();
    match mode {
        FileResponseMode::Attachment => (
            HeaderValue::from_str(&guessed_string)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            false,
        ),
        FileResponseMode::WebDav => (
            HeaderValue::from_str(&guessed_string)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            false,
        ),
        FileResponseMode::Preview => {
            let safe_inline = guessed.type_() == mime_guess::mime::IMAGE
                && guessed.subtype() != mime_guess::mime::SVG
                || guessed.type_() == mime_guess::mime::AUDIO
                || guessed.type_() == mime_guess::mime::VIDEO
                || *guessed == mime_guess::mime::APPLICATION_PDF;
            if safe_inline {
                (
                    HeaderValue::from_str(&guessed_string)
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                    false,
                )
            } else if guessed.type_() == mime_guess::mime::TEXT {
                (HeaderValue::from_static("text/plain; charset=utf-8"), false)
            } else {
                (HeaderValue::from_static("application/octet-stream"), true)
            }
        }
    }
}

pub(crate) fn attachment_header(path: &Path) -> HeaderValue {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let ascii_name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let encoded_name = name
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric()
                || matches!(
                    *byte,
                    b'!' | b'#'
                        | b'$'
                        | b'&'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
            {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{encoded_name}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"download\""))
}

#[cfg(test)]
mod tests {
    use super::{attachment_header, parse_range, StorageService};
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
        assert_eq!(writer.commit().await.unwrap(), 12);
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
