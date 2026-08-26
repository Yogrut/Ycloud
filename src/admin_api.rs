use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AppState,
    config::{
        remove_initial_credentials, validate_transfer_limits, validate_transfer_rate, Config,
        FolderLock, S3AddressingStyle, S3Provider, S3StorageConfig, Share, StorageBackendConfig,
        StorageInstanceConfig, StoragePermission, UserAccount,
    },
    error::{AppError, AppResult},
    login_security::{LoginEntry, LoginEventPage, LoginPolicy},
    s3_backend::S3Backend,
};

#[derive(Serialize)]
pub struct AdminInfo {
    pub username: String,
    pub has_global_web_password: bool,
    pub shares: Vec<ShareView>,
    pub folder_locks: Vec<FolderLockView>,
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
    pub upload_rate_bytes_per_sec: u64,
    pub download_rate_bytes_per_sec: u64,
    pub admin_login_failures: u32,
    pub web_login_failures: u32,
    pub admin_login_block_seconds: u64,
    pub web_login_block_seconds: u64,
    pub security_log_retention_days: u32,
    pub security_log_max_entries: usize,
    pub storage_instances: Vec<StorageInstanceView>,
    pub pending_storage_instance: Option<StorageInstanceView>,
    pub default_storage_id: String,
    pub local_storage_path: String,
    pub local_mounts: Vec<LocalMountView>,
    pub user_accounts: Vec<UserAccountView>,
}

#[derive(Clone, Serialize)]
pub struct LocalMountView {
    pub mount_id: String,
    pub name: String,
    pub path: String,
    pub storage_id: Option<String>,
    pub ready: bool,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Clone, Serialize)]
pub struct UserAccountView {
    pub id: String,
    pub username: String,
    pub enabled: bool,
    pub permissions: Vec<StoragePermission>,
}

