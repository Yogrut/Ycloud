use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AppState,
    config::{FolderLock, Share},
    error::{AppError, AppResult},
};

#[derive(Serialize)]
pub struct AdminInfo {
    pub username: String,
    pub has_global_web_password: bool,
    pub shares: Vec<ShareView>,
    pub folder_locks: Vec<FolderLockView>,
}

/// [安全] Administrative responses expose password presence, never hashes.
#[derive(Clone, Serialize)]
pub struct ShareView {
    pub id: String,
    pub name: String,
    pub path: String,
    pub username: Option<String>,
    pub webdav_enabled: bool,
    pub has_password: bool,
    pub readonly: bool,
}

impl From<&Share> for ShareView {
    fn from(share: &Share) -> Self {
        Self {
            id: share.id.clone(),
            name: share.name.clone(),
            path: share.path.clone(),
            username: share.username.clone(),
            webdav_enabled: share.webdav_enabled,
            has_password: share.password_hash.is_some(),
            readonly: share.readonly,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct FolderLockView {
    pub id: String,
    pub path: String,
}

impl From<&FolderLock> for FolderLockView {
    fn from(lock: &FolderLock) -> Self {
        Self {
            id: lock.id.clone(),
            path: lock.path.clone(),
        }
    }
}

#[derive(Deserialize)]
pub struct CreateShareRequest {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub webdav_enabled: bool,
    pub password: Option<String>,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Deserialize)]
pub struct UpdateShareRequest {
    pub name: Option<String>,
    pub path: Option<String>,
    pub username: Option<String>,
    pub webdav_enabled: Option<bool>,
    pub password: Option<String>,
    pub readonly: Option<bool>,
}

#[derive(Deserialize)]
pub struct UpdateAdminRequest {
    pub username: Option<String>,
    pub password: Option<String>,
    pub global_web_password: Option<String>,
}

pub async fn admin_info(State(state): State<AppState>) -> Json<AdminInfo> {
    let config = state.config_file.read().await;
    Json(AdminInfo {
        username: config.admin_username.clone(),
        has_global_web_password: config.global_web_password_hash.is_some(),
        shares: config.shares.iter().map(ShareView::from).collect(),
        folder_locks: config
            .folder_locks
            .iter()
            .map(FolderLockView::from)
            .collect(),
    })
}

pub async fn create_share(
    State(state): State<AppState>,
    Json(body): Json<CreateShareRequest>,
) -> AppResult<Json<ShareView>> {
    let password_hash = hash_optional_password(&state, body.password, 12, "WebDAV").await?;
    let share = Share {
        id: Uuid::new_v4().to_string(),
        name: body.name.trim().to_string(),
        path: body.path.trim_matches('/').to_string(),
        username: body
            .username
            .map(|username| username.trim().to_string())
            .filter(|username| !username.is_empty()),
        webdav_enabled: body.webdav_enabled,
        password_hash,
        readonly: body.readonly,
    };
    let result = ShareView::from(&share);
    state
        .update_config(move |config| {
            if config
                .shares
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&share.name))
            {
                return Err(AppError::Conflict("Share name already exists".into()));
            }
            config.shares.push(share);
            Ok(())
        })
        .await?;
    Ok(Json(result))
}

pub async fn update_share(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateShareRequest>,
) -> AppResult<Json<ShareView>> {
    let password_hash = hash_changed_password(&state, body.password, 12, "WebDAV").await?;
    let result = state
        .update_config(move |config| {
            if let Some(name) = body.name.as_ref() {
                let duplicate = config
                    .shares
                    .iter()
                    .any(|share| share.id != id && share.name.eq_ignore_ascii_case(name.trim()));
                if duplicate {
                    return Err(AppError::Conflict("Share name already exists".into()));
                }
            }
            let share = config
                .shares
                .iter_mut()
                .find(|share| share.id == id)
                .ok_or(AppError::NotFound)?;
            if let Some(name) = body.name {
                share.name = name.trim().to_string();
            }
            if let Some(path) = body.path {
                share.path = path.trim_matches('/').to_string();
            }
            if let Some(username) = body.username {
                share.username =
                    Some(username.trim().to_string()).filter(|username| !username.is_empty());
            }
            if let Some(webdav_enabled) = body.webdav_enabled {
                share.webdav_enabled = webdav_enabled;
            }
            if let Some(readonly) = body.readonly {
                share.readonly = readonly;
            }
            if let Some(hash) = password_hash {
                share.password_hash = hash;
            }
            Ok(ShareView::from(&*share))
        })
        .await?;
    Ok(Json(result))
}

