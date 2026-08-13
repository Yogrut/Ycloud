use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use async_zip::{tokio::write::ZipFileWriter, Compression, ZipEntryBuilder};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, Response},
    Json,
};
use futures_util::io::AsyncWriteExt as FuturesAsyncWriteExt;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncReadExt,
    sync::{Mutex, Semaphore},
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    file_access::{
        resolve_share, share_storage_path, validate_batch_size, FileQuery, FolderLockAuthorizer,
    },
    state::AppState,
    storage::attachment_header,
};

pub const MAX_ARCHIVE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
pub const MAX_ARCHIVE_FILES: usize = 1_000;
const MAX_ARCHIVE_VISITED_ENTRIES: usize = 10_000;
const MAX_PENDING_TICKETS: usize = 32;
const TICKET_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct ArchiveTicketStore {
    tickets: Arc<Mutex<HashMap<String, ArchiveTicket>>>,
    prepare_gate: Arc<Semaphore>,
    stream_gate: Arc<Semaphore>,
}

#[derive(Clone)]
struct ArchiveTicket {
    files: Vec<ArchiveFile>,
    archive_name: String,
    created_at: Instant,
}

#[derive(Clone)]
struct ArchiveFile {
    absolute: PathBuf,
    storage_path: String,
    zip_path: String,
    size: u64,
}

impl Default for ArchiveTicketStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveTicketStore {
    pub fn new() -> Self {
        Self {
            tickets: Arc::new(Mutex::new(HashMap::new())),
            prepare_gate: Arc::new(Semaphore::new(1)),
            stream_gate: Arc::new(Semaphore::new(1)),
        }
    }

    async fn create(&self, files: Vec<ArchiveFile>, archive_name: String) -> AppResult<String> {
        let mut tickets = self.tickets.lock().await;
        tickets.retain(|_, ticket| ticket.created_at.elapsed() <= TICKET_TTL);
        if tickets.len() >= MAX_PENDING_TICKETS {
            return Err(AppError::ServiceUnavailable(
                "Too many pending archive downloads".into(),
            ));
        }
        let token = Uuid::new_v4().to_string();
        tickets.insert(
            token.clone(),
            ArchiveTicket {
                files,
                archive_name,
                created_at: Instant::now(),
            },
        );
        Ok(token)
    }

    async fn take(&self, token: &str) -> Option<ArchiveTicket> {
        let mut tickets = self.tickets.lock().await;
        let ticket = tickets.remove(token)?;
        (ticket.created_at.elapsed() <= TICKET_TTL).then_some(ticket)
    }

    async fn acquire_prepare(&self) -> AppResult<tokio::sync::OwnedSemaphorePermit> {
        self.prepare_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Archive service is shutting down".into()))
    }

    fn acquire_stream(&self) -> AppResult<tokio::sync::OwnedSemaphorePermit> {
        self.stream_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::TooManyRequests)
    }
}

#[derive(Deserialize)]
pub struct PrepareArchiveBody {
    paths: Vec<String>,
}

#[derive(Serialize)]
pub struct PrepareArchiveResponse {
    ticket: String,
    total_bytes: u64,
    file_count: usize,
    max_bytes: u64,
    max_files: usize,
}

#[derive(Deserialize)]
pub struct ArchiveQuery {
    ticket: String,
}

