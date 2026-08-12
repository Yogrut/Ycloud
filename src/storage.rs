use std::{
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
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
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

#[derive(Clone)]
pub struct StorageService {
    root: Arc<PathBuf>,
    io_gate: Arc<Semaphore>,
    max_upload_bytes: u64,
    max_list_entries: usize,
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
    ) -> AppResult<Self> {
        fs::create_dir_all(&root)
            .await
            .map_err(|error| AppError::with_source("failed to create storage directory", error))?;
        let root = fs::canonicalize(&root)
            .await
            .map_err(|error| AppError::with_source("failed to resolve storage directory", error))?;

        Ok(Self {
            root: Arc::new(root),
            io_gate: Arc::new(Semaphore::new(io_concurrency.max(1))),
            max_upload_bytes,
            max_list_entries: max_list_entries.max(1),
        })
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_bytes
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
                value if value.contains(':') => {
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
                Ok(_) => break,
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
        fs::create_dir_all(parent).await.map_err(|error| {
            AppError::with_source("failed to create destination directory", error)
        })?;

        // Re-check after directory creation so a concurrently swapped symlink
        // cannot silently redirect the final write outside the storage root.
        self.resolve_for_write(destination.relative()).await?;

        let file_name = destination
            .absolute()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upload");
        let temporary = parent.join(format!(".{file_name}.{}.upload", Uuid::new_v4()));
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
            max_bytes: self.max_upload_bytes,
            committed: false,
            _permit: permit,
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
        let (start, length, status) =
            parse_range(request_headers, total_length).unwrap_or((0, total_length, StatusCode::OK));
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
        let metadata = fs::symlink_metadata(path.absolute())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::with_source("failed to inspect path", error),
            })?;
        if metadata.is_dir() {
            fs::remove_dir_all(path.absolute())
                .await
                .map_err(|error| AppError::with_source("failed to remove directory", error))
        } else {
            fs::remove_file(path.absolute())
                .await
                .map_err(|error| AppError::with_source("failed to remove file", error))
        }
    }

    pub async fn create_directory(&self, path: &ResolvedPath) -> AppResult<()> {
        let _permit = self.acquire_io().await?;
        fs::create_dir(path.absolute())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    AppError::Conflict("Destination already exists".into())
                }
                _ => AppError::with_source("failed to create directory", error),
            })
    }

    pub async fn move_path(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
    ) -> AppResult<()> {
        reject_root_or_descendant(source, destination)?;
        if fs::try_exists(destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to inspect destination", error))?
        {
            return Err(AppError::Conflict("Destination already exists".into()));
        }
        if let Some(parent) = destination.absolute().parent() {
            fs::create_dir_all(parent).await.map_err(|error| {
                AppError::with_source("failed to create destination directory", error)
            })?;
        }
        let _permit = self.acquire_io().await?;
        fs::rename(source.absolute(), destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to move path", error))
    }

    pub async fn copy_path(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
    ) -> AppResult<()> {
        reject_root_or_descendant(source, destination)?;
        if fs::try_exists(destination.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to inspect destination", error))?
        {
            return Err(AppError::Conflict("Destination already exists".into()));
        }

        let _permit = self.acquire_io().await?;
        let metadata = self.metadata(source).await?;
        if metadata.is_dir() {
            copy_directory_iterative(source.absolute(), destination.absolute()).await
        } else {
            if let Some(parent) = destination.absolute().parent() {
                fs::create_dir_all(parent).await.map_err(|error| {
                    AppError::with_source("failed to create destination directory", error)
                })?;
            }
            fs::copy(source.absolute(), destination.absolute())
                .await
                .map(|_| ())
                .map_err(|error| AppError::with_source("failed to copy file", error))
        }
    }

    pub async fn ready(&self) -> bool {
        fs::metadata(self.root())
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
    }

    async fn acquire_io(&self) -> AppResult<OwnedSemaphorePermit> {
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
    max_bytes: u64,
    committed: bool,
    _permit: OwnedSemaphorePermit,
}

impl AtomicFileWriter {
    pub async fn write_chunk(&mut self, chunk: &Bytes) -> AppResult<()> {
        let new_size = self.bytes_written.saturating_add(chunk.len() as u64);
        if new_size > self.max_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.write_all(chunk)
            .await
            .map_err(|error| AppError::with_source("failed to write upload", error))?;
        self.bytes_written = new_size;
        Ok(())
    }

    pub async fn commit(mut self) -> AppResult<u64> {
        let file = self
            .file
            .take()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.sync_all()
            .await
            .map_err(|error| AppError::with_source("failed to flush upload", error))?;
        drop(file);
        replace_with_backup(&self.temporary, &self.destination).await?;
        self.committed = true;
        Ok(self.bytes_written)
    }
}

impl Drop for AtomicFileWriter {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.temporary);
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

async fn replace_with_backup(temporary: &Path, destination: &Path) -> AppResult<()> {
    if !fs::try_exists(destination)
        .await
        .map_err(|error| AppError::with_source("failed to inspect destination", error))?
    {
        return fs::rename(temporary, destination)
            .await
            .map_err(|error| AppError::with_source("failed to commit file", error));
    }

    let backup = destination.with_extension(format!("replace-{}.bak", Uuid::new_v4()));
    fs::rename(destination, &backup)
        .await
        .map_err(|error| AppError::with_source("failed to prepare file replacement", error))?;
    if let Err(error) = fs::rename(temporary, destination).await {
        let _ = fs::rename(&backup, destination).await;
        return Err(AppError::with_source(
            "failed to commit file replacement",
            error,
        ));
    }
    if let Err(error) = fs::remove_file(&backup).await {
        tracing::warn!(path = %backup.display(), %error, "failed to remove replacement backup");
    }
    Ok(())
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
            let metadata = entry
                .metadata()
                .await
                .map_err(|error| AppError::with_source("failed to inspect copied entry", error))?;
            if metadata.is_dir() {
                pending.push((source_path, destination_path));
            } else if metadata.is_file() {
                fs::copy(&source_path, &destination_path)
                    .await
                    .map_err(|error| AppError::with_source("failed to copy file", error))?;
            }
        }
    }
    Ok(())
}

fn parse_range(headers: &HeaderMap, total_length: u64) -> Option<(u64, u64, StatusCode)> {
    let raw = headers.get(header::RANGE)?.to_str().ok()?;
    let value = raw.strip_prefix("bytes=")?;
    if value.contains(',') || total_length == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?.min(total_length);
        (total_length.saturating_sub(suffix), total_length - 1)
    } else {
        let start = start.parse::<u64>().ok()?;
        if start >= total_length {
            return None;
        }
        let end = if end.is_empty() {
            total_length - 1
        } else {
            end.parse::<u64>().ok()?.min(total_length - 1)
        };
        if end < start {
            return None;
        }
        (start, end)
    };
    Some((start, end - start + 1, StatusCode::PARTIAL_CONTENT))
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

fn attachment_header(path: &Path) -> HeaderValue {
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
    HeaderValue::from_str(&format!("attachment; filename=\"{ascii_name}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"download\""))
}

#[cfg(test)]
mod tests {
    use super::{parse_range, StorageService};
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
    use bytes::Bytes;

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
            Some((10, 10, StatusCode::PARTIAL_CONTENT))
        );

        headers.insert(header::RANGE, HeaderValue::from_static("bytes=-20"));
        assert_eq!(
            parse_range(&headers, 100),
            Some((80, 20, StatusCode::PARTIAL_CONTENT))
        );
    }

    #[tokio::test]
    async fn atomic_writer_commits_streamed_chunks() {
        let root =
            std::env::temp_dir().join(format!("ycloud-storage-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 16, 2, 100).await.unwrap();
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
    async fn copy_rejects_destination_below_source() {
        let root =
            std::env::temp_dir().join(format!("ycloud-copy-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 2, 100)
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
}
