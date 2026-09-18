use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::AppResult,
    file_access::{
        batch_destination_path, ensure_copy_target_outside_source, ensure_non_root,
        ensure_storage_action, ensure_writable, resolve_share, share_storage_path,
        validate_batch_size, FileQuery, FolderLockAuthorizer, StorageAction,
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
    let share = resolve_share(state, headers, query).await?;
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
    let success = results.iter().filter(|item| item.status < 400).count();
    let failed = results.len().saturating_sub(success);
    let status = if failed == 0 {
        StatusCode::OK
    } else if success == 0 {
        StatusCode::CONFLICT
    } else {
        StatusCode::MULTI_STATUS
    };
    Ok((
        status,
        Json(BatchResponse {
            success,
            failed,
            results,
        }),
    )
        .into_response())
}
