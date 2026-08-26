use std::{cmp::Ordering, path::PathBuf};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::file_access::{
    check_folder_locks, ensure_non_root, ensure_storage_action, ensure_writable, join_request_path,
    resolve_share, share_storage_path, storage_permission, FileQuery, FolderLockAuthorizer,
    StorageAction,
};
use crate::security::session_cookie;
use crate::state::AppState;
use crate::storage::FileResponseMode;

// ── Request / response types ──────────────────────────────────────

const DEFAULT_PAGE_SIZE: usize = 20;

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
    pub current_path: String,
    pub parent_path: Option<String>,
    pub entries: Vec<FileEntry>,
    pub page_start: usize,
    pub page_size: usize,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub can_write: bool,
    pub is_admin: bool,
    pub capabilities: BrowserCapabilities,
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DirectorySort {
    #[default]
    Name,
    Size,
    Time,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortDirection {
    #[default]
    Asc,
    Desc,
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
    sort: DirectorySort,
    #[serde(default)]
    direction: SortDirection,
}

impl FileListQuery {
    fn file_query(&self) -> FileQuery {
        FileQuery {
            path: self.path.clone(),
            storage_id: self.storage_id.clone(),
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

fn encode_cursor(offset: usize) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_string())
}

fn decode_cursor(cursor: Option<&str>) -> AppResult<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::BadRequest("Invalid directory cursor".into()))?;
    std::str::from_utf8(&decoded)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| AppError::BadRequest("Invalid directory cursor".into()))
}

fn compare_entries(
    left: &FileEntry,
    right: &FileEntry,
    sort: DirectorySort,
    direction: SortDirection,
) -> Ordering {
    let kind_order = right.is_dir.cmp(&left.is_dir);
    if kind_order != Ordering::Equal {
        return kind_order;
    }
    let primary = match sort {
        DirectorySort::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        DirectorySort::Size => left.size.cmp(&right.size),
        DirectorySort::Time => left.modified.cmp(&right.modified),
    };
    let primary = match direction {
        SortDirection::Asc => primary,
        SortDirection::Desc => primary.reverse(),
    };
    primary.then_with(|| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.path.cmp(&right.path))
    })
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
}

