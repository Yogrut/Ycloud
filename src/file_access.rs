use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use std::collections::HashSet;

use crate::{
    auth::{self, SessionPrincipal},
    config::{self, FolderLock, Share, StoragePermission},
    error::{AppError, AppResult},
    state::AppState,
};

#[derive(Default, Deserialize)]
pub struct FileQuery {
    pub path: Option<String>,
    #[serde(default)]
    pub storage_id: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum StorageAction {
    Browse,
    Download,
    Upload,
    CreateDirectory,
    Rename,
    Move,
    Copy,
    Delete,
}

fn action_allowed(permission: &StoragePermission, action: StorageAction) -> bool {
    match action {
        StorageAction::Browse => permission.browse,
        StorageAction::Download => permission.download,
        StorageAction::Upload => permission.upload,
        StorageAction::CreateDirectory => permission.create_directory,
        StorageAction::Rename => permission.rename,
        StorageAction::Move => permission.move_items,
        StorageAction::Copy => permission.copy,
        StorageAction::Delete => permission.delete,
    }
}

pub async fn storage_permission(
    state: &AppState,
    headers: &HeaderMap,
    storage_id: &str,
) -> Option<StoragePermission> {
    match auth::current_principal(state, headers).await {
        Some(SessionPrincipal::Administrator) => Some(StoragePermission {
            storage_id: storage_id.into(),
            browse: true,
            download: true,
            upload: true,
            create_directory: true,
            rename: true,
            move_items: true,
            copy: true,
            delete: true,
        }),
        Some(SessionPrincipal::User(user_id)) => state
            .config_file
            .read()
            .await
            .user_accounts
            .iter()
            .find(|account| account.id == user_id && account.enabled)
            .and_then(|account| {
                account
                    .permissions
                    .iter()
                    .find(|permission| permission.storage_id == storage_id)
                    .cloned()
            }),
        None => {
            let token = auth::extract_gate_token(headers)?;
            state.gate_access.get_scope(&token).await?;
            let default_storage_id = state.config_file.read().await.default_storage_id.clone();
            (storage_id == default_storage_id).then_some(StoragePermission {
                storage_id: storage_id.into(),
                browse: true,
                download: true,
                upload: false,
                create_directory: false,
                rename: false,
                move_items: false,
                copy: false,
                delete: false,
            })
        }
    }
}

pub async fn ensure_storage_action(
    state: &AppState,
    headers: &HeaderMap,
    storage_id: &str,
    action: StorageAction,
) -> AppResult<()> {
    storage_permission(state, headers, storage_id)
        .await
        .filter(|permission| action_allowed(permission, action))
        .map(|_| ())
        .ok_or(AppError::Forbidden)
}

/// Browser file APIs always operate on the storage root. WebDAV mounts are a
/// separate protocol boundary and cannot be selected through query strings.
pub async fn resolve_share(
    state: &AppState,
    headers: &HeaderMap,
    query: &FileQuery,
) -> Result<Share, StatusCode> {
    let default_storage_id = state.config_file.read().await.default_storage_id.clone();
    if let Some(principal) = auth::current_principal(state, headers).await {
        let selected = match (&principal, query.storage_id.as_deref()) {
            (_, Some(requested)) => requested.to_string(),
            (SessionPrincipal::Administrator, None) => default_storage_id.clone(),
            (SessionPrincipal::User(user_id), None) => {
                let config = state.config_file.read().await;
                let account = config
                    .user_accounts
                    .iter()
                    .find(|account| account.id == *user_id && account.enabled)
                    .ok_or(StatusCode::UNAUTHORIZED)?;
                if account.permissions.iter().any(|permission| {
                    permission.storage_id == default_storage_id && permission.browse
                }) {
                    default_storage_id.clone()
                } else {
                    account
                        .permissions
                        .iter()
                        .find(|permission| permission.browse)
                        .map(|permission| permission.storage_id.clone())
                        .ok_or(StatusCode::FORBIDDEN)?
                }
            }
        };
        let storage_id = selected.as_str();
        if !state.backends.contains(storage_id).await {
            return Err(StatusCode::FORBIDDEN);
        }
        ensure_storage_action(state, headers, storage_id, StorageAction::Browse)
            .await
            .map_err(|_| StatusCode::FORBIDDEN)?;
        return Ok(root_share(storage_id));
    }
    if let Some(token) = auth::extract_gate_token(headers) {
        if state.gate_access.get_scope(&token).await.is_some() {
            // A browser access token is always pinned to the configured
            // default storage. Query parameters cannot widen its scope.
            return Ok(root_share(&default_storage_id));
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

fn root_share(storage_id: &str) -> Share {
    Share {
        id: "root".into(),
        storage_id: storage_id.into(),
        name: "__web__".into(),
        path: String::new(),
        username: None,
        webdav_enabled: false,
        password_hash: None,
        readonly: false,
    }
}

pub fn share_storage_path(share: &Share, request_path: &str) -> String {
    match (share.path.trim_matches('/'), request_path.trim_matches('/')) {
        ("", path) => path.to_string(),
        (base, "") => base.to_string(),
        (base, path) => format!("{base}/{path}"),
    }
}

pub fn join_request_path(parent: &str, name: &str) -> String {
    match (parent.trim_matches('/'), name.trim_matches('/')) {
        ("", name) => name.to_string(),
        (parent, name) => format!("{parent}/{name}"),
    }
}

pub fn batch_destination_path(target: &str, source: &str) -> AppResult<String> {
    let source_name = source
        .trim_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::BadRequest("Source path is invalid".into()))?;
    Ok(join_request_path(target, source_name))
}

pub fn validate_batch_size(paths: &[String]) -> AppResult<()> {
    if paths.is_empty() {
        return Err(AppError::BadRequest("Batch cannot be empty".into()));
    }
    if paths.len() > 1_000 {
        return Err(AppError::BadRequest(
            "Batch exceeds the 1000 item limit".into(),
        ));
    }
    Ok(())
}

pub fn ensure_writable(share: &Share) -> AppResult<()> {
    if share.readonly {
        Err(AppError::Forbidden)
    } else {
        Ok(())
    }
}

pub fn ensure_non_root(path: &str) -> AppResult<()> {
    if path.trim_matches('/').is_empty() {
        Err(AppError::BadRequest(
            "The share root cannot be changed by this operation".into(),
        ))
    } else {
        Ok(())
    }
}

pub fn ensure_copy_target_outside_source(source: &str, destination: &str) -> AppResult<()> {
    if config::path_is_same_or_descendant(destination, source) {
        Err(AppError::Conflict(
            "Destination cannot be the source or its descendant".into(),
        ))
    } else {
        Ok(())
    }
}

pub async fn check_folder_locks(
    state: &AppState,
    headers: &HeaderMap,
    storage_id: &str,
    full_path: &str,
) -> AppResult<()> {
    FolderLockAuthorizer::new(state, headers, storage_id)
        .await
        .ensure_access(full_path)
}

pub struct FolderLockAuthorizer {
    storage_id: String,
    locks: Vec<FolderLock>,
    authorized: HashSet<String>,
}

impl FolderLockAuthorizer {
    pub async fn new(state: &AppState, headers: &HeaderMap, storage_id: &str) -> Self {
        let locks = state
            .config_file
            .read()
            .await
            .folder_locks
            .iter()
            .filter(|lock| lock.storage_id == storage_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut authorized = HashSet::new();
        for lock in &locks {
            let cookie_name = format!("folder_key_{}", lock.id);
            let Some(token) = auth::extract_cookie(headers, &cookie_name) else {
                continue;
            };
            if state.folder_access.get_scope(&token).await.as_deref() == Some(lock.id.as_str()) {
                authorized.insert(lock.id.clone());
            }
        }
        Self {
            storage_id: storage_id.into(),
            locks,
            authorized,
        }
    }

    pub fn ensure_access(&self, full_path: &str) -> AppResult<()> {
        if self.is_locked(full_path) {
            Err(AppError::Forbidden)
        } else {
            Ok(())
        }
    }

    pub fn is_locked(&self, full_path: &str) -> bool {
        self.locks
            .iter()
            .filter(|lock| lock.matches(&self.storage_id, full_path))
            .any(|lock| !self.authorized.contains(&lock.id))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_copy_target_outside_source, ensure_non_root, ensure_writable, join_request_path,
        share_storage_path, FolderLockAuthorizer,
    };
    use crate::config::{FolderLock, Share};
    use std::collections::HashSet;
    use uuid::Uuid;

    fn share(path: &str, readonly: bool) -> Share {
        Share {
            id: Uuid::new_v4().to_string(),
            storage_id: "primary".into(),
            name: "test".into(),
            path: path.into(),
            username: None,
            webdav_enabled: true,
            password_hash: None,
            readonly,
        }
    }

    #[test]
    fn boundaries_are_component_aware() {
        assert!(ensure_writable(&share("shared", true)).is_err());
        assert!(ensure_writable(&share("shared", false)).is_ok());
        assert!(ensure_non_root("").is_err());
        assert!(ensure_non_root("/documents").is_ok());
        assert!(ensure_copy_target_outside_source("documents", "documents/backup").is_err());
        assert!(ensure_copy_target_outside_source("documents", "backups/documents").is_ok());
        assert_eq!(
            share_storage_path(&share("teams/ops", false), "/runbooks"),
            "teams/ops/runbooks"
        );
        assert_eq!(
            join_request_path("/runbooks/", "/sre.md"),
            "runbooks/sre.md"
        );
    }

    #[test]
    fn folder_locks_require_an_explicit_unlock_for_every_browser_identity() {
        let authorizer = FolderLockAuthorizer {
            storage_id: "primary".into(),
            locks: vec![FolderLock {
                id: "lock-id".into(),
                storage_id: "primary".into(),
                path: "test".into(),
                password_hash: "unused".into(),
            }],
            authorized: HashSet::new(),
        };
        assert!(authorizer.is_locked("test"));
        assert!(authorizer.ensure_access("test/child").is_err());
    }
}
