use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AppState,
    config::{remove_initial_credentials, validate_transfer_limits, FolderLock, Share},
    error::{AppError, AppResult},
    login_security::{LoginEntry, LoginRecord},
};

#[derive(Serialize)]
pub struct AdminInfo {
    pub username: String,
    pub has_global_web_password: bool,
    pub shares: Vec<ShareView>,
    pub folder_locks: Vec<FolderLockView>,
    pub login_security: Vec<LoginRecord>,
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
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
    let (
        username,
        has_global_web_password,
        shares,
        folder_locks,
        max_upload_bytes,
        max_archive_bytes,
        max_archive_entries,
    ) = {
        let config = state.config_file.read().await;
        (
            config.admin_username.clone(),
            config.global_web_password_hash.is_some(),
            config.shares.iter().map(ShareView::from).collect(),
            config
                .folder_locks
                .iter()
                .map(FolderLockView::from)
                .collect(),
            config.max_upload_bytes,
            config.max_archive_bytes,
            config.max_archive_entries,
        )
    };
    Json(AdminInfo {
        username,
        has_global_web_password,
        shares,
        folder_locks,
        login_security: state.login_security.snapshot().await,
        max_upload_bytes,
        max_archive_bytes,
        max_archive_entries,
    })
}

#[derive(Deserialize)]
pub struct UpdateTransferLimitsRequest {
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
}

pub async fn update_transfer_limits(
    State(state): State<AppState>,
    Json(body): Json<UpdateTransferLimitsRequest>,
) -> AppResult<Json<serde_json::Value>> {
    validate_transfer_limits(
        body.max_upload_bytes,
        body.max_archive_bytes,
        body.max_archive_entries,
    )?;
    if body.max_upload_bytes > state.config.max_upload_bytes {
        return Err(AppError::BadRequest(
            "单文件上传上限超过部署环境允许的绝对上限；请调整 MAX_UPLOAD_BYTES 后重启服务".into(),
        ));
    }
    state
        .update_config(move |config| {
            config.max_upload_bytes = body.max_upload_bytes;
            config.max_archive_bytes = body.max_archive_bytes;
            config.max_archive_entries = body.max_archive_entries;
            Ok(())
        })
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct LoginRestrictionRequest {
    pub entry: LoginEntry,
    pub ip: String,
}

pub async fn block_login(
    State(state): State<AppState>,
    Json(body): Json<LoginRestrictionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let ip = body
        .ip
        .parse()
        .map_err(|_| AppError::BadRequest("IP 地址无效".into()))?;
    state
        .login_security
        .restrict(body.entry, ip)
        .await
        .map_err(|error| AppError::with_source("无法持久化登录安全状态", error))?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn unblock_login(
    State(state): State<AppState>,
    Json(body): Json<LoginRestrictionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let ip = body
        .ip
        .parse()
        .map_err(|_| AppError::BadRequest("IP 地址无效".into()))?;
    let found = state
        .login_security
        .unblock(body.entry, ip)
        .await
        .map_err(|error| AppError::with_source("无法持久化登录安全状态", error))?;
    if !found {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "success": true })))
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
            validate_password(&password, 12, "管理员")?;
            Some(state.passwords.hash(password).await?)
        }
        _ => None,
    };
    let global_web_password_hash =
        hash_changed_password(&state, body.global_web_password, 8, "网页访问").await?;
    let admin_password_changed = password_hash.is_some();
    let credentials_changed = admin_password_changed
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
    let mut initial_credentials_warning = None;
    if admin_password_changed || gate_changed {
        if let Err(error) = remove_initial_credentials(&state.config.config_path).await {
            tracing::error!(%error, "failed to remove initial plaintext credentials after account update");
            initial_credentials_warning = Some(
                "账户已更新，但初始凭据文件删除失败；请在服务器配置目录手动删除 initial-credentials.json",
            );
        }
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "warning": initial_credentials_warning
    })))
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
    validate_password(&body.password, 8, "文件夹锁")?;
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
            validate_password(&password, 8, "文件夹锁")?;
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
    let characters = password.chars().count();
    if characters < minimum {
        Err(AppError::BadRequest(
            format!("{kind}密码至少需要 {minimum} 位").into(),
        ))
    } else if characters > 1_024 || password.len() > 4_096 {
        Err(AppError::BadRequest(format!("{kind}密码过长").into()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::validate_password;

    #[test]
    fn password_policy_counts_unicode_characters_not_utf8_bytes() {
        assert!(validate_password("密码安全", 4, "测试").is_ok());
        assert!(validate_password("密码安全", 5, "测试").is_err());
        assert!(validate_password("Dav密码-2026-安全", 12, "WebDAV").is_ok());
    }
}
