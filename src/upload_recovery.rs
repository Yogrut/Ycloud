//! Automatic settlement of abandoned uploads; no administrator workflow.
use crate::{state::AppState, upload_batch::operation_id};

pub(crate) async fn recover(state: &AppState) {
    // Bounded by the batch store; process one storage at a time without adding
    // another queue. Active backends cannot be mistaken for abandoned work.
    let mut items = state.upload_batches.unknown_items().await;
    items.sort_by(|a, b| a.storage_id.cmp(&b.storage_id));
    for group in items.chunk_by(|a, b| a.storage_id == b.storage_id) {
        let Some(backend) = state.backends.cached(&group[0].storage_id).await else {
            continue;
        };
        let _exclusive = match backend.recover_abandoned_uploads().await {
            Ok(guard) => guard,
            Err(_) => continue, // Live owner or unavailable storage: retry later.
        };
        for item in group.iter().take(32) {
            match backend
                .recovered_upload_committed(
                    &item.path,
                    item.size,
                    &operation_id(&item.ticket, &item.path),
                )
                .await
            {
                Ok(committed) => {
                    let _ = state.upload_batches.resolve_unknown(item, committed).await;
                }
                Err(_) => break, // Keep cleanup ownership until storage returns.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::RequestSubject,
        error::{AppError, CleanupState, CommitState},
        test_support::{app_state, TestDirectory},
        upload_batch::UploadStatus,
    };
    use std::collections::HashMap;

    #[tokio::test]
    async fn recovery_settles_committed_and_missing_files_without_manual_review() {
        let directory = TestDirectory::new("automatic-upload-recovery");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                HashMap::from([
                    ("done.txt".into(), ("done.txt".into(), 7)),
                    ("missing.txt".into(), ("missing.txt".into(), 7)),
                ]),
            )
            .await
            .unwrap();
        for path in ["done.txt", "missing.txt"] {
            state
                .upload_batches
                .begin(&ticket, &owner, "primary", path, 7)
                .await
                .unwrap();
            state
                .upload_batches
                .finish_result::<()>(
                    &ticket,
                    path,
                    &Err(AppError::internal("response lost")
                        .with_operation(CommitState::Unknown, CleanupState::Unknown)),
                )
                .await;
        }
        tokio::fs::write(state.config.storage_path.join("done.txt"), b"content")
            .await
            .unwrap();
        let live_owner = state.storage_backend("primary").await.unwrap();
        recover(&state).await;
        assert_eq!(state.upload_batches.unknown_items().await.len(), 2);
        drop(live_owner);
        recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Complete);
        assert_eq!(status.items[1].status, UploadStatus::Failed);
        assert_eq!(
            status.items[1].operation.unwrap().commit,
            CommitState::NotCommitted
        );
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("done.txt"))
                .await
                .unwrap(),
            b"content"
        );
        state
            .upload_batches
            .ensure_storage_idle("primary")
            .await
            .unwrap();
    }
}
