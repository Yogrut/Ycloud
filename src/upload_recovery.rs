//! Automatic settlement of abandoned uploads; no administrator workflow.
use std::collections::{HashMap, HashSet};

use crate::{
    state::AppState,
    upload_batch::{namespace_id, operation_id, UnknownUpload},
};

const MAX_CONFIRMATIONS_PER_BACKEND: usize = 32;

/// Ordering hints owned by the single cleanup worker, not a second task store.
/// Offsets disappear when their backend has no bound unknown uploads.
#[derive(Default)]
pub(crate) struct UploadRecovery {
    next_offsets: HashMap<usize, usize>,
}

impl UploadRecovery {
    #[cfg(test)]
    pub(crate) async fn recover(&mut self, state: &AppState) {
        self.recover_with_budget(state, None, tokio_util::sync::CancellationToken::new())
            .await;
    }

    pub(crate) async fn recover_with_budget(
        &mut self,
        state: &AppState,
        budget: Option<std::time::Duration>,
        stop: tokio_util::sync::CancellationToken,
    ) {
        // Bounded by the batch store; process one storage at a time without adding
        // another queue. Active backends cannot be mistaken for abandoned work.
        let mut items = state.upload_batches.unknown_items().await;
        bind_recovery_backends(state, &mut items).await;
        items.sort_by(|a, b| {
            a.backend
                .as_ref()
                .map(|backend| backend.instance_key())
                .cmp(&b.backend.as_ref().map(|backend| backend.instance_key()))
                .then_with(|| a.ticket.cmp(&b.ticket))
                .then_with(|| a.path.cmp(&b.path))
        });
        let instances = items
            .iter()
            .filter_map(|item| item.backend.as_ref().map(|backend| backend.instance_key()))
            .collect::<HashSet<_>>();
        self.next_offsets
            .retain(|instance, _| instances.contains(instance));
        for group in items.chunk_by(|a, b| {
            a.backend.as_ref().map(|v| v.instance_key())
                == b.backend.as_ref().map(|v| v.instance_key())
        }) {
            if stop.is_cancelled() {
                break;
            }
            let Some(backend) = &group[0].backend else {
                continue;
            };
            // Give each backend its own window: a slow first storage must not
            // consume the entire pass and permanently starve later instances.
            let deadline = budget.map(|budget| tokio::time::Instant::now() + budget);
            let _exclusive = match backend
                .recover_abandoned_uploads_before(deadline, Some(stop.clone()))
                .await
            {
                Ok(guard) => guard,
                Err(_) => continue, // Live owner or unavailable storage: retry later.
            };
            let next_offset = self.next_offsets.entry(backend.instance_key()).or_default();
            *next_offset %= group.len();
            for item in confirmation_window(group, *next_offset) {
                if stop.is_cancelled()
                    || deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
                {
                    break;
                }
                // Advance for every attempted confirmation, including conflicts and
                // storage errors, so one failing item cannot pin the next window.
                *next_offset = (*next_offset + 1) % group.len();
                match backend
                    .recovered_upload_committed(
                        &item.path,
                        item.size,
                        &operation_id(&item.ticket, &item.path),
                    )
                    .await
                {
                    Ok(true) => {
                        let _ = state.upload_batches.confirm_unknown_committed(item).await;
                    }
                    Ok(false) => continue, // Missing proof cannot establish that publication never happened.
                    Err(crate::error::AppError::Conflict(_)) => continue, // One operation remains uncertain; do not starve the others.
                    Err(_) => break, // Keep cleanup ownership until storage returns.
                }
            }
        }
    }
}

fn confirmation_window(
    items: &[UnknownUpload],
    offset: usize,
) -> impl Iterator<Item = &UnknownUpload> {
    let start = if items.is_empty() {
        0
    } else {
        offset % items.len()
    };
    items[start..]
        .iter()
        .chain(&items[..start])
        .take(MAX_CONFIRMATIONS_PER_BACKEND)
}

