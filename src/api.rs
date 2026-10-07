use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, Method},
    response::IntoResponse,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::directory_listing::{
    DirectoryEntryFilter, DirectoryListRequest, DirectorySort, EntryPosition, SortDirection,
};
use crate::error::{AppError, AppResult};
use crate::file_access::{
    check_folder_lock_tree, check_folder_locks, ensure_non_root, ensure_storage_action,
    ensure_writable, join_request_path, resolve_share, resolve_write_share, share_storage_path,
    storage_permission, FileQuery, FolderLockAuthorizer, StorageAction,
};
use crate::security::session_cookie;
use crate::state::AppState;
use crate::storage::FileResponseMode;

// ── Request / response types ──────────────────────────────────────

const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_DIRECTORY_SEARCH_CHARS: usize = 256;
const MAX_DIRECTORY_CURSOR_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: String,
    pub mime: String,
    pub icon: String,
    #[serde(default)]
    pub locked: bool,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub storage_id: String,
    pub storages: Vec<BrowserStorageView>,
    pub selection_scope: String,
    pub empty_reason: Option<&'static str>,
    pub current_path: String,
    pub parent_path: Option<String>,
    pub entries: Vec<FileEntry>,
    pub page_start: usize,
    pub page_size: usize,
    pub next_cursor: Option<String>,
    pub can_write: bool,
    pub is_admin: bool,
    pub capabilities: BrowserCapabilities,
    pub max_upload_bytes: u64,
    pub max_upload_batch_bytes: u64,
    pub max_upload_batch_entries: usize,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
}

#[derive(Default, Deserialize)]
pub struct FileListQuery {
    pub path: Option<String>,
    #[serde(default)]
    pub storage_id: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub search: Option<String>,
    #[serde(default)]
    gallery: bool,
    #[serde(default)]
    sort: DirectorySort,
    #[serde(default)]
    direction: SortDirection,
}

impl FileListQuery {
    fn file_query(&self) -> FileQuery {
        FileQuery {
            path: self.path.clone(),
            storage_id: self.storage_id.clone(),
            batch: None,
        }
    }
}

fn page_size(limit: Option<usize>) -> AppResult<usize> {
    match limit.unwrap_or(DEFAULT_PAGE_SIZE) {
        value @ (10 | 20 | 50 | 100) => Ok(value),
        _ => Err(AppError::BadRequest(
            "Page size must be one of 10, 20, 50, or 100".into(),
        )),
    }
}

const DIRECTORY_CURSOR_VERSION: u8 = 1;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DirectoryCursorToken {
    version: u8,
    storage_id: String,
    directory: String,
    search: Option<String>,
    sort: DirectorySort,
    direction: SortDirection,
    #[serde(default)]
    filter: DirectoryEntryFilter,
    delivered: usize,
    position: EntryPosition,
}

fn normalized_search(search: Option<&str>) -> Option<String> {
    search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

fn encode_cursor(cursor: &DirectoryCursorToken) -> AppResult<String> {
    serde_json::to_vec(cursor)
        .map(|value| URL_SAFE_NO_PAD.encode(value))
        .map_err(|error| AppError::with_source("failed to encode directory cursor", error))
}

fn decode_cursor(
    cursor: Option<&str>,
    storage_id: &str,
    directory: &str,
    search: Option<&str>,
    sort: DirectorySort,
    direction: SortDirection,
    filter: DirectoryEntryFilter,
) -> AppResult<Option<DirectoryCursorToken>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.len() > MAX_DIRECTORY_CURSOR_BYTES {
        return Err(AppError::BadRequest("Invalid directory cursor".into()));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::BadRequest("Invalid directory cursor".into()))?;
    let token: DirectoryCursorToken = serde_json::from_slice(&decoded)
        .map_err(|_| AppError::BadRequest("Invalid directory cursor".into()))?;
    if token.version != DIRECTORY_CURSOR_VERSION
        || token.storage_id != storage_id
        || token.directory != directory
        || token.search != normalized_search(search)
        || token.sort != sort
        || token.direction != direction
        || token.filter != filter
    {
        return Err(AppError::BadRequest(
            "Directory cursor does not match this listing request".into(),
        ));
    }
    Ok(Some(token))
}

#[derive(Default, Serialize)]
pub struct BrowserCapabilities {
    pub download: bool,
    pub upload: bool,
    pub create_directory: bool,
    pub rename: bool,
    pub move_items: bool,
    pub copy: bool,
    pub delete: bool,
}

