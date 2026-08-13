use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{AppError, AppResult};
use crate::file_access::{
    check_folder_locks, ensure_non_root, ensure_writable, join_request_path, resolve_existing_path,
    resolve_share, resolve_write_path, share_storage_path, FileQuery, FolderLockAuthorizer,
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
    pub current_path: String,
    pub parent_path: Option<String>,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
    pub can_write: bool,
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_files: usize,
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
    let request_path = query.path.as_deref().unwrap_or("");
    let directory = resolve_existing_path(&state, &share, request_path).await?;
    if !state.storage.metadata(&directory).await?.is_dir() {
        return Err(AppError::NotFound);
    }
    let lock_authorizer = FolderLockAuthorizer::new(&state, &headers).await;
    lock_authorizer.ensure_access(&share_storage_path(&share, request_path))?;

    let mut read_dir = fs::read_dir(directory.absolute())
        .await
        .map_err(|error| AppError::with_source("failed to list directory", error))?;
    let mut entries = Vec::new();
    let limit = state.storage.max_list_entries();
    while entries.len() < limit {
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
        if crate::storage::is_link_or_reparse_point(&metadata) {
            tracing::warn!(path = %entry.path().display(), "skipping symbolic link in storage directory");
            continue;
        }
        let is_dir = metadata.is_dir();
        let size = metadata.len();
        let modified = metadata
            .modified()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.format("%Y-%m-%d %H:%M").to_string()
            })
            .unwrap_or_default();
        let share_relative = if request_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", request_path.trim_end_matches('/'), name)
        };
        let mime = if is_dir {
            "inode/directory".into()
        } else {
            mime_guess::from_path(entry.path())
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
    let truncated = entries.len() == limit
        && read_dir
            .next_entry()
            .await
            .map_err(|error| AppError::with_source("failed to read directory entry", error))?
            .is_some();

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

    Ok(Json(ListResponse {
        current_path,
        parent_path,
        entries,
        truncated,
        can_write: !share.readonly && crate::auth::is_admin_authenticated(&state, &headers).await,
        max_upload_bytes: state.storage.max_upload_bytes(),
        max_archive_bytes: crate::archive::MAX_ARCHIVE_BYTES,
        max_archive_files: crate::archive::MAX_ARCHIVE_FILES,
    }))
}

pub async fn create_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<MkdirBody>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_writable(&share)?;
    let request_path = query.path.as_deref().unwrap_or("");
    check_folder_locks(&state, &headers, &share_storage_path(&share, request_path)).await?;
    let name = sanitize_name(&body.name);
    if name.is_empty() {
        return Err(AppError::BadRequest("Directory name is required".into()));
    }
    let new_path = join_request_path(request_path, &name);
    check_folder_locks(&state, &headers, &share_storage_path(&share, &new_path)).await?;
    let directory = resolve_write_path(&state, &share, &new_path).await?;
    state.storage.create_directory(&directory).await?;
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
        &share_storage_path(&share, file_request_path),
    )
    .await?;
    let storage_path = share_storage_path(&share, file_request_path);
    let mut writer = match expected_bytes {
        Some(bytes) => {
            state
                .storage
                .begin_atomic_write_with_expected(&storage_path, bytes)
                .await?
        }
        None => state.storage.begin_atomic_write(&storage_path).await?,
    };
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| AppError::with_source("failed to read upload", error))?;
        writer.write_chunk(&chunk).await?;
    }
    writer.commit().await?;
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
    let request_path = query.path.as_deref().unwrap_or("");
    check_folder_locks(&state, &headers, &share_storage_path(&share, request_path)).await?;
    let file = resolve_existing_path(&state, &share, request_path).await?;
    state
        .storage
        .stream_file(&file, &headers, FileResponseMode::Attachment)
        .await
}

pub async fn delete_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_writable(&share)?;
    let request_path = query.path.as_deref().unwrap_or("");
    ensure_non_root(request_path)?;
    check_folder_locks(&state, &headers, &share_storage_path(&share, request_path)).await?;
    let target = resolve_existing_path(&state, &share, request_path).await?;
    state.storage.remove(&target).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn rename_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<RenameBody>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_writable(&share)?;
    ensure_non_root(&body.path)?;
    check_folder_locks(&state, &headers, &share_storage_path(&share, &body.path)).await?;
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
        &share_storage_path(&share, &destination_request_path),
    )
    .await?;
    let source = resolve_existing_path(&state, &share, &body.path).await?;
    let destination = resolve_write_path(&state, &share, &destination_request_path).await?;
    state.storage.move_path(&source, &destination).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn preview_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<axum::response::Response> {
    let share = resolve_share(&state, &headers, &query).await?;
    let request_path = query.path.as_deref().unwrap_or("");
    check_folder_locks(&state, &headers, &share_storage_path(&share, request_path)).await?;
    let file = resolve_existing_path(&state, &share, request_path).await?;
    state
        .storage
        .stream_file(&file, &headers, FileResponseMode::Preview)
        .await
}

/// POST /api/folder/unlock — verify a folder lock password and return a token cookie.
pub async fn unlock_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LockBody>,
) -> AppResult<axum::response::Response> {
    let share = resolve_share(&state, &headers, &FileQuery { path: None }).await?;
    let storage_path = share_storage_path(&share, &body.path);
    let lock = {
        let config = state.config_file.read().await;
        config
            .folder_locks
            .iter()
            .filter(|lock| lock.matches(&storage_path))
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
