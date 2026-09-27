use axum::{extract::Path, extract::State, http::StatusCode, Json};
use serde::Deserialize;
use uuid::Uuid;

use super::{validate_password, AppError, AppResult, AppState, UserAccountView};
use crate::config::{StoragePermission, UserAccount};

#[derive(Deserialize)]
pub struct CreateUserAccountRequest {
    #[serde(default)]
    pub traffic: crate::traffic::Quota,
    pub username: String,
    pub password: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub permissions: Vec<StoragePermission>,
}

#[derive(Deserialize)]
pub struct UpdateUserAccountRequest {
    pub expected_revision: Option<String>,
    pub traffic: Option<crate::traffic::Quota>,
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
    let mut view = UserAccountView::from(&account);
    view.revision = super::user_account_revision(&account, Some(&body.traffic));
    state
        .update_config(move |config| {
            body.traffic.validate()?;
            config
                .traffic
                .users
                .insert(account.id.clone(), body.traffic);
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
    let _auth_guard = state.auth_transitions.lock().await;
    let (view, revoke) = state
        .update_config(move |config| {
            let account = config
                .user_accounts
                .iter_mut()
                .find(|account| account.id == id)
                .ok_or(AppError::NotFound)?;
            let previous_traffic = config.traffic.users.get(&id);
            if body.expected_revision.as_deref().is_some_and(|expected| {
                expected != super::user_account_revision(account, previous_traffic)
            }) {
                return Err(AppError::Conflict(
                    "该条配置已经变化，请刷新后重新编辑".into(),
                ));
            }
            let revoke = password_hash.is_some()
                || body
                    .username
                    .as_ref()
                    .is_some_and(|name| name.trim() != account.username)
                || body
                    .enabled
                    .is_some_and(|enabled| enabled != account.enabled)
                || body
                    .permissions
                    .as_ref()
                    .is_some_and(|permissions| *permissions != account.permissions);
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
            let mut view = UserAccountView::from(&*account);
            if let Some(traffic) = body.traffic {
                traffic.validate()?;
                config.traffic.users.insert(id.clone(), traffic);
            }
            view.revision = super::user_account_revision(account, config.traffic.users.get(&id));
            Ok((view, revoke))
        })
        .await?;
    if revoke {
        state.sessions.revoke_user(&user_id).await;
    }
    Ok(Json(view))
}

pub async fn delete_user_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let user_id = id.clone();
    let _auth_guard = state.auth_transitions.lock().await;
    state
        .update_config(move |config| {
            let before = config.user_accounts.len();
            config.user_accounts.retain(|account| account.id != id);
            config.traffic.users.remove(&id);
            if before == config.user_accounts.len() {
                return Err(AppError::NotFound);
            }
            Ok(())
        })
        .await?;
    state.sessions.revoke_user(&user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quota_edits_preserve_user_session_and_reject_a_stale_version() {
        let root = crate::test_support::TestDirectory::new("user-quota-session");
        let state =
            crate::test_support::app_state(&root, crate::config::ConfigFile::with_test_storage())
                .await;
        let (_, Json(created)) = create_user_account(
            State(state.clone()),
            Json(CreateUserAccountRequest {
                traffic: crate::traffic::Quota::default(),
                username: "ordinary".into(),
                password: "test-account-password".into(),
                enabled: true,
                permissions: vec![],
            }),
        )
        .await
        .unwrap();
        let session = state.sessions.create_user(created.id.clone()).await;
        let request = |expected_revision| UpdateUserAccountRequest {
            expected_revision,
            traffic: Some(crate::traffic::Quota {
                enabled: true,
                upload: 1024,
                download: 2048,
            }),
            username: None,
            password: None,
            enabled: None,
            permissions: None,
        };
        let Json(updated) = update_user_account(
            State(state.clone()),
            Path(created.id.clone()),
            Json(request(Some(created.revision.clone()))),
        )
        .await
        .unwrap();
        assert!(state.sessions.validate(&session).await);
        assert_ne!(updated.revision, created.revision);
        assert!(matches!(
            update_user_account(
                State(state.clone()),
                Path(created.id),
                Json(request(Some(created.revision)))
            )
            .await,
            Err(AppError::Conflict(_))
        ));
        assert!(state.sessions.validate(&session).await);
    }
}
