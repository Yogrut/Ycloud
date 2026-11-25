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
    normalized_search, DirectoryEntryFilter, DirectoryListRequest, DirectorySort, EntryPosition,
    SortDirection,
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
    pub modified_unix: Option<i64>,
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
        // A disabled instance is not offered anywhere; every request path also
        // rejects it through AppState::storage_backend.
        if !storage.enabled {
            continue;
        }
        let can_browse = storage_permission(state, headers, &storage.id)
            .await
            .is_some_and(|permission| permission.browse);
        if !administrator && !can_browse {
            continue;
        }
        let ready = state.backends.is_ready(&storage.id).await;
        storages.push(BrowserStorageView {
            id: storage.id,
            name: storage.name,
            enabled: true,
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

fn get_icon(name: &str, is_dir: bool, mime: &str) -> String {
    if is_dir {
        return "folder".into();
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "jfif" | "png" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif" => {
            "image"
        }
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
    let page_size = page_size(query.limit)?;
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
            modified_unix: entry.modified_unix,
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
    let name = crate::storage::StorageService::validate_name(&body.name)?;
    let new_path = join_request_path(request_path, name);
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
    let file_name = crate::storage::StorageService::validate_name(
        file_request_path.rsplit('/').next().unwrap_or(""),
    )?
    .to_owned();
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
    let body = crate::transfer_limit::upload_body_with_deadline(
        body,
        std::time::Duration::from_secs(state.config.upload_timeout_secs),
    );
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
    let (_, backend, file) = authorized_file_read(&state, &headers, &query).await?;
    let response = backend.stream_file(&file, &headers, mode, &method).await?;
    meter_download(&state, &headers, response).await
}

async fn authorized_file_read(
    state: &AppState,
    headers: &HeaderMap,
    query: &FileQuery,
) -> AppResult<(String, crate::storage_backend::StorageBackend, String)> {
    let share = resolve_share(state, headers, query).await?;
    ensure_storage_action(state, headers, &share.storage_id, StorageAction::Download).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    let file = share_storage_path(&share, query.path.as_deref().unwrap_or(""));
    check_folder_locks(state, headers, &share.storage_id, &file).await?;
    Ok((share.storage_id, backend, file))
}

async fn meter_download(
    state: &AppState,
    headers: &HeaderMap,
    response: axum::response::Response,
) -> AppResult<axum::response::Response> {
    let subject = crate::traffic::browser_subject(state, headers).await;
    Ok(state
        .download_limiter
        .wrap_response(state.traffic.download(response, subject).await?))
}

pub async fn thumbnail_file(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<axum::response::Response> {
    let (id, backend, file) = authorized_file_read(&state, &headers, &query).await?;
    let response = state
        .gallery
        .response(&backend, &id, &file, &method)
        .await?;
    if response.status() == axum::http::StatusCode::OK {
        meter_download(&state, &headers, response).await
    } else {
        Ok(response)
    }
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
    let new_name = crate::storage::StorageService::validate_name(&body.new_name)?;
    let destination_request_path = body
        .path
        .trim_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| join_request_path(parent, new_name))
        .unwrap_or_else(|| new_name.to_owned());
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

/// Server-sent change signals for one directory of one storage. The stream
/// carries no file names: a browser that receives one re-reads the page it is
/// already showing.
pub async fn directory_changes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<axum::response::Response> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Browse).await?;
    // One stream per browser credential: the token is hashed, never stored raw.
    let subject = crate::auth::current_request_subject(&state, &headers)
        .await
        .ok_or(AppError::Unauthorized)?;
    let token = match subject {
        crate::auth::RequestSubject::Session(token) | crate::auth::RequestSubject::Gate(token) => {
            token
        }
    };
    let token_permit = state
        .changes_tokens
        .try_enter(&token)
        .ok_or_else(|| AppError::ServiceUnavailable("实时同步连接数已满，请稍后重试".into()))?;
    let storage = share.storage_id.clone();
    let dir = query
        .path
        .as_deref()
        .unwrap_or("")
        .trim_matches('/')
        .to_owned();
    // Same folder-lock gate as the listing: a locked directory needs its key,
    // otherwise the caller could watch activity inside it.
    check_folder_locks(
        &state,
        &headers,
        &share.storage_id,
        &share_storage_path(&share, &dir),
    )
    .await?;
    // One permit per live stream: a client that opens too many gets a clear
    // refusal instead of an unbounded pile of connections.
    let permit = state
        .changes_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::ServiceUnavailable("实时同步连接数已满，请稍后重试".into()))?;
    let receiver = crate::events::subscribe();
    // The stream closes after a fixed age so every client re-authenticates on its
    // automatic reconnect; dead connections (crashed tabs, lost networks) are
    // reclaimed this way. A one-minute jitter keeps a restart from making every
    // client reconnect at the same instant.
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::from(elapsed.subsec_nanos()) % 120)
        .unwrap_or(0);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(540 + jitter);
    let stream = futures_util::stream::unfold(
        (receiver, storage, dir, (permit, token_permit), deadline),
        |(mut receiver, storage, dir, permits, deadline)| async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => return None,
                    received = receiver.recv() => match received {
                        Ok(change) => {
                            if change.storage == storage && change.dir == dir {
                                let event = axum::response::sse::Event::default()
                                    .event("change")
                                    .data("1");
                                return Some((
                                    Ok::<_, std::convert::Infallible>(event),
                                    (receiver, storage, dir, permits, deadline),
                                ));
                            }
                        }
                        // A slow consumer missed signals; tell it to resync
                        // instead of silently skipping them.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let event = axum::response::sse::Event::default()
                                .event("change")
                                .data("resync");
                            return Some((
                                Ok::<_, std::convert::Infallible>(event),
                                (receiver, storage, dir, permits, deadline),
                            ));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    },
                }
            }
        },
    );
    // Proxies must not buffer the stream, and a change signal is never cacheable.
    let mut response = axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-accel-buffering"),
        axum::http::HeaderValue::from_static("no"),
    );
    Ok(response)
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
mod changes_auth_tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    use crate::{
        config::ConfigFile,
        test_support::{app_state, TestDirectory},
    };

    // `oneshot` builds a relative URI, so the request needs a Host: the router
    // rejects host-less requests with 400 before any handler runs.
    fn subscription(token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .uri("/api/changes?path=&storage_id=primary")
            .header(header::HOST, "localhost");
        if let Some(token) = token {
            builder = builder.header(header::COOKIE, format!("session={token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn fixture() -> (TestDirectory, Router, crate::state::AppState) {
        let directory = TestDirectory::new("changes-auth");
        let state = app_state(&directory, ConfigFile::with_test_storage()).await;
        (directory, crate::app::build_router(state.clone()), state)
    }

    #[tokio::test]
    async fn rejects_a_subscription_without_credentials() {
        let (_directory, app, _state) = fixture().await;
        let response = app.oneshot(subscription(None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    // The test configuration keeps the site gate on, so a session alone is not
    // enough: the gate cookie must be present too. Proving the 200 path needs a
    // gate token, which this fixture does not mint yet.
    #[tokio::test]
    async fn refuses_a_session_subscription_without_the_site_gate() {
        let (_directory, app, state) = fixture().await;
        let token = state.sessions.create().await;
        let response = app.oneshot(subscription(Some(&token))).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
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
    async fn navigation_is_permission_scoped_and_disabled_storages_are_hidden() {
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
        assert_eq!(
            admin
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["primary", "private"]
        );
        assert!(admin.iter().all(|item| item.enabled));
        // The hidden instance is still rejected on every request path.
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
    async fn gallery_honors_each_supported_page_size_and_cursor() {
        let directory = TestDirectory::new("gallery-selected-page-size");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        for index in 0..113 {
            tokio::fs::write(
                state
                    .config
                    .storage_path
                    .join(format!("photo-{index:03}.jpg")),
                b"fixture",
            )
            .await
            .unwrap();
        }
        let token = state.sessions.create().await;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("session={token}")).unwrap(),
        );
        for limit in [10, 20, 50, 100] {
            let query = |cursor: Option<String>| {
                serde_json::from_value::<super::FileListQuery>(serde_json::json!({
                "storage_id": crate::config::DEFAULT_STORAGE_ID, "gallery": true, "limit": limit, "cursor": cursor
            })).unwrap()
            };
            let first =
                super::list_files(State(state.clone()), headers.clone(), Query(query(None)))
                    .await
                    .unwrap()
                    .0;
            assert_eq!(first.page_size, limit);
            assert_eq!(first.entries.len(), limit);
            let second = super::list_files(
                State(state.clone()),
                headers.clone(),
                Query(query(first.next_cursor)),
            )
            .await
            .unwrap()
            .0;
            assert_eq!(second.page_start, limit + 1);
            assert_eq!(second.entries.len(), limit.min(113 - limit));
            assert!(second
                .entries
                .iter()
                .all(|item| !first.entries.iter().any(|old| old.path == item.path)));
        }
    }

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

        let preview = preview_file(State(state.clone()), Method::GET, headers, Query(query()))
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

#[cfg(test)]
mod permission_matrix_tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    use crate::{
        config::{ConfigFile, FolderLock, StoragePermission, UserAccount},
        test_support::{app_state, TestDirectory},
    };

    // The router validates Host against the configured port before any handler.
    const HOST: &str = "localhost:18473";

    fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri(uri).header(header::HOST, HOST);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn mkdir(cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/mkdir?storage_id=primary")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            // A browser always sends Origin; CSRF requires it to match the host.
            .header(header::ORIGIN, format!("http://{HOST}"));
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        // The empty name guarantees that a request which reaches the handler
        // cannot create anything. A 403 therefore means an earlier layer refused
        // it; the companion test with create_directory granted expects 400 and
        // proves the difference comes from the permission, not the payload.
        builder.body(Body::from(r#"{"name":""}"#)).unwrap()
    }

    fn unlock(cookie: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/folder/unlock?storage_id=primary")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header(header::COOKIE, cookie)
            .body(Body::from(
                r#"{"path":"locked","password":"lock-password"}"#,
            ))
            .unwrap()
    }

    fn download(path: &str, cookie: &str) -> Request<Body> {
        Request::builder()
            .uri(format!("/api/download?storage_id=primary&path={path}"))
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    }

    async fn fixture(
        label: &str,
        config: ConfigFile,
    ) -> (TestDirectory, Router, crate::state::AppState) {
        let directory = TestDirectory::new(label);
        let state = app_state(&directory, config).await;
        let router = crate::app::build_router(state.clone());
        (directory, router, state)
    }

    #[tokio::test]
    async fn anonymous_requests_are_rejected_before_any_handler_runs() {
        let (_directory, app, _state) = fixture("perm-anon", ConfigFile::with_test_storage()).await;

        let list = app
            .clone()
            .oneshot(get("/api/files?storage_id=primary", None))
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::UNAUTHORIZED);

        let write = app.oneshot(mkdir(None)).await.unwrap();
        assert_eq!(write.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn gate_reads_but_cannot_write_or_enter_admin() {
        let (_directory, app, state) = fixture("perm-gate", ConfigFile::with_test_storage()).await;
        let token = state.gate_access.create("__gate__".into()).await;
        let cookie = format!("gate_access={token}");

        let list = app
            .clone()
            .oneshot(get("/api/files?storage_id=primary", Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);

        let write = app.clone().oneshot(mkdir(Some(&cookie))).await.unwrap();
        assert_eq!(write.status(), StatusCode::FORBIDDEN);

        let admin = app
            .oneshot(get("/api/admin/info", Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(admin.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_only_user_reads_but_cannot_write() {
        let mut config = ConfigFile::with_test_storage();
        config.user_accounts.push(UserAccount {
            id: "reader".into(),
            username: "reader".into(),
            password_hash: crate::config::hash_password("reader-password"),
            enabled: true,
            permissions: vec![StoragePermission {
                storage_id: "primary".into(),
                browse: true,
                download: true,
                ..Default::default()
            }],
        });
        let (_directory, app, state) = fixture("perm-read-only-user", config).await;
        let token = state.sessions.create_user("reader".into()).await;
        let cookie = format!("session={token}");

        let list = app
            .clone()
            .oneshot(get("/api/files?storage_id=primary", Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);

        let write = app.clone().oneshot(mkdir(Some(&cookie))).await.unwrap();
        assert_eq!(write.status(), StatusCode::FORBIDDEN);

        let admin = app
            .oneshot(get("/api/admin/info", Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(admin.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn write_refusal_comes_from_the_permission_not_the_payload() {
        let mut config = ConfigFile::with_test_storage();
        config.user_accounts.push(UserAccount {
            id: "writer".into(),
            username: "writer".into(),
            password_hash: crate::config::hash_password("writer-password"),
            enabled: true,
            permissions: vec![StoragePermission {
                storage_id: "primary".into(),
                browse: true,
                create_directory: true,
                ..Default::default()
            }],
        });
        let (_directory, app, state) = fixture("perm-writer", config).await;
        let token = state.sessions.create_user("writer".into()).await;
        let cookie = format!("session={token}");

        // Same request as the read-only test, now allowed past the permission
        // check: the empty name reaches validation and yields 400, not 403.
        let response = app.oneshot(mkdir(Some(&cookie))).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn folder_locks_match_equivalent_path_spellings_and_reject_traversal() {
        let mut config = ConfigFile::with_test_storage();
        config.folder_locks.push(FolderLock {
            id: "lock-1".into(),
            storage_id: "primary".into(),
            path: "locked".into(),
            password_hash: crate::config::hash_password("lock-password"),
        });
        let (directory, app, state) = fixture("perm-lock-paths", config).await;
        std::fs::create_dir_all(directory.path().join("storage/locked/sub")).unwrap();
        std::fs::create_dir_all(directory.path().join("storage/open")).unwrap();
        std::fs::write(
            directory.path().join("storage/locked/file.txt"),
            b"locked-content",
        )
        .unwrap();
        let token = state.gate_access.create("__gate__".into()).await;
        let cookie = format!("gate_access={token}");

        // Dot segments and empty segments are not a way past a lock: the matcher
        // normalizes them the same way the storage layer does.
        for path in [
            "locked",
            "%2Flocked",
            "//locked",
            "./locked",
            "locked//sub",
            "locked/./sub",
        ] {
            let response = app
                .clone()
                .oneshot(get(
                    &format!("/api/files?storage_id=primary&path={path}"),
                    Some(&cookie),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }

        let open = app
            .clone()
            .oneshot(get(
                "/api/files?storage_id=primary&path=open",
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(open.status(), StatusCode::OK);

        // Traversal never reaches the lock matcher as an equivalent path; the
        // storage layer rejects it outright.
        let traversal = app
            .clone()
            .oneshot(get(
                "/api/files?storage_id=primary&path=..%2Foutside",
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);

        // Downloads take the shared file-read path, not the listing path.
        let locked_file = app
            .clone()
            .oneshot(download("./locked/file.txt", &cookie))
            .await
            .unwrap();
        assert_eq!(locked_file.status(), StatusCode::FORBIDDEN);

        // The unlock endpoint issues the folder cookie that clears the lock.
        let unlocked = app.clone().oneshot(unlock(&cookie)).await.unwrap();
        assert_eq!(unlocked.status(), StatusCode::OK);
        let folder_cookie = unlocked
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("unlock must set a folder cookie")
            .to_string();
        let unlocked_file = app
            .oneshot(download(
                "./locked/file.txt",
                &format!("{cookie}; {folder_cookie}"),
            ))
            .await
            .unwrap();
        assert_eq!(unlocked_file.status(), StatusCode::OK);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_alias_spellings_cannot_bypass_folder_locks() {
        let mut config = ConfigFile::with_test_storage();
        for (index, path) in ["locked", "Ärzte"].into_iter().enumerate() {
            config.folder_locks.push(FolderLock {
                id: format!("lock-{index}"),
                storage_id: "primary".into(),
                path: path.into(),
                password_hash: crate::config::hash_password("lock-password"),
            });
        }
        let (directory, app, state) = fixture("perm-windows-locks", config).await;
        std::fs::create_dir_all(directory.path().join("storage/locked")).unwrap();
        std::fs::create_dir_all(directory.path().join("storage/Ärzte")).unwrap();
        let token = state.gate_access.create("__gate__".into()).await;
        let cookie = format!("gate_access={token}");

        // Windows would strip these suffixes when opening the path; reject them
        // before the filesystem can resolve them onto the locked directory.
        for path in ["locked.", "locked%20"] {
            let response = app
                .clone()
                .oneshot(get(
                    &format!("/api/files?storage_id=primary&path={path}"),
                    Some(&cookie),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        }

        // NTFS folds case across the whole character set: the lock must match.
        for path in ["LOCKED", "%C3%A4rzte", "%C3%84RZTE"] {
            let response = app
                .clone()
                .oneshot(get(
                    &format!("/api/files?storage_id=primary&path={path}"),
                    Some(&cookie),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }
}