#[derive(Serialize)]
pub struct BrowserStorageView {
    pub id: String,
    pub name: String,
    pub requires_login: bool,
    pub enabled: bool,
    pub ready: bool,
}

async fn browser_selection_scope(state: &AppState, headers: &HeaderMap) -> String {
    match crate::auth::current_principal(state, headers).await {
        Some(crate::auth::SessionPrincipal::Administrator) => "administrator".into(),
        Some(crate::auth::SessionPrincipal::User(id)) => format!("user:{id}"),
        None => "guest".into(),
    }
}

async fn browser_storage_views(state: &AppState, headers: &HeaderMap) -> Vec<BrowserStorageView> {
    let storage_configs = state.config_file.read().await.storage_instances.clone();
    let administrator = matches!(
        crate::auth::current_principal(state, headers).await,
        Some(crate::auth::SessionPrincipal::Administrator)
    );
    let mut storages = Vec::with_capacity(storage_configs.len());
    for storage in storage_configs {
        let can_browse = storage_permission(state, headers, &storage.id)
            .await
            .is_some_and(|permission| permission.browse);
        if !administrator && !can_browse {
            continue;
        }
        let ready = storage.enabled && state.backends.is_ready(&storage.id).await;
        storages.push(BrowserStorageView {
            id: storage.id,
            name: storage.name,
            requires_login: false,
            enabled: storage.enabled,
            ready,
        });
    }
    storages
}

