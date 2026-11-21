use std::collections::HashSet;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult, CommitState},
    file_access::{
        batch_destination_path, ensure_copy_target_outside_source, ensure_non_root,
        ensure_storage_action, ensure_writable, share_storage_path, validate_batch_size, FileQuery,
        FolderLockAuthorizer, StorageAction,
    },
    state::AppState,
    storage_backend::{StorageBackend, TransferKind},
};

/// What a batch transfer does when the destination already holds the name.
#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Keep the current behaviour: report the collision to the caller.
    #[default]
    Error,
    /// Allocate the numbered name the upload path already uses.
    KeepBoth,
    /// Replace the destination file through the WebDAV transfer path.
    Overwrite,
}

#[derive(Deserialize)]
pub struct BatchBody {
    pub paths: Vec<String>,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub conflict: ConflictPolicy,
}

#[derive(Serialize)]
struct BatchItemResult {
    path: String,
    status: u16,
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<crate::error::OperationOutcome>,
}

#[derive(Serialize)]
struct BatchResponse {
    success: usize,
    failed: usize,
    pending: usize,
    results: Vec<BatchItemResult>,
}

#[derive(Clone, Copy)]
enum Operation {
    Delete,
    Move,
    Copy,
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<BatchBody>,
) -> AppResult<Response> {
    execute(&state, &headers, &query, body, Operation::Delete).await
}

pub async fn move_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<BatchBody>,
) -> AppResult<Response> {
    execute(&state, &headers, &query, body, Operation::Move).await
}

pub async fn copy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<BatchBody>,
) -> AppResult<Response> {
    execute(&state, &headers, &query, body, Operation::Copy).await
}

async fn execute(
    state: &AppState,
    headers: &HeaderMap,
    query: &FileQuery,
    body: BatchBody,
    operation: Operation,
) -> AppResult<Response> {
    let share = crate::file_access::resolve_write_share(state, headers, query).await?;
    let action = match operation {
        Operation::Delete => StorageAction::Delete,
        Operation::Move => StorageAction::Move,
        Operation::Copy => StorageAction::Copy,
    };
    ensure_storage_action(state, headers, &share.storage_id, action).await?;
    let backend = state.storage_backend(&share.storage_id).await?;
    ensure_writable(&share)?;
    validate_batch_size(&body.paths)?;
    let lock_authorizer = FolderLockAuthorizer::new(state, headers, &share.storage_id).await;
    let mut results = Vec::with_capacity(body.paths.len());
    for path in body.paths {
        let outcome = async {
            ensure_non_root(&path)?;
            lock_authorizer.ensure_tree_access(&share_storage_path(&share, &path))?;
            let source = share_storage_path(&share, &path);
            match operation {
                Operation::Delete => backend.remove(&source).await,
                Operation::Move | Operation::Copy => {
                    let destination_path = batch_destination_path(&body.target, &path)?;
                    ensure_copy_target_outside_source(&path, &destination_path)?;
                    lock_authorizer
                        .ensure_tree_access(&share_storage_path(&share, &destination_path))?;
                    transfer(
                        &backend,
                        operation,
                        &share_storage_path(&share, &path),
                        &share_storage_path(&share, &destination_path),
                        body.conflict,
                    )
                    .await
                }
            }
        }
        .await;
        results.push(match outcome {
            Ok(()) => BatchItemResult {
                path,
                status: StatusCode::OK.as_u16(),
                code: "ok",
                message: "Completed".into(),
                operation: None,
            },
            Err(error) => BatchItemResult {
                path,
                status: error.status().as_u16(),
                code: error.code(),
                message: error.public_message().into_owned(),
                operation: error.operation(),
            },
        });
    }
    Ok(batch_response(results).into_response())
}

/// Applies the collision policy the caller chose before the transfer started.
/// Directories always take the numbered name: replacing a tree would delete
/// content the caller never selected.
async fn transfer(
    backend: &StorageBackend,
    operation: Operation,
    source: &str,
    destination: &str,
    conflict: ConflictPolicy,
) -> AppResult<()> {
    let directory = backend.metadata(source).await?.is_dir;
    let policy = if directory {
        ConflictPolicy::KeepBoth
    } else {
        conflict
    };
    match policy {
        ConflictPolicy::Overwrite => {
            let kind = match operation {
                Operation::Move => TransferKind::Move,
                _ => TransferKind::Copy,
            };
            backend
                .dav_transfer(source, destination, kind, true)
                .await
                .map(|_| ())
        }
        ConflictPolicy::Error => match operation {
            Operation::Move => backend.move_path(source, destination).await,
            _ => backend.copy_path(source, destination).await,
        },
        ConflictPolicy::KeepBoth => {
            // Only a collision detected before any write is retried. Files create
            // the destination with create-new semantics, so a collision means
            // nothing was written. Directories are not retried: a partial tree
            // could remain if the conflict surfaced mid-copy.
            let attempts = if directory { 1 } else { MAX_COLLISION_ATTEMPTS };
            keep_both_with_retries(
                attempts,
                |reserved| {
                    Box::pin(
                        async move { available_destination(backend, destination, &reserved).await },
                    )
                },
                |candidate| {
                    Box::pin(async move {
                        match operation {
                            Operation::Move => backend.move_path(source, &candidate).await,
                            _ => backend.copy_path(source, &candidate).await,
                        }
                    })
                },
            )
            .await
        }
    }
}