/// Resolve missing owners once per storage in this pass. Existing owner
/// references take precedence, including removed/replaced storage generations.
async fn bind_recovery_backends(state: &AppState, items: &mut [UnknownUpload]) {
    if items.iter().all(|item| item.backend.is_some()) {
        return;
    }
    let namespaces = {
        let configured = state.config_file.read().await;
        let mut namespaces = HashMap::new();
        for item in items
            .iter()
            .filter(|item| item.backend.is_none() && item.namespace_id.is_some())
        {
            if !namespaces.contains_key(&item.storage_id) {
                let current = configured
                    .storage_instances
                    .iter()
                    .find(|instance| instance.id == item.storage_id)
                    .and_then(|instance| namespace_id(&state.config, &instance.backend).ok());
                namespaces.insert(item.storage_id.clone(), current);
            }
        }
        namespaces
    }; // Release the configuration lock before any asynchronous backend lookup.
    let mut backends = HashMap::new();
    for item in items {
        if item.backend.is_some() {
            continue;
        }
        if let Some(expected) = &item.namespace_id {
            let current = namespaces.get(&item.storage_id).and_then(Option::as_deref);
            if current != Some(expected.as_str()) {
                continue; // The ID now points at a different physical namespace.
            }
        }
        if !backends.contains_key(&item.storage_id) {
            // Cache unavailable entries too; another pass will resolve them anew.
            backends.insert(
                item.storage_id.clone(),
                state.backends.cached(&item.storage_id).await,
            );
        }
        item.backend = backends
            .get(&item.storage_id)
            .and_then(Option::as_ref)
            .cloned();
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

    fn unbound_item(storage_id: &str, namespace: Option<&str>, path: &str) -> UnknownUpload {
        UnknownUpload {
            backend: None,
            ticket: "original-ticket".into(),
            storage_id: storage_id.into(),
            namespace_id: namespace.map(str::to_owned),
            path: path.into(),
            size: 7,
            operation: AppError::internal("response lost")
                .with_operation(CommitState::Unknown, CleanupState::Unknown)
                .operation(),
        }
    }

    #[test]
    fn confirmation_windows_wrap_without_duplicates_and_keep_the_budget() {
        for count in [
            0,
            1,
            MAX_CONFIRMATIONS_PER_BACKEND - 1,
            MAX_CONFIRMATIONS_PER_BACKEND,
            MAX_CONFIRMATIONS_PER_BACKEND + 1,
            MAX_CONFIRMATIONS_PER_BACKEND * 2,
        ] {
            let items = (0..count)
                .map(|index| unbound_item("primary", None, &format!("{index}.txt")))
                .collect::<Vec<_>>();
            for offset in [0, 1, count.saturating_sub(1), usize::MAX] {
                let paths = confirmation_window(&items, offset)
                    .map(|item| item.path.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(paths.len(), count.min(MAX_CONFIRMATIONS_PER_BACKEND));
                assert_eq!(paths.iter().collect::<HashSet<_>>().len(), paths.len());
                if count > 0 {
                    let expected = (0..paths.len())
                        .map(|index| format!("{}.txt", (offset % count + index) % count))
                        .collect::<Vec<_>>();
                    assert_eq!(paths, expected);
                }
            }
        }
    }

    #[test]
    fn consecutive_windows_visit_every_item_in_a_stable_group() {
        for count in [
            MAX_CONFIRMATIONS_PER_BACKEND + 1,
            MAX_CONFIRMATIONS_PER_BACKEND * 2,
            MAX_CONFIRMATIONS_PER_BACKEND * 3 + 7,
        ] {
            let items = (0..count)
                .map(|index| unbound_item("primary", None, &format!("{index}.txt")))
                .collect::<Vec<_>>();
            let mut visited = HashSet::new();
            let mut offset = 0;
            for _ in 0..count.div_ceil(MAX_CONFIRMATIONS_PER_BACKEND) {
                let window = confirmation_window(&items, offset).collect::<Vec<_>>();
                visited.extend(window.iter().map(|item| item.path.as_str()));
                offset = (offset + window.len()) % count;
            }
            assert_eq!(visited.len(), count);
        }
    }

    #[tokio::test]
    async fn confirmation_errors_advance_the_cursor_without_losing_the_bad_receipt() {
        let directory = TestDirectory::new("upload-recovery-error-cursor");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                HashMap::from([
                    ("a-bad.txt".into(), ("a-bad.txt".into(), 7)),
                    ("b-proven.txt".into(), ("b-proven.txt".into(), 7)),
                ]),
            )
            .await
            .unwrap();
        for path in ["a-bad.txt", "b-proven.txt"] {
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
        let backend = state.storage_backend("primary").await.unwrap();
        let instance = backend.instance_key();
        let mut input = crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
        input.operation_id = Some(operation_id(&ticket, "b-proven.txt"));
        backend
            .upload_tracked_new_file("b-proven.txt", input, Some(7), 100, None)
            .await
            .unwrap();
        drop(backend);
        let bad_receipt = state
            .config
            .storage_path
            .join(".ycloud-system/upload-receipts")
            .join(format!("{}.json", operation_id(&ticket, "a-bad.txt")));
        tokio::fs::write(&bad_receipt, b"invalid JSON fixture")
            .await
            .unwrap();

        let mut recovery = UploadRecovery::default();
        recovery.recover(&state).await;
        assert_eq!(recovery.next_offsets[&instance], 1);
        assert_eq!(state.upload_batches.unknown_items().await.len(), 2);
        recovery.recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Unknown);
        assert_eq!(status.items[1].status, UploadStatus::Complete);
        assert_eq!(
            tokio::fs::read(&bad_receipt).await.unwrap(),
            b"invalid JSON fixture"
        );
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("b-proven.txt"))
                .await
                .unwrap(),
            b"content"
        );
    }

    #[tokio::test]
    async fn cursor_hints_follow_only_current_bound_work_and_keep_unknown_results() {
        let directory = TestDirectory::new("upload-recovery-cursor-lifecycle");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                HashMap::from([("pending.txt".into(), ("pending.txt".into(), 7))]),
            )
            .await
            .unwrap();
        state
            .upload_batches
            .begin(&ticket, &owner, "primary", "pending.txt", 7)
            .await
            .unwrap();
        state
            .upload_batches
            .finish_result::<()>(
                &ticket,
                "pending.txt",
                &Err(AppError::internal("response lost")
                    .with_operation(CommitState::Unknown, CleanupState::Unknown)),
            )
            .await;
        let live_owner = state.storage_backend("primary").await.unwrap();
        let key = live_owner.instance_key();
        let unused_key = key.wrapping_add(1);
        let mut recovery = UploadRecovery {
            next_offsets: HashMap::from([(key, usize::MAX), (unused_key, 0)]),
        };
        recovery.recover(&state).await;
        assert_eq!(recovery.next_offsets, HashMap::from([(key, usize::MAX)]));
        drop(live_owner);
        recovery.recover(&state).await;
        assert_eq!(recovery.next_offsets, HashMap::from([(key, 0)]));
        state.backends.insert_unavailable("primary").await;
        recovery.recover(&state).await;
        assert!(recovery.next_offsets.is_empty());
        assert_eq!(state.upload_batches.unknown_items().await.len(), 1);
    }

    #[tokio::test]
    async fn binding_checks_each_namespace_and_preserves_original_owners_and_identity() {
        let directory = TestDirectory::new("upload-recovery-binding");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let current = state.backends.cached("primary").await.unwrap();
        let old_directory = TestDirectory::new("upload-recovery-old-owner");
        let old_state = app_state(
            &old_directory,
            crate::config::ConfigFile::with_test_storage(),
        )
        .await;
        let old = old_state.backends.cached("primary").await.unwrap();
        let namespace = namespace_id(
            &state.config,
            &state.config_file.read().await.storage_instances[0].backend,
        )
        .unwrap();
        let mut items = vec![
            unbound_item("primary", Some("different"), "wrong-first.txt"),
            unbound_item("primary", Some(&namespace), "matching.txt"),
            unbound_item("primary", None, "legacy.txt"),
            unbound_item("primary", Some("different"), "wrong-last.txt"),
            unbound_item("primary", Some("different"), "old-owner.txt"),
        ];
        items[4].backend = Some(old.clone());
        let identity = serde_json::to_value(&items).unwrap();

        bind_recovery_backends(&state, &mut items).await;

        assert!(items[0].backend.is_none());
        assert!(items[3].backend.is_none());
        for index in [1, 2] {
            assert_eq!(
                items[index].backend.as_ref().unwrap().instance_key(),
                current.instance_key()
            );
        }
        assert_ne!(old.instance_key(), current.instance_key());
        assert_eq!(
            items[4].backend.as_ref().unwrap().instance_key(),
            old.instance_key()
        );
        assert_eq!(serde_json::to_value(&items).unwrap(), identity);
    }

    #[tokio::test]
    async fn binding_retries_unavailable_backends_on_the_next_pass_including_disabled_storage() {
        let directory = TestDirectory::new("upload-recovery-binding-retry");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let backend = state.backends.cached("primary").await.unwrap();
        state.backends.insert_unavailable("primary").await;
        let mut items = vec![
            unbound_item("primary", None, "first.txt"),
            unbound_item("primary", None, "second.txt"),
        ];
        bind_recovery_backends(&state, &mut items).await;
        assert!(items.iter().all(|item| item.backend.is_none()));

        state
            .backends
            .insert_ready("primary", backend.clone())
            .await;
        state.backends.set_enabled("primary", false).await;
        bind_recovery_backends(&state, &mut items).await;
        assert!(items
            .iter()
            .all(|item| item.backend.as_ref().unwrap().instance_key() == backend.instance_key()));
        assert!(matches!(
            state.backends.get("primary").await,
            Err(AppError::Forbidden)
        ));
    }

    #[tokio::test]
    async fn binding_does_not_adopt_missing_or_unresolvable_namespaces() {
        let directory = TestDirectory::new("upload-recovery-binding-invalid-namespace");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let backend = state.backends.cached("primary").await.unwrap();
        state
            .backends
            .insert_ready("missing", backend.clone())
            .await;
        {
            let mut configured = state.config_file.write().await;
            let crate::config::StorageBackendConfig::Local(settings) =
                &mut configured.storage_instances[0].backend
            else {
                panic!("local fixture expected");
            };
            settings.mount_id = "missing-mount".into();
        }
        let mut items = vec![
            unbound_item("primary", Some("expected"), "unresolvable.txt"),
            unbound_item("missing", Some("expected"), "missing-config.txt"),
            unbound_item("missing", None, "legacy-missing-config.txt"),
            unbound_item("unregistered", None, "missing-backend.txt"),
        ];
        bind_recovery_backends(&state, &mut items).await;
        assert!(items[0].backend.is_none());
        assert!(items[1].backend.is_none());
        assert_eq!(
            items[2].backend.as_ref().unwrap().instance_key(),
            backend.instance_key()
        );
        assert!(items[3].backend.is_none());
    }

    #[tokio::test]
    async fn binding_refreshes_namespaces_without_replacing_an_existing_owner() {
        let directory = TestDirectory::new("upload-recovery-binding-new-namespace");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let namespace = namespace_id(
            &state.config,
            &state.config_file.read().await.storage_instances[0].backend,
        )
        .unwrap();
        let mut owned = [unbound_item("primary", Some(&namespace), "owned.txt")];
        bind_recovery_backends(&state, &mut owned).await;
        let owner = owned[0].backend.as_ref().unwrap().instance_key();
        let previous = state.config_file.write().await.storage_instances.remove(0);
        let mut unbound = [unbound_item("primary", Some(&namespace), "unbound.txt")];
        bind_recovery_backends(&state, &mut unbound).await;
        assert!(unbound[0].backend.is_none());
        bind_recovery_backends(&state, &mut owned).await;
        assert_eq!(owned[0].backend.as_ref().unwrap().instance_key(), owner);

        state
            .config_file
            .write()
            .await
            .storage_instances
            .push(previous);
        bind_recovery_backends(&state, &mut unbound).await;
        assert_eq!(unbound[0].backend.as_ref().unwrap().instance_key(), owner);
        bind_recovery_backends(&state, &mut []).await;
    }

    #[tokio::test]
    async fn unresolved_head_items_do_not_starve_a_later_proven_upload() {
        let directory = TestDirectory::new("upload-recovery-unresolved-prefix");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let count = MAX_CONFIRMATIONS_PER_BACKEND * 2 + 1;
        let paths = (0..count)
            .map(|index| format!("{index:03}.txt"))
            .collect::<Vec<_>>();
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                paths
                    .iter()
                    .map(|path| (path.clone(), (path.clone(), 7)))
                    .collect(),
            )
            .await
            .unwrap();
        for path in &paths {
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
        // Place the proof outside both the old map-order prefix and the stable
        // sorted prefix, so merely sorting without rotation cannot pass.
        let snapshots = state.upload_batches.unknown_items().await;
        let old_prefix = snapshots[..MAX_CONFIRMATIONS_PER_BACKEND]
            .iter()
            .map(|item| item.path.as_str())
            .collect::<HashSet<_>>();
        let proven = paths[MAX_CONFIRMATIONS_PER_BACKEND..]
            .iter()
            .find(|path| !old_prefix.contains(path.as_str()))
            .unwrap();
        let backend = state.storage_backend("primary").await.unwrap();
        let mut input = crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
        input.operation_id = Some(operation_id(&ticket, proven));
        backend
            .upload_tracked_new_file(proven, input, Some(7), 100, None)
            .await
            .unwrap();
        drop(backend);

        let mut recovery = UploadRecovery::default();
        for _ in 0..count.div_ceil(MAX_CONFIRMATIONS_PER_BACKEND) {
            recovery.recover(&state).await;
        }
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(
            status
                .items
                .iter()
                .find(|item| item.path == *proven)
                .unwrap()
                .status,
            UploadStatus::Complete
        );
        assert_eq!(state.upload_batches.unknown_items().await.len(), count - 1);
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join(proven))
                .await
                .unwrap(),
            b"content"
        );
    }

    #[tokio::test]
    async fn recovery_confirmation_budget_leaves_remaining_proofs_for_the_next_pass() {
        let directory = TestDirectory::new("upload-recovery-confirmation-budget");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let count = MAX_CONFIRMATIONS_PER_BACKEND + 3;
        let paths = (0..count)
            .map(|index| format!("{index}.txt"))
            .collect::<Vec<_>>();
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                paths
                    .iter()
                    .map(|path| (path.clone(), (path.clone(), 7)))
                    .collect(),
            )
            .await
            .unwrap();
        let backend = state.storage_backend("primary").await.unwrap();
        for path in &paths {
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
            let mut input =
                crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
            input.operation_id = Some(operation_id(&ticket, path));
            backend
                .upload_tracked_new_file(path, input, Some(7), 100, None)
                .await
                .unwrap();
        }
        drop(backend);

        let mut recovery = UploadRecovery::default();
        recovery.recover(&state).await;
        assert_eq!(state.upload_batches.unknown_items().await.len(), 3);
        recovery.recover(&state).await;
        assert!(state.upload_batches.unknown_items().await.is_empty());
        recovery.recover(&state).await;
        assert!(recovery.next_offsets.is_empty());
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert!(status
            .items
            .iter()
            .all(|item| item.status == UploadStatus::Complete));
    }

    #[tokio::test]
    async fn matching_namespace_keeps_the_original_operation_proof() {
        let directory = TestDirectory::new("matching-namespace-upload-recovery");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let namespace = crate::upload_batch::namespace_id(
            &state.config,
            &state.config_file.read().await.storage_instances[0].backend,
        )
        .unwrap();
        let ticket = state
            .upload_batches
            .create_for_account_with_namespace(
                owner.clone(),
                "user:1".into(),
                "primary".into(),
                HashMap::from([
                    ("proven.txt".into(), ("proven.txt".into(), 7)),
                    ("unproven.txt".into(), ("unproven.txt".into(), 7)),
                ]),
                Some(namespace),
            )
            .await
            .unwrap();
        for path in ["proven.txt", "unproven.txt"] {
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
        let backend = state.storage_backend("primary").await.unwrap();
        let mut input = crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
        input.operation_id = Some(operation_id(&ticket, "proven.txt"));
        backend
            .upload_tracked_new_file("proven.txt", input, Some(7), 100, None)
            .await
            .unwrap();
        drop(backend);
        tokio::fs::write(state.config.storage_path.join("unproven.txt"), b"content")
            .await
            .unwrap();

        UploadRecovery::default().recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Complete);
        assert_eq!(status.items[1].status, UploadStatus::Unknown);
    }

    #[tokio::test]
    async fn unavailable_current_backend_does_not_discard_the_unknown_ticket() {
        let directory = TestDirectory::new("unavailable-upload-recovery");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let owner = RequestSubject::Session("owner".into());
        let ticket = state
            .upload_batches
            .create(
                owner.clone(),
                "primary".into(),
                HashMap::from([("missing.txt".into(), ("missing.txt".into(), 7))]),
            )
            .await
            .unwrap();
        state
            .upload_batches
            .begin(&ticket, &owner, "primary", "missing.txt", 7)
            .await
            .unwrap();
        state
            .upload_batches
            .finish_result::<()>(
                &ticket,
                "missing.txt",
                &Err(AppError::internal("response lost")
                    .with_operation(CommitState::Unknown, CleanupState::Unknown)),
            )
            .await;
        state.backends.insert_unavailable("primary").await;

        UploadRecovery::default().recover(&state).await;
        let unknown = state.upload_batches.unknown_items().await;
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].ticket, ticket);
        assert!(unknown[0].backend.is_none());
    }

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
        UploadRecovery::default().recover(&state).await;
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
                    ("proven.txt".into(), ("proven.txt".into(), 7)),
                ]),
            )
            .await
            .unwrap();
        for path in ["ambiguous.txt", "proven.txt"] {
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
        let live_owner = state.storage_backend("primary").await.unwrap();
        let mut input = crate::s3_backend::UploadInput::relay(axum::body::Body::from("content"));
        input.operation_id = Some(operation_id(&ticket, "proven.txt"));
        live_owner
            .upload_tracked_new_file("proven.txt", input, Some(7), 100, None)
            .await
            .unwrap();
        drop(live_owner);
        let transaction_id = uuid::Uuid::new_v4();
        tokio::fs::write(
            state.config.storage_path.join(".ycloud-system/transactions").join(format!("{transaction_id}.json")),
            serde_json::to_vec(&serde_json::json!({
                "version": 2, "id": transaction_id.to_string(), "destination": "ambiguous.txt", "published": false,
                "operation_id": operation_id(&ticket, "ambiguous.txt"), "operation_size": 7
            })).unwrap(),
        ).await.unwrap();
        UploadRecovery::default().recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Unknown);
        assert_eq!(status.items[1].status, UploadStatus::Complete);
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
        let retired = state.upload_batches.unknown_items().await[0]
            .backend
            .clone()
            .unwrap();
        crate::test_support::wait_storage_settled(&retired).await;
        UploadRecovery::default().recover(&state).await;
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
    async fn recovery_confirms_proven_uploads_without_guessing_missing_results() {
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
        let mut recovery = UploadRecovery::default();
        recovery.recover(&state).await;
        assert_eq!(state.upload_batches.unknown_items().await.len(), 3);
        drop(live_owner);
        recovery.recover(&state).await;
        let status = state
            .upload_batches
            .status(&ticket, &owner, "primary")
            .await
            .unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Complete);
        assert_eq!(status.items[1].status, UploadStatus::Unknown);
        assert_eq!(status.items[2].status, UploadStatus::Unknown);
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("done.txt"))
                .await
                .unwrap(),
            b"content"
        );
        assert!(matches!(
            state.upload_batches.ensure_storage_idle("primary").await,
            Err(AppError::Conflict(_))
        ));
    }
}