pub async fn browser_storages(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<Vec<BrowserStorageView>> {
    Json(browser_storage_views(&state, &headers).await)
}

#[derive(Deserialize)]
pub struct MkdirBody {
    pub name: String,
}

#[derive(Deserialize)]
pub struct RenameBody {
    pub path: String,
    pub new_name: String,
}

#[derive(Deserialize)]
pub struct LockBody {
    pub path: String,
    pub password: String,
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| {
            !matches!(
                c,
                '/' | '\\' | '\0' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
            )
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn get_icon(name: &str, is_dir: bool, mime: &str) -> String {
    if is_dir {
        return "folder".into();
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif" => "image",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "flv" => "video",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" => "audio",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" => "archive",
        "pdf" => "pdf",
        "doc" | "docx" | "xls" | "xlsx" | "csv" | "ppt" | "pptx" => "doc",
        "rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "cpp" | "h" | "hpp" | "html" | "css"
        | "json" | "xml" | "yaml" | "yml" | "toml" | "sh" | "sql" | "vue" | "rb" | "php"
        | "swift" | "kt" | "cs" | "lua" | "txt" | "md" => "code",
        _ => {
            if mime.starts_with("image/") {
                "image"
            } else if mime.starts_with("video/") {
                "video"
            } else if mime.starts_with("audio/") {
                "audio"
            } else if mime.starts_with("text/") {
                "code"
            } else {
                "file"
            }
        }
    }
    .into()
}
// ── Handlers ──────────────────────────────────────────────────────

/// List files in a share directory.
pub async fn list_files(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileListQuery>,
) -> AppResult<Json<ListResponse>> {
    if query
        .search
        .as_deref()
        .is_some_and(|search| search.chars().count() > MAX_DIRECTORY_SEARCH_CHARS)
    {
        return Err(AppError::BadRequest(
            "Directory search must contain at most 256 characters".into(),
        ));
    }
    let file_query = query.file_query();
    let share = match resolve_share(&state, &headers, &file_query).await {
        Ok(share) => share,
        Err(status)
            if query.storage_id.is_none() && status != axum::http::StatusCode::UNAUTHORIZED =>
        {
            let storages = browser_storage_views(&state, &headers).await;
            let selection_scope = browser_selection_scope(&state, &headers).await;
            let config = state.config_file.read().await.clone();
            let empty_reason = if config.storage_instances.is_empty() {
                "unconfigured"
            } else if storages.is_empty() {
                "forbidden"
            } else {
                "unavailable"
            };
            return Ok(Json(ListResponse {
                storage_id: String::new(),
                storages,
                selection_scope,
                empty_reason: Some(empty_reason),
                current_path: String::new(),
                parent_path: None,
                entries: Vec::new(),
                page_start: 0,
                page_size: page_size(query.limit)?,
                next_cursor: None,
                can_write: false,
                is_admin: matches!(
                    crate::auth::current_principal(&state, &headers).await,
                    Some(crate::auth::SessionPrincipal::Administrator)
                ),
                capabilities: BrowserCapabilities::default(),
                max_upload_bytes: config.max_upload_bytes,
                max_upload_batch_bytes: config.max_upload_batch_bytes,
                max_upload_batch_entries: config.max_upload_batch_entries,
                max_archive_bytes: config.max_archive_bytes,
                max_archive_entries: config.max_archive_entries,
            }));
        }
        Err(status) => return Err(status.into()),
    };
    let backend = state.storage_backend(&share.storage_id).await?;
    let request_path = query.path.as_deref().unwrap_or("");
    let storage_directory = share_storage_path(&share, request_path);
    if !backend.metadata(&storage_directory).await?.is_dir {
        return Err(AppError::NotFound);
    }
    let lock_authorizer = FolderLockAuthorizer::new(&state, &headers, &share.storage_id).await;
    lock_authorizer.ensure_access(&share_storage_path(&share, request_path))?;
    let filter = if query.gallery {
        DirectoryEntryFilter::Gallery
    } else {
        DirectoryEntryFilter::All
    };
    let page_size = if query.gallery {
        DEFAULT_PAGE_SIZE
    } else {
        page_size(query.limit)?
    };
    let cursor = decode_cursor(
        query.cursor.as_deref(),
        &share.storage_id,
        &storage_directory,
        query.search.as_deref(),
        query.sort,
        query.direction,
        filter,
    )?;
    let delivered = cursor.as_ref().map_or(0, |cursor| cursor.delivered);
    let page = backend
        .list_directory_page(
            &storage_directory,
            DirectoryListRequest {
                limit: page_size,
                search: normalized_search(query.search.as_deref()),
                sort: query.sort,
                direction: query.direction,
                filter,
                after: cursor.map(|cursor| cursor.position),
            },
        )
        .await?;
    let next_cursor = page
        .next_position
        .map(|position| {
            encode_cursor(&DirectoryCursorToken {
                version: DIRECTORY_CURSOR_VERSION,
                storage_id: share.storage_id.clone(),
                directory: storage_directory.clone(),
                search: normalized_search(query.search.as_deref()),
                sort: query.sort,
                direction: query.direction,
                filter,
                delivered: delivered
                    .checked_add(page.entries.len())
                    .ok_or_else(|| AppError::BadRequest("Invalid directory cursor".into()))?,
                position,
            })
        })
        .transpose()?;
    let backend_entries = page.entries;
    let mut entries = Vec::with_capacity(backend_entries.len());
    for entry in backend_entries {
        let name = entry.name;
        let is_dir = entry.is_dir;
        let size = entry.size;
        let modified = entry
            .modified_unix
            .and_then(chrono::DateTime::from_timestamp_secs)
            .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let share_relative = if request_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", request_path.trim_end_matches('/'), name)
        };
        let mime = if is_dir {
            "inode/directory".into()
        } else {
            mime_guess::from_path(&name)
                .first_or_octet_stream()
                .to_string()
        };
        let icon = get_icon(&name, is_dir, &mime);
        let full_entry_path = share_storage_path(&share, &share_relative);
        let locked = is_dir && lock_authorizer.is_locked(&full_entry_path);
        entries.push(FileEntry {
            name,
            path: share_relative,
            is_dir,
            size,
            modified,
            mime,
            icon,
            locked,
        });
    }

    let page_start = if entries.is_empty() {
        0
    } else {
        delivered
            .checked_add(1)
            .ok_or_else(|| AppError::BadRequest("Invalid directory cursor".into()))?
    };

    let current_path = request_path.to_string();
    let parent_path = PathBuf::from(&current_path)
        .parent()
        .and_then(|p| p.to_str())
        .map(|s| s.to_string());

    let permission = storage_permission(&state, &headers, &share.storage_id).await;
    let (
        max_upload_bytes,
        max_upload_batch_bytes,
        max_upload_batch_entries,
        max_archive_bytes,
        max_archive_entries,
    ) = {
        let config = state.config_file.read().await;
        (
            config.max_upload_bytes,
            config.max_upload_batch_bytes,
            config.max_upload_batch_entries,
            config.max_archive_bytes,
            config.max_archive_entries,
        )
    };
    let principal = crate::auth::current_principal(&state, &headers).await;
    let is_admin = matches!(
        principal,
        Some(crate::auth::SessionPrincipal::Administrator)
    );
    let storages = browser_storage_views(&state, &headers).await;
    let mut capabilities = permission
        .as_ref()
        .map(|permission| BrowserCapabilities {
            download: permission.download,
            upload: permission.upload,
            create_directory: permission.create_directory,
            rename: permission.rename,
            move_items: permission.move_items,
            copy: permission.copy,
            delete: permission.delete,
        })
        .unwrap_or_default();
    if share.readonly {
        capabilities.upload = false;
        capabilities.create_directory = false;
        capabilities.rename = false;
        capabilities.move_items = false;
        capabilities.copy = false;
        capabilities.delete = false;
    }
    let can_write = permission
        .as_ref()
        .is_some_and(|permission| permission.grants_write());
    Ok(Json(ListResponse {
        storage_id: share.storage_id,
        selection_scope: browser_selection_scope(&state, &headers).await,
        empty_reason: None,
        storages,
        current_path,
        parent_path,
        entries,
        page_start,
        page_size,
        next_cursor,
        can_write: !share.readonly && can_write,
        is_admin,
        capabilities,
        max_upload_bytes,
        max_upload_batch_bytes,
        max_upload_batch_entries,
        max_archive_bytes,
        max_archive_entries,
    }))
}

pub async fn create_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<MkdirBody>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_write_share(&state, &headers, &query).await?;
    ensure_storage_action(
        &state,
        &headers,
        &share.storage_id,
        StorageAction::CreateDirectory,
    )
    .await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    let request_path = query.path.as_deref().unwrap_or("");
    check_folder_locks(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, request_path),
    )
    .await?;
    let name = sanitize_name(&body.name);
    if name.is_empty() {
        return Err(AppError::BadRequest("Directory name is required".into()));
    }
    let new_path = join_request_path(request_path, &name);
    check_folder_locks(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, &new_path),
    )
    .await?;
    backend
        .create_directory(&share_storage_path(&share, &new_path))
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn upload_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    body: Body,
) -> AppResult<Json<serde_json::Value>> {
    let expected_bytes = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let share = resolve_write_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Upload).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    let file_request_path = query.path.as_deref().unwrap_or("").trim_matches('/');
    ensure_non_root(file_request_path)?;
    let file_name = file_request_path
        .rsplit('/')
        .next()
        .map(sanitize_name)
        .unwrap_or_default();
    if file_name.is_empty() || file_name != file_request_path.rsplit('/').next().unwrap_or("") {
        return Err(AppError::BadRequest(
            "A valid target file path is required".into(),
        ));
    }
    check_folder_locks(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, file_request_path),
    )
    .await?;
    let storage_path = share_storage_path(&share, file_request_path);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let traffic_subject = crate::traffic::browser_subject(&state, &headers).await;
    let max_upload_bytes = state.config_file.read().await.max_upload_bytes;
    if query.batch.is_some() && expected_bytes.is_none() {
        return Err(AppError::BadRequest(
            "批量上传必须提供 Content-Length".into(),
        ));
    }
    if let (Some(ticket), Some(size)) = (query.batch.as_deref(), expected_bytes) {
        let subject = crate::auth::current_request_subject(&state, &headers)
            .await
            .ok_or(AppError::Forbidden)?;
        let begin = state
            .upload_batches
            .begin(ticket, &subject, &share.storage_id, &storage_path, size)
            .await?;
        match begin {
            crate::upload_batch::UploadBegin::Start => {}
            crate::upload_batch::UploadBegin::AlreadyComplete(None) => {
                return Ok(Json(
                    serde_json::json!({ "success": true, "uploaded": [file_name], "replayed": true }),
                ));
            }
            crate::upload_batch::UploadBegin::AlreadyComplete(Some(outcome)) => {
                return Err(AppError::Conflict(
                    "文件变更已提交，但后续收尾未完成；请勿重复上传".into(),
                )
                .with_operation(outcome.commit, outcome.cleanup));
            }
        }
    }
    let operation_id = query
        .batch
        .as_deref()
        .map(|ticket| crate::upload_batch::operation_id(ticket, &storage_path));
    let execution = query.batch.map(|ticket| {
        crate::upload_batch::UploadExecutionGuard::new(
            state.upload_batches.clone(),
            ticket,
            storage_path.clone(),
        )
    });
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(state.config.upload_timeout_secs);
    let body = Body::from_stream(futures_util::stream::unfold(
        (body.into_data_stream(), false),
        move |(mut stream, finished)| async move {
            if finished {
                return None;
            }
            use futures_util::StreamExt;
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(Some(chunk)) => Some((chunk.map_err(std::io::Error::other), (stream, false))),
                Ok(None) => None,
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "upload body deadline reached",
                    )),
                    (stream, true),
                )),
            }
        },
    ));
    // Detaching the waiter must not detach result registration from a backend
    // publication which may already own an independent commit task.
    tokio::spawn(async move {
        let mut execution = execution;
        let upload_result = async {
            state
                .traffic
                .preflight(
                    &traffic_subject,
                    crate::traffic::Direction::Upload,
                    expected_bytes.unwrap_or(1),
                )
                .await?;
            let (body, meter) =
                state
                    .traffic
                    .meter(body, traffic_subject, crate::traffic::Direction::Upload);
            let mut input =
                crate::s3_backend::UploadInput::relay(state.upload_limiter.wrap_body(body));
            input.operation_id = operation_id;
            let result = backend
                .upload_tracked_new_file(
                    &storage_path,
                    input,
                    expected_bytes,
                    max_upload_bytes,
                    content_type.as_deref(),
                )
                .await;
            meter.finish(result)
        }
        .await;
        if let Some(execution) = &mut execution {
            let started = std::time::Instant::now();
            execution.finish(&upload_result).await;
            if started.elapsed() >= std::time::Duration::from_secs(1) {
                tracing::info!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "slow upload result persistence"
                );
            }
        }
        upload_result?;
        Ok(Json(
            serde_json::json!({ "success": true, "uploaded": [file_name] }),
        ))
    })
    .await
    .map_err(|error| {
        AppError::with_source("upload execution failed", error).with_operation(
            crate::error::CommitState::Unknown,
            crate::error::CleanupState::Unknown,
        )
    })?
}