pub async fn delete_share(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> AppResult<StatusCode> {
    state
        .update_config(move |config| {
            let index = config
                .shares
                .iter()
                .position(|share| share.id == id)
                .ok_or(AppError::NotFound)?;
            config.shares.remove(index);
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_admin_account(
    State(state): State<AppState>,
    Json(body): Json<UpdateAdminRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let password_hash = match body.password {
        Some(password) if !password.is_empty() => {
            validate_password(&password, 12, "Administrator")?;
            Some(state.passwords.hash(password).await?)
        }
        _ => None,
    };
    let global_web_password_hash =
        hash_changed_password(&state, body.global_web_password, 8, "Web access").await?;
    let credentials_changed = password_hash.is_some()
        || body
            .username
            .as_deref()
            .is_some_and(|username| !username.trim().is_empty());
    let gate_changed = global_web_password_hash.is_some();
    state
        .update_config(move |config| {
            if let Some(username) = body.username {
                let username = username.trim();
                if !username.is_empty() {
                    config.admin_username = username.to_string();
                }
            }
            if let Some(hash) = password_hash {
                config.admin_password_hash = hash;
            }
            if let Some(hash) = global_web_password_hash {
                config.global_web_password_hash = hash;
            }
            Ok(())
        })
        .await?;
    if credentials_changed {
        state.sessions.clear().await;
    }
    if gate_changed {
        state.gate_access.clear().await;
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct CreateLockRequest {
    pub path: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct UpdateLockRequest {
    pub path: Option<String>,
    pub password: Option<String>,
}

pub async fn create_lock(
    State(state): State<AppState>,
    Json(body): Json<CreateLockRequest>,
) -> AppResult<Json<FolderLockView>> {
    validate_password(&body.password, 8, "Folder lock")?;
    let lock = FolderLock {
        id: Uuid::new_v4().to_string(),
        path: body.path.trim_matches('/').to_string(),
        password_hash: state.passwords.hash(body.password).await?,
    };
    let result = FolderLockView::from(&lock);
    state
        .update_config(move |config| {
            if config
                .folder_locks
                .iter()
                .any(|existing| existing.path == lock.path)
            {
                return Err(AppError::Conflict("Folder already has a lock".into()));
            }
            config.folder_locks.push(lock);
            Ok(())
        })
        .await?;
    Ok(Json(result))
}

pub async fn update_lock(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateLockRequest>,
) -> AppResult<Json<FolderLockView>> {
    let revoked_id = id.clone();
    let password_hash = match body.password {
        Some(password) if !password.is_empty() => {
            validate_password(&password, 8, "Folder lock")?;
            Some(state.passwords.hash(password).await?)
        }
        _ => None,
    };
    let result = state
        .update_config(move |config| {
            if let Some(path) = body.path.as_ref() {
                let normalized = path.trim_matches('/');
                if config
                    .folder_locks
                    .iter()
                    .any(|lock| lock.id != id && lock.path == normalized)
                {
                    return Err(AppError::Conflict("Folder already has a lock".into()));
                }
            }
            let lock = config
                .folder_locks
                .iter_mut()
                .find(|lock| lock.id == id)
                .ok_or(AppError::NotFound)?;
            if let Some(path) = body.path {
                lock.path = path.trim_matches('/').to_string();
            }
            if let Some(hash) = password_hash {
                lock.password_hash = hash;
            }
            Ok(FolderLockView::from(&*lock))
        })
        .await?;
    state.folder_access.remove_scope(&revoked_id).await;
    Ok(Json(result))
}

pub async fn delete_lock(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> AppResult<StatusCode> {
    let revoked_id = id.clone();
    state
        .update_config(move |config| {
            let original_length = config.folder_locks.len();
            config.folder_locks.retain(|lock| lock.id != id);
            if config.folder_locks.len() == original_length {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await?;
    state.folder_access.remove_scope(&revoked_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn hash_optional_password(
    state: &AppState,
    password: Option<String>,
    minimum: usize,
    kind: &'static str,
) -> AppResult<Option<String>> {
    match password {
        Some(password) if !password.is_empty() => {
            validate_password(&password, minimum, kind)?;
            Ok(Some(state.passwords.hash(password).await?))
        }
        _ => Ok(None),
    }
}

async fn hash_changed_password(
    state: &AppState,
    password: Option<String>,
    minimum: usize,
    kind: &'static str,
) -> AppResult<Option<Option<String>>> {
    match password {
        None => Ok(None),
        Some(password) if password.is_empty() => Ok(Some(None)),
        Some(password) => {
            validate_password(&password, minimum, kind)?;
            Ok(Some(Some(state.passwords.hash(password).await?)))
        }
    }
}

fn validate_password(password: &str, minimum: usize, kind: &'static str) -> AppResult<()> {
    if password.len() < minimum || password.len() > 1_024 {
        Err(AppError::BadRequest(
            format!("{kind} password must contain {minimum}-1024 bytes").into(),
        ))
    } else {
        Ok(())
    }
}
