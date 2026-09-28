//! Automatic settlement of abandoned uploads; no administrator workflow.
use crate::{state::AppState, upload_batch::operation_id};

pub(crate) async fn recover(state: &AppState) {
    // Bounded by the batch store; process one storage at a time without adding
    // another queue. Active backends cannot be mistaken for abandoned work.
    let mut items = state.upload_batches.unknown_items().await;
    let configured = state.config_file.read().await.clone();
    for item in &mut items {
        if item.backend.is_none() {
            if let Some(expected) = &item.namespace_id {
                let current = configured
                    .storage_instances
                    .iter()
                    .find(|instance| instance.id == item.storage_id)
                    .and_then(|instance| {
                        crate::upload_batch::namespace_id(&state.config, &instance.backend).ok()
                    });
                if current.as_deref() != Some(expected.as_str()) {
                    continue; // The ID now points at a different physical namespace.
                }
            }
            item.backend = state.backends.cached(&item.storage_id).await;
        }
    }
    items.sort_by_key(|item| item.backend.as_ref().map(|backend| backend.instance_key()));
    for group in items.chunk_by(|a, b| {
        a.backend.as_ref().map(|v| v.instance_key()) == b.backend.as_ref().map(|v| v.instance_key())
    }) {
        let Some(backend) = &group[0].backend else {
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
                Err(crate::error::AppError::Conflict(_)) => continue, // One operation remains uncertain; do not starve the others.
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
    async fn changed_storage_namespace_does_not_turn_old_upload_into_a_failure() {
        let directory = TestDirectory::new("changed-namespace-upload-recovery");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create_for_account_with_namespace(
                owner.clone(),
                "user:1".into(),
                "primary".into(),
                HashMap::from([("same.txt".into(), ("same.txt".into(), 7))]),
                Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".into()),
            )
            .await
            .unwrap();
        state
            .upload_batches
            .begin(&ticket, &owner, "primary", "same.txt", 7)
            .await
            .unwrap();
        state
            .upload_batches
            .finish_result::<()>(
                &ticket,
                "same.txt",
                &Err(AppError::internal("response lost")
                    .with_operation(CommitState::Unknown, CleanupState::Unknown)),
            )
            .await;
        tokio::fs::write(state.config.storage_path.join("same.txt"), b"content")
            .await
            .unwrap();
        recover(&state).await;
        assert_eq!(state.upload_batches.unknown_items().await.len(), 1);
    }

    #[tokio::test]
    async fn uncertain_local_publication_does_not_block_other_recovery_items() {
        let directory = TestDirectory::new("uncertain-local-upload-recovery");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                HashMap::from([
                    ("ambiguous.txt".into(), ("ambiguous.txt".into(), 7)),
                    ("missing.txt".into(), ("missing.txt".into(), 7)),
                ]),
            )
            .await
            .unwrap();
        for path in ["ambiguous.txt", "missing.txt"] {
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
        tokio::fs::write(state.config.storage_path.join("ambiguous.txt"), b"content")
            .await
            .unwrap();
        let transaction_id = uuid::Uuid::new_v4();
        tokio::fs::write(
            state.config.storage_path.join(".ycloud-system/transactions").join(format!("{transaction_id}.json")),
            serde_json::to_vec(&serde_json::json!({
                "version": 2, "id": transaction_id.to_string(), "destination": "ambiguous.txt", "published": false,
                "operation_id": operation_id(&ticket, "ambiguous.txt"), "operation_size": 7
            })).unwrap(),
        ).await.unwrap();
        recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Unknown);
        assert_eq!(status.items[1].status, UploadStatus::Failed);
    }

    #[tokio::test]
    async fn deleted_storage_keeps_its_original_cleanup_backend() {
        let directory = TestDirectory::new("removed-storage-recovery");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let subject = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create(
                subject.clone(),
                "primary".into(),
                HashMap::from([("done.txt".into(), ("done.txt".into(), 7))]),
            )
            .await
            .unwrap();
        state
            .upload_batches
            .begin(&ticket, &subject, "primary", "done.txt", 7)
            .await
            .unwrap();
        state
            .upload_batches
            .finish_result::<()>(
                &ticket,
                "done.txt",
                &Err(AppError::internal("response lost")
                    .with_operation(CommitState::Unknown, CleanupState::Unknown)),
            )
            .await;
        let backend = state.storage_backend("primary").await.unwrap();
        let mut input = crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
        input.operation_id = Some(operation_id(&ticket, "done.txt"));
        backend
            .upload_tracked_new_file("done.txt", input, Some(7), 100, None)
            .await
            .unwrap();
        drop(backend);
        state.delete_storage("primary").await.unwrap();
        assert!(state.backends.cached("primary").await.is_none());
        assert!(state.upload_batches.unknown_items().await[0]
            .backend
            .is_some());
        recover(&state).await;
        assert!(state.upload_batches.unknown_items().await.is_empty());
        assert_eq!(
            state
                .upload_batches
                .status(&ticket, &subject, "primary")
                .await
                .unwrap()
                .items[0]
                .status,
            UploadStatus::Complete
        );
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("done.txt"))
                .await
                .unwrap(),
            b"content"
        );
    }

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
                    ("unproven.txt".into(), ("unproven.txt".into(), 7)),
                ]),
            )
            .await
            .unwrap();
        for path in ["done.txt", "missing.txt", "unproven.txt"] {
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
        let live_owner = state.storage_backend("primary").await.unwrap();
        let mut input = crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
        input.operation_id = Some(operation_id(&ticket, "done.txt"));
        live_owner
            .upload_tracked_new_file("done.txt", input, Some(7), 100, None)
            .await
            .unwrap();
        tokio::fs::write(state.config.storage_path.join("unproven.txt"), b"content")
            .await
            .unwrap();
        recover(&state).await;
        assert_eq!(state.upload_batches.unknown_items().await.len(), 3);
        drop(live_owner);
        recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Complete);
        assert_eq!(status.items[1].status, UploadStatus::Failed);
        assert_eq!(status.items[2].status, UploadStatus::Failed);
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
