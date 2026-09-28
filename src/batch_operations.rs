use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppResult, CommitState},
    file_access::{
        batch_destination_path, ensure_copy_target_outside_source, ensure_non_root,
        ensure_storage_action, ensure_writable, share_storage_path, validate_batch_size, FileQuery,
        FolderLockAuthorizer, StorageAction,
    },
    state::AppState,
};

#[derive(Deserialize)]
pub struct BatchBody {
    pub paths: Vec<String>,
    #[serde(default)]
    pub target: String,
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
                    let destination = share_storage_path(&share, &destination_path);
                    if matches!(operation, Operation::Move) {
                        backend.move_path(&source, &destination).await
                    } else {
                        backend.copy_path(&source, &destination).await
                    }
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
