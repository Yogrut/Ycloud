use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use super::{AppError, AppResult, AppState};

#[derive(Deserialize)]
pub struct TotpSetupRequest {
    pub current_password: String,
}

#[derive(Serialize)]
pub struct TotpSetupResponse {
    pub secret: String,
    pub provisioning_uri: String,
    pub qr_svg: String,
}

#[derive(Deserialize)]
pub struct TotpEnableRequest {
    pub current_password: String,
    pub secret: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct TotpEnableResponse {
    pub success: bool,
    pub recovery_codes: Vec<String>,
}

#[derive(Deserialize)]
pub struct TotpDisableRequest {
    pub current_password: String,
    pub code: String,
}

pub async fn setup_admin_totp(
    State(state): State<AppState>,
    Json(body): Json<TotpSetupRequest>,
) -> AppResult<Json<TotpSetupResponse>> {
    verify_current_admin_password(&state, body.current_password).await?;
    let (username, already_enabled) = {
        let config = state.config_file.read().await;
        (
            config.admin_username.clone(),
            config.admin_totp_secret.is_some(),
        )
    };
    if already_enabled {
        return Err(AppError::Conflict(
            "管理员两步验证已启用；如需更换，请先验证并停用当前配置".into(),
        ));
    }
    let secret = crate::totp::generate_secret();
    let provisioning_uri = crate::totp::provisioning_uri(&secret, &username);
    Ok(Json(TotpSetupResponse {
        qr_svg: crate::totp::provisioning_qr_svg(&provisioning_uri)?,
        provisioning_uri,
        secret,
    }))
}

pub async fn enable_admin_totp(
    State(state): State<AppState>,
    Json(body): Json<TotpEnableRequest>,
) -> AppResult<Json<TotpEnableResponse>> {
    let _auth_guard = state.auth_transitions.lock().await;
    verify_current_admin_password(&state, body.current_password).await?;
    crate::totp::validate_secret(&body.secret)?;
    if !crate::totp::verify_now(&body.secret, &body.code) {
        return Err(AppError::BadRequest("动态验证码无效或已过期".into()));
    }
    let recovery_codes = crate::totp::generate_recovery_codes(8);
    let mut recovery_hashes = Vec::with_capacity(recovery_codes.len());
    for code in &recovery_codes {
        recovery_hashes.push(
            state
                .passwords
                .hash(crate::totp::normalize_recovery_code(code))
                .await?,
        );
    }
    let secret = body.secret;
    state
        .update_config(move |config| {
            if config.admin_totp_secret.is_some() {
                return Err(AppError::Conflict("管理员两步验证已经启用".into()));
            }
            config.admin_totp_secret = Some(secret);
            config.admin_recovery_code_hashes = recovery_hashes;
            Ok(())
        })
        .await?;
    state.admin_totp_replay.clear().await;
    state.sessions.clear().await;
    Ok(Json(TotpEnableResponse {
        success: true,
        recovery_codes,
    }))
}

pub async fn disable_admin_totp(
    State(state): State<AppState>,
    Json(body): Json<TotpDisableRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let _auth_guard = state.auth_transitions.lock().await;
    verify_current_admin_password(&state, body.current_password).await?;
    let (secret, recovery_hashes) = {
        let config = state.config_file.read().await;
        (
            config
                .admin_totp_secret
                .clone()
                .ok_or_else(|| AppError::Conflict("管理员两步验证尚未启用".into()))?,
            config.admin_recovery_code_hashes.clone(),
        )
    };
    if !verify_admin_second_factor(&state, &secret, &recovery_hashes, &body.code).await {
        return Err(AppError::BadRequest("动态验证码或恢复码无效".into()));
    }
    state
        .update_config(|config| {
            config.admin_totp_secret = None;
            config.admin_recovery_code_hashes.clear();
            Ok(())
        })
        .await?;
    state.admin_totp_replay.clear().await;
    state.sessions.clear().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn verify_current_admin_password(state: &AppState, password: String) -> AppResult<()> {
    if password.chars().count() > 4_096 {
        return Err(AppError::BadRequest("管理员密码无效".into()));
    }
    let hash = state.config_file.read().await.admin_password_hash.clone();
    if !state.passwords.verify(hash, password).await {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

async fn verify_admin_second_factor(
    state: &AppState,
    secret: &str,
    recovery_hashes: &[String],
    supplied: &str,
) -> bool {
    if crate::totp::verify_now(secret, supplied) {
        return true;
    }
    let recovery = crate::totp::normalize_recovery_code(supplied);
    if recovery.len() != 10 {
        return false;
    }
    for hash in recovery_hashes {
        if state.passwords.verify(hash.clone(), recovery.clone()).await {
            return true;
        }
    }
    false
}