async fn browser_storage_views(state: &AppState, headers: &HeaderMap) -> Vec<BrowserStorageView> {
    let storage_configs = state.config_file.read().await.storage_instances.clone();
    let mut storages = Vec::with_capacity(storage_configs.len());
    for storage in storage_configs {
        if !storage.enabled || !state.backends.is_ready(&storage.id).await {
            continue;
        }
        let can_browse = storage_permission(state, headers, &storage.id)
            .await
            .is_some_and(|permission| permission.browse);
        storages.push(BrowserStorageView {
            id: storage.id,
            name: storage.name,
            requires_login: !can_browse,
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
    let file_query = query.file_query();
    let share = resolve_share(&state, &headers, &file_query).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    let request_path = query.path.as_deref().unwrap_or("");
    let storage_directory = share_storage_path(&share, request_path);
    if !backend.metadata(&storage_directory).await?.is_dir {
        return Err(AppError::NotFound);
    }
    let lock_authorizer = FolderLockAuthorizer::new(&state, &headers, &share.storage_id).await;
    lock_authorizer.ensure_access(&share_storage_path(&share, request_path))?;
    let page_size = page_size(query.limit)?;
    let requested_offset = decode_cursor(query.cursor.as_deref())?;

    let limit = state.config.max_list_entries;
    let (backend_entries, truncated) = backend.list_directory(&storage_directory, limit).await?;
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

    if let Some(search) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let search = search.to_lowercase();
        entries.retain(|entry| entry.name.to_lowercase().contains(&search));
    }
    entries.sort_by(|left, right| compare_entries(left, right, query.sort, query.direction));

    let page_offset = requested_offset.min(entries.len());
    let page_end = page_offset.saturating_add(page_size).min(entries.len());
    let next_cursor = (page_end < entries.len()).then(|| encode_cursor(page_end));
    let entries = entries[page_offset..page_end].to_vec();
    let page_start = (!entries.is_empty())
        .then_some(page_offset + 1)
        .unwrap_or(0);

    let current_path = request_path.to_string();
    let parent_path = PathBuf::from(&current_path)
        .parent()
        .and_then(|p| p.to_str())
        .map(|s| s.to_string());

    let permission = storage_permission(&state, &headers, &share.storage_id).await;
    let (max_upload_bytes, max_archive_bytes, max_archive_entries) = {
        let config = state.config_file.read().await;
        (
            config.max_upload_bytes,
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
        storages,
        current_path,
        parent_path,
        entries,
        page_start,
        page_size,
        next_cursor,
        truncated,
        can_write: !share.readonly && can_write,
        is_admin,
        capabilities,
        max_upload_bytes,
        max_archive_bytes,
        max_archive_entries,
    }))
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64, modified: &str) -> FileEntry {
        FileEntry {
            name: name.into(),
            path: name.into(),
            is_dir,
            size,
            modified: modified.into(),
            mime: String::new(),
            icon: String::new(),
            locked: false,
        }
    }

    #[test]
    fn directory_cursor_is_opaque_and_round_trips() {
        let cursor = encode_cursor(40);
        assert_ne!(cursor, "40");
        assert_eq!(decode_cursor(Some(&cursor)).unwrap(), 40);
        assert!(decode_cursor(Some("not-a-cursor")).is_err());
    }

    #[test]
    fn directory_sort_keeps_folders_first_and_orders_files() {
        let mut entries = vec![
            entry("small.txt", false, 2, "2026-01-01 00:00"),
            entry("folder", true, 0, "2026-01-01 00:00"),
            entry("large.txt", false, 9, "2026-01-02 00:00"),
        ];
        entries.sort_by(|left, right| {
            compare_entries(left, right, DirectorySort::Size, SortDirection::Desc)
        });
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["folder", "large.txt", "small.txt"]
        );
    }

    #[test]
    fn directory_page_size_rejects_unlisted_values() {
        assert_eq!(page_size(None).unwrap(), 20);
        assert_eq!(page_size(Some(100)).unwrap(), 100);
        assert!(page_size(Some(25)).is_err());
    }
}

pub async fn create_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<MkdirBody>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
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
    let share = resolve_share(&state, &headers, &query).await?;
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
        .and_then(|value| value.to_str().ok());
    let body = state.upload_limiter.wrap_body(body);
    let max_upload_bytes = state.config_file.read().await.max_upload_bytes;
    backend
        .upload_file(
            &storage_path,
            body,
            expected_bytes,
            max_upload_bytes,
            content_type,
        )
        .await?;
    Ok(Json(
        serde_json::json!({ "success": true, "uploaded": [file_name] }),
    ))
}

pub async fn download_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
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
    let response = backend
        .stream_file(&file, &headers, FileResponseMode::Attachment)
        .await?;
    Ok(state.download_limiter.wrap_response(response))
}

pub async fn delete_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Delete).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    let request_path = query.path.as_deref().unwrap_or("");
    ensure_non_root(request_path)?;
    check_folder_locks(
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
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Rename).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    ensure_non_root(&body.path)?;
    check_folder_locks(
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
    check_folder_locks(
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
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
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
    let response = backend
        .stream_file(&file, &headers, FileResponseMode::Preview)
        .await?;
    Ok(state.download_limiter.wrap_response(response))
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
    if body.password.len() > 1_024 || !state.passwords.verify(password_hash, body.password).await {
        return Ok(
            Json(serde_json::json!({ "success": false, "message": "Wrong password" }))
                .into_response(),
        );
    }

    let mut response =
        Json(serde_json::json!({ "success": true, "message": "Folder unlocked" })).into_response();
    let token = state.folder_access.create(lock_id.clone()).await;
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