const MAX_NAME_ATTEMPTS: usize = 1000;
const MAX_COLLISION_ATTEMPTS: usize = 3;

#[cfg(test)]
mod retry_tests;

type RetryFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Retries `attempt` with a fresh candidate while it reports `DestinationExists`.
/// The caller decides how many attempts are safe (directories get one).
async fn keep_both_with_retries<'a, N, A>(
    attempts: usize,
    mut next: N,
    mut attempt: A,
) -> AppResult<()>
where
    N: FnMut(HashSet<String>) -> RetryFuture<'a, AppResult<String>>,
    A: FnMut(String) -> RetryFuture<'a, AppResult<()>>,
{
    let mut reserved = HashSet::new();
    let mut last = None;
    for _ in 0..attempts {
        let candidate = next(reserved.clone()).await?;
        match attempt(candidate.clone()).await {
            Ok(()) => return Ok(()),
            Err(error) if error.is_destination_exists() => {
                reserved.insert(candidate);
                last = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(if attempts == 1 {
        last.unwrap_or(AppError::DestinationExists)
    } else {
        AppError::Conflict(format!("已尝试 {attempts} 个编号仍冲突").into())
    })
}

/// Returns `destination` when it is free, otherwise the next numbered name the
/// upload path would allocate for the same file. Names this batch already tried
/// are skipped, so a retry never reuses the same candidate.
async fn available_destination(
    backend: &StorageBackend,
    destination: &str,
    reserved: &HashSet<String>,
) -> AppResult<String> {
    if !reserved.contains(destination) && backend.metadata(destination).await.is_err() {
        return Ok(destination.to_owned());
    }
    let (parent, name) = match destination.rsplit_once('/') {
        Some((parent, name)) => (format!("{parent}/"), name),
        None => (String::new(), destination),
    };
    let mut names = HashSet::from([name.to_owned()]);
    for _ in 0..MAX_NAME_ATTEMPTS {
        let candidate = crate::upload_batch::numbered_upload_name(name, &names)?;
        let path = format!("{parent}{candidate}");
        if !reserved.contains(&path) && backend.metadata(&path).await.is_err() {
            return Ok(path);
        }
        names.insert(candidate);
    }
    Err(AppError::DestinationExists)
}

fn batch_response(results: Vec<BatchItemResult>) -> (StatusCode, Json<BatchResponse>) {
    let success = results
        .iter()
        .filter(|item| {
            item.status < 400
                || item
                    .operation
                    .is_some_and(|outcome| outcome.commit == CommitState::Committed)
        })
        .count();
    let pending = results
        .iter()
        .filter(|item| {
            item.operation
                .is_some_and(|outcome| outcome.commit == CommitState::Unknown)
        })
        .count();
    let failed = results.len().saturating_sub(success + pending);
    let status = if failed == 0 && pending == 0 {
        StatusCode::OK
    } else if success == 0 {
        StatusCode::CONFLICT
    } else {
        StatusCode::MULTI_STATUS
    };
    (
        status,
        Json(BatchResponse {
            success,
            failed,
            pending,
            results,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, CleanupState};

    #[test]
    fn counts_verified_commits_and_unknown_results_separately_from_failures() {
        let results = [
            CommitState::Committed,
            CommitState::Unknown,
            CommitState::NotCommitted,
        ]
        .into_iter()
        .map(|commit| {
            let error = AppError::ServiceUnavailable("test".into())
                .with_operation(commit, CleanupState::Pending);
            BatchItemResult {
                path: "file".into(),
                status: error.status().as_u16(),
                code: error.code(),
                message: error.public_message().into_owned(),
                operation: error.operation(),
            }
        })
        .collect();
        let (status, Json(response)) = batch_response(results);
        assert_eq!(status, StatusCode::MULTI_STATUS);
        assert_eq!(response.success, 1);
        assert_eq!(response.pending, 1);
        assert_eq!(response.failed, 1);
    }
}
