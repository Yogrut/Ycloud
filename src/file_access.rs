use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use std::collections::HashSet;

use crate::{
    auth,
    config::{self, FolderLock, Share},
    error::{AppError, AppResult},
    state::AppState,
    storage::ResolvedPath,
};

#[derive(Deserialize)]
pub struct FileQuery {
    pub path: Option<String>,
}

/// Browser file APIs always operate on the storage root. WebDAV mounts are a
/// separate protocol boundary and cannot be selected through query strings.
pub async fn resolve_share(
    state: &AppState,
    headers: &HeaderMap,
    _query: &FileQuery,
) -> Result<Share, StatusCode> {
    if let Some(token) = auth::extract_session_token(headers) {
        if state.sessions.validate(&token).await {
            return Ok(root_share());
        }
    }
    if let Some(token) = auth::extract_gate_token(headers) {
        if state.gate_access.get_scope(&token).await.is_some() {
            return Ok(root_share());
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

fn root_share() -> Share {
    Share {
        id: "root".into(),
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

pub async fn resolve_existing_path(
    state: &AppState,
    share: &Share,
    request_path: &str,
) -> AppResult<ResolvedPath> {
    state
        .storage
        .resolve_existing(&share_storage_path(share, request_path))
        .await
}

pub async fn resolve_write_path(
    state: &AppState,
    share: &Share,
    request_path: &str,
) -> AppResult<ResolvedPath> {
    state
        .storage
        .resolve_for_write(&share_storage_path(share, request_path))
        .await
}

pub async fn check_folder_locks(
    state: &AppState,
    headers: &HeaderMap,
    full_path: &str,
) -> AppResult<()> {
    FolderLockAuthorizer::new(state, headers)
        .await
        .ensure_access(full_path)
}

pub struct FolderLockAuthorizer {
    locks: Vec<FolderLock>,
    authorized: HashSet<String>,
}

impl FolderLockAuthorizer {
    pub async fn new(state: &AppState, headers: &HeaderMap) -> Self {
        let locks = state.config_file.read().await.folder_locks.clone();
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
        Self { locks, authorized }
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
            .filter(|lock| lock.matches(full_path))
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
            locks: vec![FolderLock {
                id: "lock-id".into(),
                path: "test".into(),
                password_hash: "unused".into(),
            }],
            authorized: HashSet::new(),
        };
        assert!(authorizer.is_locked("test"));
        assert!(authorizer.ensure_access("test/child").is_err());
    }
}
