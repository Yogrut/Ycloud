use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
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

#[derive(Serialize)]
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
    pub truncated: bool,
    pub can_write: bool,
    pub is_admin: bool,
    pub capabilities: BrowserCapabilities,
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
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
    pub is_default: bool,
    pub ready: bool,
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
    Query(query): Query<FileQuery>,
) -> AppResult<Json<ListResponse>> {
    let share = resolve_share(&state, &headers, &query).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    let request_path = query.path.as_deref().unwrap_or("");
    let storage_directory = share_storage_path(&share, request_path);
    if !backend.metadata(&storage_directory).await?.is_dir {
        return Err(AppError::NotFound);
    }
    let lock_authorizer = FolderLockAuthorizer::new(&state, &headers, &share.storage_id).await;
    lock_authorizer.ensure_access(&share_storage_path(&share, request_path))?;

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

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let current_path = request_path.to_string();
    let parent_path = PathBuf::from(&current_path)
        .parent()
        .and_then(|p| p.to_str())
        .map(|s| s.to_string());

    let permission = storage_permission(&state, &headers, &share.storage_id).await;
    let (max_upload_bytes, max_archive_bytes, max_archive_entries, storage_configs, default_id) = {
        let config = state.config_file.read().await;
        (
            config.max_upload_bytes,
            config.max_archive_bytes,
            config.max_archive_entries,
            config.storage_instances.clone(),
            config.default_storage_id.clone(),
        )
    };
    let principal = crate::auth::current_principal(&state, &headers).await;
    let is_admin = matches!(
        principal,
        Some(crate::auth::SessionPrincipal::Administrator)
    );
    let mut storages = Vec::new();
    if principal.is_some() {
        storages.reserve(storage_configs.len());
        for storage in storage_configs {
            if storage_permission(&state, &headers, &storage.id)
                .await
                .is_none_or(|permission| !permission.browse)
            {
                continue;
            }
            storages.push(BrowserStorageView {
                id: storage.id.clone(),
                name: storage.name,
                is_default: storage.id == default_id,
                ready: state.backends.is_ready(&storage.id).await,
            });
        }
    }
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
        truncated,
        can_write: !share.readonly && can_write,
        is_admin,
        capabilities,
        max_upload_bytes,
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
