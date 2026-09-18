//! On-demand browser statistics, separate from quota accounting and listing.
use crate::{
    error::{AppError, AppResult},
    file_access::{
        check_folder_lock_tree, ensure_storage_action, resolve_share, share_storage_path,
        FileQuery, StorageAction,
    },
    state::AppState,
    storage::StorageService,
};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Serialize;
use std::time::Duration;

const MAX_ENTRIES: usize = 100_000;
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize)]
pub(crate) struct DirectorySize {
    size: u64,
}

pub(crate) async fn calculate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<Json<DirectorySize>> {
    let share = resolve_share(&state, &headers, &query).await?;
    let relative = StorageService::normalize_relative(&share_storage_path(
        &share,
        query.path.as_deref().unwrap_or(""),
    ))?;
    check_folder_lock_tree(&state, &headers, &share.storage_id, &relative).await?;
    let _permit = state
        .directory_size_gate
        .try_acquire()
        .map_err(|_| AppError::ServiceUnavailable("正在计算其他目录，请稍后重试".into()))?;
    let backend = state.storage_backend(&share.storage_id).await?;
    let size = tokio::time::timeout(SCAN_TIMEOUT, backend.directory_size(&relative, MAX_ENTRIES))
        .await
        .map_err(|_| {
            AppError::ServiceUnavailable("目录大小计算超时，请选择较小的子目录".into())
        })??;
    // Do not return a measurement after access was revoked during the scan.
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Browse).await?;
    check_folder_lock_tree(&state, &headers, &share.storage_id, &relative).await?;
    Ok(Json(DirectorySize { size }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{ConfigFile, FolderLock},
        test_support::{app_state, TestDirectory},
    };
    use axum::http::header;

    #[tokio::test]
    async fn authorized_measurement_and_busy_response() {
        let fixture = TestDirectory::new("directory-size-api");
        let state = app_state(&fixture, ConfigFile::default()).await;
        tokio::fs::create_dir_all(fixture.path().join("storage/notes"))
            .await
            .unwrap();
        tokio::fs::write(fixture.path().join("storage/notes/readme.txt"), b"notes")
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        let token = state.sessions.create().await;
        headers.insert(header::COOKIE, format!("session={token}").parse().unwrap());
        let query = || {
            Query(FileQuery {
                path: Some("notes".into()),
                storage_id: Some("primary".into()),
                batch: None,
            })
        };
        assert_eq!(
            calculate(State(state.clone()), headers.clone(), query())
                .await
                .unwrap()
                .0
                .size,
            5
        );
        let _busy = state.directory_size_gate.acquire_many(2).await.unwrap();
        assert!(matches!(
            calculate(State(state.clone()), headers.clone(), query()).await,
            Err(AppError::ServiceUnavailable(_))
        ));
        drop(_busy);
        // A normal configured child lock applies to tree statistics too.
        state
            .config_file
            .write()
            .await
            .folder_locks
            .push(FolderLock {
                id: "notes-lock".into(),
                storage_id: "primary".into(),
                path: "notes/private".into(),
                password_hash: "unused-fixture-hash".into(),
            });
        assert!(matches!(
            calculate(State(state), headers, query()).await,
            Err(AppError::Forbidden)
        ));
    }
}