pub async fn prepare_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PrepareArchiveBody>,
) -> AppResult<Json<PrepareArchiveResponse>> {
    let _prepare_permit = state.archive_tickets.acquire_prepare().await?;
    validate_batch_size(&body.paths)?;
    let share = resolve_share(&state, &headers, &FileQuery { path: None }).await?;
    let authorizer = FolderLockAuthorizer::new(&state, &headers).await;
    let normalized = body
        .paths
        .iter()
        .map(|path| crate::storage::StorageService::normalize_relative(path))
        .collect::<AppResult<Vec<_>>>()?;
    if normalized.iter().any(String::is_empty) {
        return Err(AppError::BadRequest(
            "The storage root cannot be archived".into(),
        ));
    }
    let common_parent = common_parent(&normalized);
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut total_bytes = 0_u64;
    let mut visited_entries = 0_usize;
    for path in &normalized {
        let storage_path = share_storage_path(&share, path);
        authorizer.ensure_access(&storage_path)?;
        let resolved = state.storage.resolve_existing(&storage_path).await?;
        collect_files(
            &state,
            &authorizer,
            resolved.absolute().to_path_buf(),
            path.clone(),
            &common_parent,
            &mut files,
            &mut seen,
            &mut total_bytes,
            &mut visited_entries,
        )
        .await?;
    }
    if files.is_empty() {
        return Err(AppError::BadRequest("No files selected for archive".into()));
    }
    let archive_name = archive_name(&normalized);
    let file_count = files.len();
    let ticket = state.archive_tickets.create(files, archive_name).await?;
    Ok(Json(PrepareArchiveResponse {
        ticket,
        total_bytes,
        file_count,
        max_bytes: MAX_ARCHIVE_BYTES,
        max_files: MAX_ARCHIVE_FILES,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn collect_files(
    state: &AppState,
    authorizer: &FolderLockAuthorizer,
    root: PathBuf,
    request_path: String,
    common_parent: &str,
    files: &mut Vec<ArchiveFile>,
    seen: &mut HashSet<PathBuf>,
    total_bytes: &mut u64,
    visited_entries: &mut usize,
) -> AppResult<()> {
    let mut pending = vec![(root, request_path)];
    while let Some((absolute, relative)) = pending.pop() {
        *visited_entries = visited_entries.saturating_add(1);
        if *visited_entries > MAX_ARCHIVE_VISITED_ENTRIES {
            return Err(AppError::BadRequest(
                "Archive traversal exceeds the 10000 entry safety limit".into(),
            ));
        }
        let metadata = fs::symlink_metadata(&absolute)
            .await
            .map_err(|error| AppError::with_source("failed to inspect archive entry", error))?;
        if crate::storage::is_link_or_reparse_point(&metadata) {
            return Err(AppError::BadRequest(
                "Archives cannot contain symbolic links".into(),
            ));
        }
        authorizer.ensure_access(&relative)?;
        if metadata.is_dir() {
            let mut directory = fs::read_dir(&absolute).await.map_err(|error| {
                AppError::with_source("failed to read archive directory", error)
            })?;
            while let Some(entry) = directory
                .next_entry()
                .await
                .map_err(|error| AppError::with_source("failed to read archive entry", error))?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR) {
                    continue;
                }
                pending.push((entry.path(), join_path(&relative, &name)));
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let canonical = fs::canonicalize(&absolute)
            .await
            .map_err(|error| AppError::with_source("failed to resolve archive entry", error))?;
        if !canonical.starts_with(state.storage.root()) {
            return Err(AppError::Forbidden);
        }
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if files.len() >= MAX_ARCHIVE_FILES {
            return Err(AppError::BadRequest(
                "Archive exceeds the 1000 file limit".into(),
            ));
        }
        *total_bytes = total_bytes
            .checked_add(metadata.len())
            .ok_or(AppError::PayloadTooLarge)?;
        if *total_bytes > MAX_ARCHIVE_BYTES {
            return Err(AppError::PayloadTooLarge);
        }
        let zip_path = relative
            .strip_prefix(common_parent)
            .unwrap_or(&relative)
            .trim_start_matches('/')
            .to_string();
        files.push(ArchiveFile {
            absolute: canonical,
            storage_path: relative,
            zip_path,
            size: metadata.len(),
        });
    }
    Ok(())
}

pub async fn download_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ArchiveQuery>,
) -> AppResult<Response<Body>> {
    let stream_permit = state.archive_tickets.acquire_stream()?;
    let ticket = state
        .archive_tickets
        .take(&query.ticket)
        .await
        .ok_or(AppError::NotFound)?;
    let authorizer = FolderLockAuthorizer::new(&state, &headers).await;
    for file in &ticket.files {
        authorizer.ensure_access(&file.storage_path)?;
    }
    let (writer_side, reader_side) = tokio::io::duplex(128 * 1024);
    let storage = state.storage.clone();
    tokio::spawn(async move {
        let _stream_permit = stream_permit;
        if let Err(error) = write_archive(storage, ticket.files, writer_side).await {
            tracing::warn!(%error, "archive download stopped");
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            attachment_header(Path::new(&ticket.archive_name)),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from_stream(ReaderStream::new(reader_side)))
        .map_err(|error| AppError::with_source("failed to build archive response", error))
}

async fn write_archive(
    storage: crate::storage::StorageService,
    files: Vec<ArchiveFile>,
    output: tokio::io::DuplexStream,
) -> anyhow::Result<()> {
    let _permit = storage.acquire_io().await?;
    let mut zip = ZipFileWriter::with_tokio(output);
    let mut buffer = vec![0_u8; 128 * 1024];
    for file in files {
        let current = fs::symlink_metadata(&file.absolute).await?;
        let canonical = fs::canonicalize(&file.absolute).await?;
        if crate::storage::is_link_or_reparse_point(&current)
            || !current.is_file()
            || current.len() != file.size
            || canonical != file.absolute
            || !canonical.starts_with(storage.root())
        {
            anyhow::bail!("archive source changed during download");
        }
        let entry = ZipEntryBuilder::new(file.zip_path.into(), Compression::Stored);
        let mut entry_writer = zip.write_entry_stream(entry).await?;
        let mut source = fs::File::open(&file.absolute).await?;
        loop {
            let read = source.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            entry_writer.write_all(&buffer[..read]).await?;
        }
        entry_writer.close().await?;
    }
    zip.close().await?;
    Ok(())
}

fn common_parent(paths: &[String]) -> String {
    let first_parent = paths[0]
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let mut components: Vec<&str> = first_parent
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for path in &paths[1..] {
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        let other: Vec<&str> = parent.split('/').filter(|part| !part.is_empty()).collect();
        let shared = components
            .iter()
            .zip(other.iter())
            .take_while(|(left, right)| left == right)
            .count();
        components.truncate(shared);
    }
    components.join("/")
}

fn archive_name(paths: &[String]) -> String {
    if paths.len() == 1 {
        let name = paths[0].rsplit('/').next().unwrap_or("Ycloud");
        format!("{name}.zip")
    } else {
        "Ycloud-打包下载.zip".to_string()
    }
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        archive_name, common_parent, write_archive, ArchiveFile, MAX_ARCHIVE_BYTES,
        MAX_ARCHIVE_FILES,
    };
    use crate::storage::StorageService;
    use futures_util::io::AsyncReadExt as FuturesAsyncReadExt;
    use tokio::io::AsyncReadExt;

    #[test]
    fn archive_limit_is_exactly_three_gibibytes() {
        assert_eq!(MAX_ARCHIVE_BYTES, 3_221_225_472);
        assert_eq!(MAX_ARCHIVE_FILES, 1_000);
    }

    #[test]
    fn archive_paths_are_relative_to_the_selection_parent() {
        let paths = vec!["games/a.7z.001".into(), "games/a.7z.002".into()];
        assert_eq!(common_parent(&paths), "games");
        assert_eq!(archive_name(&paths), "Ycloud-打包下载.zip");
    }

    #[tokio::test]
    async fn streamed_archive_preserves_utf8_name_and_content() {
        let root = std::env::temp_dir().join(format!("ycloud-archive-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let absolute = root.join("游戏音乐.flac");
        tokio::fs::write(&absolute, b"sample-content")
            .await
            .unwrap();
        let canonical = tokio::fs::canonicalize(&absolute).await.unwrap();
        let storage = StorageService::new(root.clone(), 1024, 2, 100, 0)
            .await
            .unwrap();
        let (writer_side, mut reader_side) = tokio::io::duplex(16 * 1024);
        let task = tokio::spawn(write_archive(
            storage,
            vec![ArchiveFile {
                absolute: canonical,
                storage_path: "游戏音乐.flac".into(),
                zip_path: "游戏音乐.flac".into(),
                size: 14,
            }],
            writer_side,
        ));
        let mut bytes = Vec::new();
        reader_side.read_to_end(&mut bytes).await.unwrap();
        task.await.unwrap().unwrap();

        let archive = async_zip::base::read::mem::ZipFileReader::new(bytes)
            .await
            .unwrap();
        assert_eq!(
            archive.file().entries()[0].filename().as_str().unwrap(),
            "游戏音乐.flac"
        );
        let mut entry = archive.reader_without_entry(0).await.unwrap();
        let mut content = Vec::new();
        FuturesAsyncReadExt::read_to_end(&mut entry, &mut content)
            .await
            .unwrap();
        assert_eq!(content, b"sample-content");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn only_one_archive_stream_can_run_at_a_time() {
        let store = super::ArchiveTicketStore::new();
        let permit = store.acquire_stream().unwrap();
        assert_eq!(
            store.acquire_stream().unwrap_err().status(),
            axum::http::StatusCode::TOO_MANY_REQUESTS
        );
        drop(permit);
        assert!(store.acquire_stream().is_ok());
    }
}