impl From<&UserAccount> for UserAccountView {
    fn from(account: &UserAccount) -> Self {
        Self {
            id: account.id.clone(),
            username: account.username.clone(),
            enabled: account.enabled,
            permissions: account.permissions.clone(),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct StorageInstanceView {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub enabled: bool,
    pub allow_guest_access: bool,
    pub status: &'static str,
    pub ready: bool,
    pub backend: StorageBackendView,
    pub usage_bytes: u64,
    pub reserved_bytes: u64,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StorageBackendView {
    Local {
        mount_id: String,
        path: String,
        capacity_limit_bytes: Option<u64>,
    },
    S3 {
        provider: S3Provider,
        endpoint: String,
        bucket: String,
        region: String,
        prefix: String,
        addressing_style: S3AddressingStyle,
        has_access_key_id: bool,
        has_secret_access_key: bool,
        capacity_limit_bytes: Option<u64>,
    },
}

impl StorageBackendView {
    fn from_config(backend: &StorageBackendConfig, config: &Config) -> Self {
        match backend {
            StorageBackendConfig::Local(settings) => Self::Local {
                mount_id: settings.mount_id.clone(),
                path: config
                    .local_mounts
                    .resolve(&settings.mount_id)
                    .map(|mount| mount.path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                capacity_limit_bytes: settings.capacity_limit_bytes,
            },
            StorageBackendConfig::S3(settings) => Self::S3 {
                provider: settings.provider,
                endpoint: settings.endpoint.clone(),
                bucket: settings.bucket.clone(),
                region: settings.region.clone(),
                prefix: settings.prefix.clone(),
                addressing_style: settings.addressing_style,
                has_access_key_id: !settings.access_key_id.is_empty(),
                has_secret_access_key: !settings.secret_access_key.is_empty(),
                capacity_limit_bytes: settings.capacity_limit_bytes,
            },
        }
    }
}

/// [安全] Administrative responses expose password presence, never hashes.
#[derive(Clone, Serialize)]
pub struct ShareView {
    pub id: String,
    pub storage_id: String,
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
            storage_id: share.storage_id.clone(),
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
    pub storage_id: String,
    pub path: String,
}

impl From<&FolderLock> for FolderLockView {
    fn from(lock: &FolderLock) -> Self {
        Self {
            id: lock.id.clone(),
            storage_id: lock.storage_id.clone(),
            path: lock.path.clone(),
        }
    }
}

#[derive(Deserialize)]
pub struct CreateShareRequest {
    #[serde(default)]
    pub storage_id: Option<String>,
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
    pub storage_id: Option<String>,
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

pub async fn admin_info(State(state): State<AppState>) -> AppResult<Json<AdminInfo>> {
    let (
        username,
        has_global_web_password,
        shares,
        folder_locks,
        max_upload_bytes,
        max_archive_bytes,
        max_archive_entries,
        upload_rate_bytes_per_sec,
        download_rate_bytes_per_sec,
        admin_login_failures,
        web_login_failures,
        admin_login_block_seconds,
        web_login_block_seconds,
        security_log_retention_days,
        security_log_max_entries,
        storage_configs,
        pending_storage_config,
        default_storage_id,
        user_accounts,
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
            config.upload_rate_bytes_per_sec,
            config.download_rate_bytes_per_sec,
            config.admin_login_failures,
            config.web_login_failures,
            config.admin_login_block_seconds,
            config.web_login_block_seconds,
            config.security_log_retention_days,
            config.security_log_max_entries,
            config.storage_instances.clone(),
            config.pending_storage_instance.clone(),
            config.default_storage_id.clone(),
            config
                .user_accounts
                .iter()
                .map(UserAccountView::from)
                .collect(),
        )
    };
    let mut storage_instances = Vec::with_capacity(storage_configs.len());
    let configured_local_mounts = storage_configs
        .iter()
        .filter_map(|instance| match &instance.backend {
            StorageBackendConfig::Local(settings) => {
                Some((settings.mount_id.clone(), instance.id.clone()))
            }
            StorageBackendConfig::S3(_) => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    for instance in storage_configs {
        storage_instances.push(storage_instance_view(&state, instance, &default_storage_id).await);
    }
    let pending_storage_instance = pending_storage_config.map(|instance| StorageInstanceView {
        id: instance.id,
        name: instance.name,
        is_default: false,
        enabled: instance.enabled,
        allow_guest_access: instance.allow_guest_access,
        status: "pending",
        ready: false,
        backend: StorageBackendView::from_config(&instance.backend, &state.config),
        usage_bytes: 0,
        reserved_bytes: 0,
    });
    let mut local_mounts = Vec::with_capacity(state.config.local_mounts.all().len());
    for mount in state.config.local_mounts.all() {
        let status = state.config.local_mounts.status(&mount.id).await;
        let storage_id = configured_local_mounts.get(&mount.id).cloned();
        let ready = match storage_id.as_deref() {
            Some(id) => state.backends.is_ready(id).await,
            None => status.ready,
        };
        local_mounts.push(LocalMountView {
            mount_id: mount.id.clone(),
            name: mount.name.clone(),
            path: mount.path.to_string_lossy().into_owned(),
            storage_id,
            ready,
            total_bytes: status.total_bytes,
            available_bytes: status.available_bytes,
        });
    }
    Ok(Json(AdminInfo {
        username,
        has_global_web_password,
        shares,
        folder_locks,
        max_upload_bytes,
        max_archive_bytes,
        max_archive_entries,
        upload_rate_bytes_per_sec,
        download_rate_bytes_per_sec,
        admin_login_failures,
        web_login_failures,
        admin_login_block_seconds,
        web_login_block_seconds,
        security_log_retention_days,
        security_log_max_entries,
        storage_instances,
        pending_storage_instance,
        default_storage_id,
        local_storage_path: state.config.storage_path.to_string_lossy().into_owned(),
        local_mounts,
        user_accounts,
    }))
}

async fn storage_instance_view(
    state: &AppState,
    instance: StorageInstanceConfig,
    default_storage_id: &str,
) -> StorageInstanceView {
    let backend_view = StorageBackendView::from_config(&instance.backend, &state.config);
    let (ready, usage_bytes, reserved_bytes) = match state.storage_backend(&instance.id).await {
        Ok(backend) => {
            let capacity = backend.capacity_status().await;
            (true, capacity.used, capacity.reserved)
        }
        Err(_) => (false, 0, 0),
    };
    StorageInstanceView {
        is_default: instance.id == default_storage_id,
        status: if !instance.enabled {
            "disabled"
        } else if ready {
            "enabled"
        } else {
            "abnormal"
        },
        enabled: instance.enabled,
        allow_guest_access: instance.allow_guest_access,
        id: instance.id,
        name: instance.name,
        ready,
        backend: backend_view,
        usage_bytes,
        reserved_bytes,
    }
}

/// Validate credentials and the minimum list permission without changing the
/// active storage backend. Secrets are consumed by the SDK credential provider
/// and are never returned by this endpoint.
pub async fn test_s3_storage(
    State(state): State<AppState>,
    Json(settings): Json<S3StorageConfig>,
) -> AppResult<Json<serde_json::Value>> {
    let backend = S3Backend::new(&settings, &state.config)?;
    backend.probe().await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Run the full read/write/copy/delete capability probe and persist the
/// credentials only as a pending backend. User traffic remains on the current
/// backend until a separate activation request succeeds.
#[derive(Deserialize)]
pub struct StageS3StorageRequest {
    pub name: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_guest_access: bool,
    #[serde(flatten)]
    pub settings: S3StorageConfig,
}

fn enabled_by_default() -> bool {
    true
}

pub async fn stage_s3_storage(
    State(state): State<AppState>,
    Json(body): Json<StageS3StorageRequest>,
) -> AppResult<Json<serde_json::Value>> {
    state
        .stage_s3_storage(
            body.name,
            body.settings,
            body.enabled,
            body.allow_guest_access,
        )
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct UpdateS3StorageRequest {
    pub name: String,
    #[serde(flatten)]
    pub settings: S3StorageConfig,
}

pub async fn update_s3_storage(
    State(state): State<AppState>,
    Path(storage_id): Path<String>,
    Json(body): Json<UpdateS3StorageRequest>,
) -> AppResult<Json<serde_json::Value>> {
    state
        .update_s3_storage(&storage_id, body.name, body.settings)
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct UpdateLocalStorageRequest {
    pub storage_id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub capacity_limit_bytes: Option<u64>,
}

pub async fn update_local_storage(
    State(state): State<AppState>,
    Json(settings): Json<UpdateLocalStorageRequest>,
) -> AppResult<Json<serde_json::Value>> {
    state
        .update_local_storage(
            &settings.storage_id,
            settings.name,
            settings.path,
            settings.capacity_limit_bytes,
        )
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct AddLocalStorageRequest {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub capacity_limit_bytes: Option<u64>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_guest_access: bool,
}

pub async fn add_local_storage(
    State(state): State<AppState>,
    Json(settings): Json<AddLocalStorageRequest>,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    let storage_id = state
        .add_local_storage(
            settings.path,
            settings.name,
            settings.capacity_limit_bytes,
            settings.enabled,
            settings.allow_guest_access,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "storage_id": storage_id })),
    ))
}

#[derive(Deserialize)]
pub struct UpdateStorageAccessRequest {
    pub enabled: bool,
    pub allow_guest_access: bool,
}

pub async fn update_storage_access(
    State(state): State<AppState>,
    Path(storage_id): Path<String>,
    Json(body): Json<UpdateStorageAccessRequest>,
) -> AppResult<Json<serde_json::Value>> {
    state
        .update_storage_access(&storage_id, body.enabled, body.allow_guest_access)
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn delete_storage(
    State(state): State<AppState>,
    Path(storage_id): Path<String>,
) -> AppResult<StatusCode> {
    state.delete_storage(&storage_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn activate_pending_storage(
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    state.activate_pending_storage().await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn discard_pending_storage(State(state): State<AppState>) -> AppResult<StatusCode> {
    state.discard_pending_storage().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct UpdateTransferLimitsRequest {
    pub max_upload_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_archive_entries: usize,
    #[serde(default)]
    pub upload_rate_bytes_per_sec: Option<u64>,
    #[serde(default)]
    pub download_rate_bytes_per_sec: Option<u64>,
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
    let current = state.config_file.read().await.clone();
    let upload_rate = body
        .upload_rate_bytes_per_sec
        .unwrap_or(current.upload_rate_bytes_per_sec);
    let download_rate = body
        .download_rate_bytes_per_sec
        .unwrap_or(current.download_rate_bytes_per_sec);
    validate_transfer_rate(upload_rate, "上传")?;
    validate_transfer_rate(download_rate, "下载")?;
    state
        .update_config(move |config| {
            config.max_upload_bytes = body.max_upload_bytes;
            config.max_archive_bytes = body.max_archive_bytes;
            config.max_archive_entries = body.max_archive_entries;
            config.upload_rate_bytes_per_sec = upload_rate;
            config.download_rate_bytes_per_sec = download_rate;
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

#[derive(Deserialize)]
pub struct UpdateLoginSecuritySettingsRequest {
    pub admin_login_failures: Option<u32>,
    pub web_login_failures: Option<u32>,
    pub admin_login_block_seconds: Option<u64>,
    pub web_login_block_seconds: Option<u64>,
    pub security_log_retention_days: Option<u32>,
    pub security_log_max_entries: Option<usize>,
}

pub async fn update_login_security_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateLoginSecuritySettingsRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if body.admin_login_failures.is_none()
        && body.web_login_failures.is_none()
        && body.admin_login_block_seconds.is_none()
        && body.web_login_block_seconds.is_none()
        && body.security_log_retention_days.is_none()
        && body.security_log_max_entries.is_none()
    {
        return Err(AppError::BadRequest("没有需要更新的登录安全设置".into()));
    }
    state
        .update_config(move |config| {
            if let Some(value) = body.admin_login_failures {
                config.admin_login_failures = value;
            }
            if let Some(value) = body.web_login_failures {
                config.web_login_failures = value;
            }
            if let Some(value) = body.admin_login_block_seconds {
                config.admin_login_block_seconds = value;
            }
            if let Some(value) = body.web_login_block_seconds {
                config.web_login_block_seconds = value;
            }
            if let Some(value) = body.security_log_retention_days {
                config.security_log_retention_days = value;
            }
            if let Some(value) = body.security_log_max_entries {
                config.security_log_max_entries = value;
            }
            Ok(())
        })
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct LoginEventQuery {
    pub success: Option<bool>,
    pub entry: Option<LoginEntry>,
    pub ip: Option<String>,
    pub since: Option<i64>,
    pub cursor: Option<u64>,
    pub limit: Option<usize>,
}

pub async fn login_events(
    State(state): State<AppState>,
    Query(query): Query<LoginEventQuery>,
) -> AppResult<Json<LoginEventPage>> {
    let now = chrono::Utc::now().timestamp();
    let earliest = now - 30 * 24 * 60 * 60;
    let since = query.since.unwrap_or(now - 7 * 24 * 60 * 60);
    if since < earliest || since > now {
        return Err(AppError::BadRequest(
            "日志查询时间必须在最近 30 天内".into(),
        ));
    }
    let limit = query.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(AppError::BadRequest(
            "每页日志数量必须在 1 到 100 之间".into(),
        ));
    }
    if query.ip.as_deref().is_some_and(|ip| ip.len() > 64) {
        return Err(AppError::BadRequest("IP 筛选条件过长".into()));
    }
    Ok(Json(
        state
            .login_security
            .query_events(
                query.success,
                query.entry,
                query.ip.as_deref(),
                since,
                query.cursor,
                limit,
            )
            .await,
    ))
}

pub async fn block_login(
    State(state): State<AppState>,
    Json(body): Json<LoginRestrictionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let ip = body
        .ip
        .parse()
        .map_err(|_| AppError::BadRequest("IP 地址无效".into()))?;
    let policy = match body.entry {
        LoginEntry::Admin => {
            let config = state.config_file.read().await;
            LoginPolicy {
                maximum_failures: config.admin_login_failures,
                block_seconds: config.admin_login_block_seconds as i64,
            }
        }
        LoginEntry::Account => LoginEntry::Account.fixed_policy(),
        LoginEntry::Web => {
            let config = state.config_file.read().await;
            LoginPolicy {
                maximum_failures: config.web_login_failures,
                block_seconds: config.web_login_block_seconds as i64,
            }
        }
        LoginEntry::WebDav => LoginEntry::WebDav.fixed_policy(),
    };
    state
        .login_security
        .restrict(body.entry, ip, policy)
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
    let default_storage_id = state.config_file.read().await.default_storage_id.clone();
    let share = Share {
        id: Uuid::new_v4().to_string(),
        storage_id: body.storage_id.unwrap_or(default_storage_id),
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
            if let Some(storage_id) = body.storage_id {
                share.storage_id = storage_id;
            }
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
pub struct CreateUserAccountRequest {
    pub username: String,
    pub password: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub permissions: Vec<StoragePermission>,
}

#[derive(Deserialize)]
pub struct UpdateUserAccountRequest {
    pub username: Option<String>,
    pub password: Option<String>,
    pub enabled: Option<bool>,
    pub permissions: Option<Vec<StoragePermission>>,
}

fn default_enabled() -> bool {
    true
}

pub async fn create_user_account(
    State(state): State<AppState>,
    Json(body): Json<CreateUserAccountRequest>,
) -> AppResult<(StatusCode, Json<UserAccountView>)> {
    validate_password(&body.password, 12, "普通账号")?;
    let password_hash = state.passwords.hash(body.password).await?;
    let account = UserAccount {
        id: Uuid::new_v4().to_string(),
        username: body.username.trim().to_string(),
        password_hash,
        enabled: body.enabled,
        permissions: body.permissions,
    };
    let view = UserAccountView::from(&account);
    state
        .update_config(move |config| {
            config.user_accounts.push(account);
            Ok(())
        })
        .await?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn update_user_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserAccountRequest>,
) -> AppResult<Json<UserAccountView>> {
    let password_hash = match body.password {
        Some(password) if !password.is_empty() => {
            validate_password(&password, 12, "普通账号")?;
            Some(state.passwords.hash(password).await?)
        }
        _ => None,
    };
    let user_id = id.clone();
    let view = state
        .update_config(move |config| {
            let account = config
                .user_accounts
                .iter_mut()
                .find(|account| account.id == id)
                .ok_or(AppError::NotFound)?;
            if let Some(username) = body.username {
                account.username = username.trim().to_string();
            }
            if let Some(hash) = password_hash {
                account.password_hash = hash;
            }
            if let Some(enabled) = body.enabled {
                account.enabled = enabled;
            }
            if let Some(permissions) = body.permissions {
                account.permissions = permissions;
            }
            Ok(UserAccountView::from(&*account))
        })
        .await?;
    // Credential, status and authorization changes take effect immediately.
    state.sessions.revoke_user(&user_id).await;
    Ok(Json(view))
}

pub async fn delete_user_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let user_id = id.clone();
    state
        .update_config(move |config| {
            let before = config.user_accounts.len();
            config.user_accounts.retain(|account| account.id != id);
            if before == config.user_accounts.len() {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await?;
    state.sessions.revoke_user(&user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CreateLockRequest {
    #[serde(default)]
    pub storage_id: Option<String>,
    pub path: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct UpdateLockRequest {
    pub storage_id: Option<String>,
    pub path: Option<String>,
    pub password: Option<String>,
}

pub async fn create_lock(
    State(state): State<AppState>,
    Json(body): Json<CreateLockRequest>,
) -> AppResult<Json<FolderLockView>> {
    validate_password(&body.password, 8, "文件夹锁")?;
    let default_storage_id = state.config_file.read().await.default_storage_id.clone();
    let lock = FolderLock {
        id: Uuid::new_v4().to_string(),
        storage_id: body.storage_id.unwrap_or(default_storage_id),
        path: body.path.trim_matches('/').to_string(),
        password_hash: state.passwords.hash(body.password).await?,
    };
    let result = FolderLockView::from(&lock);
    state
        .update_config(move |config| {
            if config.folder_locks.iter().any(|existing| {
                existing.storage_id == lock.storage_id && existing.path == lock.path
            }) {
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
            if let Some(storage_id) = body.storage_id {
                lock.storage_id = storage_id;
            }
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
