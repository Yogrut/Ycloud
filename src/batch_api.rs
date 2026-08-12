use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;

use crate::{
    error::{AppError, AppResult},
    file_access::{
        batch_destination_path, ensure_copy_target_outside_source, ensure_non_root,
        ensure_writable, resolve_existing_path, resolve_share, resolve_write_path,
        share_storage_path, validate_batch_size, FileQuery, FolderLockAuthorizer,
    },
    state::AppState,
    storage::ResolvedPath,
};

#[derive(Deserialize)]
pub struct BatchBody {
    pub paths: Vec<String>,
    #[serde(default)]
    pub target: String,
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<BatchBody>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_writable(&share)?;
    validate_batch_size(&body.paths)?;
    let mut targets = Vec::with_capacity(body.paths.len());
    let lock_authorizer = FolderLockAuthorizer::new(&state, &headers).await;

    for path in &body.paths {
        ensure_non_root(path)?;
        lock_authorizer.ensure_access(&share_storage_path(&share, path))?;
        targets.push(resolve_existing_path(&state, &share, path).await?);
    }
    for target in targets {
        state.storage.remove(&target).await?;
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn move_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<BatchBody>,
) -> AppResult<Json<serde_json::Value>> {
    let operations = plan(&state, &headers, &query, &body).await?;
    for (source, destination) in operations {
        state.storage.move_path(&source, &destination).await?;
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn copy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<BatchBody>,
) -> AppResult<Json<serde_json::Value>> {
    let operations = plan(&state, &headers, &query, &body).await?;
    for (source, destination) in operations {
        state.storage.copy_path(&source, &destination).await?;
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn plan(
    state: &AppState,
    headers: &HeaderMap,
    query: &FileQuery,
    body: &BatchBody,
) -> AppResult<Vec<(ResolvedPath, ResolvedPath)>> {
    let share = resolve_share(state, headers, query).await?;
    ensure_writable(&share)?;
    validate_batch_size(&body.paths)?;
    let mut operations = Vec::with_capacity(body.paths.len());
    let lock_authorizer = FolderLockAuthorizer::new(state, headers).await;

    for source_path in &body.paths {
        ensure_non_root(source_path)?;
        let destination_path = batch_destination_path(&body.target, source_path)?;
        ensure_copy_target_outside_source(source_path, &destination_path)?;
        lock_authorizer.ensure_access(&share_storage_path(&share, source_path))?;
        lock_authorizer.ensure_access(&share_storage_path(&share, &destination_path))?;
        let source = resolve_existing_path(state, &share, source_path).await?;
        let destination = resolve_write_path(state, &share, &destination_path).await?;
        if operations
            .iter()
            .any(|(_, planned): &(ResolvedPath, ResolvedPath)| {
                planned.relative() == destination.relative()
            })
        {
            return Err(AppError::Conflict(
                "Multiple items have the same destination".into(),
            ));
        }
        operations.push((source, destination));
    }
    Ok(operations)
}