pub async fn download_file(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<axum::response::Response> {
    file_response(state, headers, query, FileResponseMode::Attachment, method).await
}

async fn file_response(
    state: AppState,
    headers: HeaderMap,
    query: FileQuery,
    mode: FileResponseMode,
    method: Method,
) -> AppResult<axum::response::Response> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Download).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    let request_path = query.path.as_deref().unwrap_or("");
    check_folder_locks(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, request_path),
    )
    .await?;
    let file = share_storage_path(&share, request_path);
    let response = backend.stream_file(&file, &headers, mode, &method).await?;
    let subject = crate::traffic::browser_subject(&state, &headers).await;
    Ok(state
        .download_limiter
        .wrap_response(state.traffic.download(response, subject).await?))
}

pub async fn delete_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_write_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Delete).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    let request_path = query.path.as_deref().unwrap_or("");
    ensure_non_root(request_path)?;
    check_folder_lock_tree(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, request_path),
    )
    .await?;
    backend
        .remove(&share_storage_path(&share, request_path))
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn rename_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<RenameBody>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_write_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Rename).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    ensure_non_root(&body.path)?;
    check_folder_lock_tree(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, &body.path),
    )
    .await?;
    let new_name = sanitize_name(&body.new_name);
    if new_name.is_empty() {
        return Err(AppError::BadRequest("File name is required".into()));
    }
    let destination_request_path = body
        .path
        .trim_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| join_request_path(parent, &new_name))
        .unwrap_or_else(|| new_name.clone());
    check_folder_lock_tree(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, &destination_request_path),
    )
    .await?;
    backend
        .move_path(
            &share_storage_path(&share, &body.path),
            &share_storage_path(&share, &destination_request_path),
        )
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn preview_file(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<axum::response::Response> {
    file_response(state, headers, query, FileResponseMode::Preview, method).await
}

/// POST /api/folder/unlock — verify a folder lock password and return a token cookie.
pub async fn unlock_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<LockBody>,
) -> AppResult<axum::response::Response> {
    let share = resolve_share(&state, &headers, &query).await?;
    let storage_path = share_storage_path(&share, &body.path);
    let lock = {
        let config = state.config_file.read().await;
        config
            .folder_locks
            .iter()
            .filter(|lock| lock.matches(&share.storage_id, &storage_path))
            .max_by_key(|lock| lock.path.trim_matches('/').split('/').count())
            .map(|lock| (lock.id.clone(), lock.password_hash.clone()))
    };
    let Some((lock_id, password_hash)) = lock else {
        return Ok(Json(
            serde_json::json!({ "success": false, "message": "No lock on this path" }),
        )
        .into_response());
    };
    if body.password.len() > 1_024
        || !state
            .passwords
            .verify(password_hash.clone(), body.password)
            .await
    {
        return Ok(
            Json(serde_json::json!({ "success": false, "message": "Wrong password" }))
                .into_response(),
        );
    }

    let auth_guard = state.auth_transitions.lock().await;
    let lock_is_current = state
        .config_file
        .read()
        .await
        .folder_locks
        .iter()
        .any(|lock| {
            lock.id == lock_id
                && lock.password_hash == password_hash
                && lock.matches(&share.storage_id, &storage_path)
        });
    if !lock_is_current {
        drop(auth_guard);
        return Ok(
            Json(serde_json::json!({ "success": false, "message": "Wrong password" }))
                .into_response(),
        );
    }
    let mut response =
        Json(serde_json::json!({ "success": true, "message": "Folder unlocked" })).into_response();
    let token = state.folder_access.create(lock_id.clone()).await;
    drop(auth_guard);
    let cookie = session_cookie(
        &format!("folder_key_{lock_id}"),
        &token,
        24 * 60 * 60,
        state.config.secure_cookies,
    );
    let value = header::HeaderValue::from_str(&cookie)
        .map_err(|error| AppError::with_source("failed to build lock cookie", error))?;
    response.headers_mut().append(header::SET_COOKIE, value);
    Ok(response)
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    #[test]
    fn directory_cursor_is_opaque_and_round_trips() {
        let token = DirectoryCursorToken {
            version: DIRECTORY_CURSOR_VERSION,
            storage_id: "primary".into(),
            directory: "docs".into(),
            search: Some("log".into()),
            sort: DirectorySort::Time,
            direction: SortDirection::Desc,
            filter: DirectoryEntryFilter::Gallery,
            delivered: 40,
            position: EntryPosition::from(&crate::directory_listing::BackendEntry {
                name: "server.log".into(),
                relative: "docs/server.log".into(),
                is_dir: false,
                size: 10,
                modified_unix: Some(1),
            }),
        };
        let cursor = encode_cursor(&token).unwrap();
        assert_ne!(cursor, "40");
        let decoded = decode_cursor(
            Some(&cursor),
            "primary",
            "docs",
            Some("LOG"),
            DirectorySort::Time,
            SortDirection::Desc,
            DirectoryEntryFilter::Gallery,
        )
        .unwrap()
        .unwrap();
        assert_eq!(decoded.delivered, 40);
        assert!(decode_cursor(
            Some(&cursor),
            "primary",
            "docs",
            Some("LOG"),
            DirectorySort::Time,
            SortDirection::Desc,
            DirectoryEntryFilter::All,
        )
        .is_err());
        assert!(decode_cursor(
            Some("not-a-cursor"),
            "primary",
            "docs",
            None,
            DirectorySort::Name,
            SortDirection::Asc,
            DirectoryEntryFilter::All,
        )
        .is_err());
        assert!(decode_cursor(
            Some(&"x".repeat(MAX_DIRECTORY_CURSOR_BYTES + 1)),
            "primary",
            "docs",
            None,
            DirectorySort::Name,
            SortDirection::Asc,
            DirectoryEntryFilter::All,
        )
        .is_err());
    }

    #[tokio::test]
    async fn navigation_is_permission_scoped_and_admin_can_see_disabled_storages() {
        use crate::config::{ConfigFile, StoragePermission, UserAccount};
        let directory = crate::test_support::TestDirectory::new("storage-navigation");
        let state =
            crate::test_support::app_state(&directory, ConfigFile::with_test_storage()).await;
        {
            let mut config = state.config_file.write().await;
            let mut private = config.storage_instances[0].clone();
            private.id = "private".into();
            private.allow_guest_access = false;
            let mut disabled = private.clone();
            disabled.id = "disabled".into();
            disabled.enabled = false;
            config.storage_instances.extend([private, disabled]);
            config.user_accounts.push(UserAccount {
                id: "alice".into(),
                username: "alice".into(),
                password_hash: "unused".into(),
                enabled: true,
                permissions: vec![StoragePermission {
                    storage_id: "private".into(),
                    browse: true,
                    ..Default::default()
                }],
            });
        }
        let mut headers = HeaderMap::new();
        let gate = state.gate_access.create("__gate__".into()).await;
        headers.insert(
            axum::http::header::COOKIE,
            format!("gate_access={gate}").parse().unwrap(),
        );
        let guest = browser_storage_views(&state, &headers).await;
        assert_eq!(
            guest
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["primary"]
        );
        let token = state.sessions.create_user("alice".into()).await;
        headers.insert(
            axum::http::header::COOKIE,
            format!("session={token}").parse().unwrap(),
        );
        let user = browser_storage_views(&state, &headers).await;
        assert_eq!(
            user.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["private"]
        );
        assert_eq!(
            browser_selection_scope(&state, &headers).await,
            "user:alice"
        );
        let token = state.sessions.create().await;
        headers.insert(
            axum::http::header::COOKIE,
            format!("session={token}").parse().unwrap(),
        );
        let admin = browser_storage_views(&state, &headers).await;
        assert_eq!(admin.len(), 3);
        assert!(!admin[2].enabled);
        assert!(!admin[2].ready);
        assert!(resolve_share(
            &state,
            &headers,
            &FileQuery {
                storage_id: Some("disabled".into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn zero_storage_listing_is_an_explicit_non_writable_empty_state() {
        let directory = crate::test_support::TestDirectory::new("empty-storage-list");
        let state =
            crate::test_support::app_state(&directory, crate::config::ConfigFile::default()).await;
        let token = state.sessions.create().await;
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("session={token}").parse().unwrap(),
        );
        let query = serde_json::from_value(serde_json::json!({})).unwrap();
        let Json(list) = list_files(State(state.clone()), headers, Query(query))
            .await
            .unwrap();
        assert_eq!(list.empty_reason, Some("unconfigured"));
        assert_eq!(list.selection_scope, "administrator");
        assert!(list.storages.is_empty());
        assert!(list.entries.is_empty());
        assert!(list.storage_id.is_empty());
        assert!(!list.capabilities.upload);
        assert!(!list.capabilities.download);
        assert!(state.config_file.read().await.storage_instances.is_empty());
    }

    #[test]
    fn directory_page_size_rejects_unlisted_values() {
        assert_eq!(page_size(None).unwrap(), 20);
        assert_eq!(page_size(Some(100)).unwrap(), 100);
        assert!(page_size(Some(25)).is_err());
    }
}

#[cfg(test)]
mod response_tests {
    use super::{download_file, preview_file};
    use crate::{
        file_access::FileQuery,
        test_support::{app_state, TestDirectory},
    };
    use axum::{
        body::to_bytes,
        extract::{Query, State},
        http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    };

    #[tokio::test]
    async fn download_and_preview_share_access_but_keep_their_response_modes() {
        let directory = TestDirectory::new("browser-file-response");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        tokio::fs::write(state.config.storage_path.join("note.txt"), b"file content")
            .await
            .unwrap();
        let token = state.sessions.create().await;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("session={token}")).unwrap(),
        );
        let query = || FileQuery {
            path: Some("note.txt".into()),
            storage_id: Some(crate::config::DEFAULT_STORAGE_ID.into()),
            batch: None,
        };

        assert!(download_file(
            State(state.clone()),
            Method::GET,
            HeaderMap::new(),
            Query(query())
        )
        .await
        .is_err());
        assert!(preview_file(
            State(state.clone()),
            Method::GET,
            HeaderMap::new(),
            Query(query())
        )
        .await
        .is_err());

        let download = download_file(
            State(state.clone()),
            Method::GET,
            headers.clone(),
            Query(query()),
        )
        .await
        .unwrap();
        assert_eq!(download.status(), StatusCode::OK);
        assert!(download.headers().contains_key(header::CONTENT_DISPOSITION));
        assert_eq!(
            to_bytes(download.into_body(), 1024).await.unwrap(),
            "file content"
        );

        let preview = preview_file(State(state), Method::GET, headers, Query(query()))
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::OK);
        assert!(!preview.headers().contains_key(header::CONTENT_DISPOSITION));
        assert_eq!(
            to_bytes(preview.into_body(), 1024).await.unwrap(),
            "file content"
        );
    }
}
