//! Storage-specific background scheduling. Keep waits and retry policy here;
//! capacity scans and transaction recovery stay with their storage implementations.

use std::{
    sync::{atomic::Ordering, Arc, Weak},
    time::Duration,
};
use tokio::sync::Notify;

use super::{ActiveStorage, StorageBackendKind};
use crate::{
    capacity::CapacityTracker, error::AppResult, s3_backend::S3Backend, storage::StorageService,
};

pub(super) const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
const QUIET_PERIOD: Duration = Duration::from_secs(1);

pub(super) async fn reconcile_local_capacity(
    capacity: &CapacityTracker,
    storage: &StorageService,
) -> AppResult<()> {
    if !capacity.begin_reconciliation() {
        return Ok(());
    }
    match storage.reconcile_capacity_snapshot(capacity).await {
        Ok(()) => Ok(()),
        Err(error) => {
            capacity.reconciliation_failed();
            tracing::error!(%error, "failed to reconcile local capacity after a storage mutation error");
            Err(error)
        }
    }
}

pub(super) fn spawn_local_capacity_reconciler(
    active: Weak<ActiveStorage>,
    wake: Arc<Notify>,
    retry_min: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            wake.notified().await;
            let mut retry = retry_min;
            loop {
                let Some(current) = active.upgrade() else {
                    return;
                };
                if current.retired.load(Ordering::Acquire) {
                    return;
                }
                let lease = current.lifecycle.clone().try_read_owned();
                let Ok(_lease) = lease else {
                    drop(current);
                    tokio::time::sleep(retry).await;
                    continue;
                };
                let result = match &current.kind {
                    StorageBackendKind::Local(storage) => {
                        reconcile_local_capacity(&current.capacity, storage).await
                    }
                    StorageBackendKind::S3(_) => return,
                };
                drop(_lease);
                drop(current);
                if result.is_ok() {
                    break;
                }
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    })
}

pub(super) fn schedule_s3_capacity_reconcile(capacity: CapacityTracker, storage: S3Backend) {
    capacity.mark_uncertain();
    if !capacity.begin_reconciliation() {
        return;
    }
    tokio::spawn(async move {
        let mut retry = RETRY_MIN;
        loop {
            if storage.recovery_worker_stopped() {
                capacity.reconciliation_failed();
                return;
            }
            match storage.reconcile_capacity_snapshot(&capacity).await {
                Ok(()) => return,
                Err(error) => {
                    tracing::warn!(%error, retry_seconds = retry.as_secs(), "S3 capacity reconciliation will retry");
                }
            }
            tokio::time::sleep(retry).await;
            retry = (retry * 2).min(RETRY_MAX);
        }
    });
}

pub(crate) fn spawn_s3_recovery_reconciler(storage: S3Backend, capacity: CapacityTracker) {
    if !storage.begin_recovery_worker() {
        tracing::warn!("refused to start a duplicate S3 recovery worker");
        return;
    }
    tokio::spawn(async move {
        loop {
            if storage.recovery_worker_stopped() {
                return;
            }
            tokio::select! {
                _ = storage.wait_for_recovery_work() => {}
                _ = storage.wait_for_recovery_shutdown() => return,
            }
            if storage.recovery_worker_stopped() {
                return;
            }

            // A normal mutation usually creates and settles its journal while
            // holding the shared mutation gate. Give it a short quiet period
            // so successful requests do not trigger needless journal checks.
            tokio::select! {
                _ = tokio::time::sleep(QUIET_PERIOD) => {}
                _ = storage.wait_for_recovery_shutdown() => return,
            }
            if !storage.recovery_has_pending() {
                continue;
            }

            let mut retry = RETRY_MIN;
            let mut needs_capacity_reconcile = false;
            loop {
                if storage.recovery_worker_stopped() {
                    return;
                }
                if !storage.recovery_has_pending() {
                    break;
                }
                storage.runtime_recovery_started();
                match storage.recover_runtime_transactions(&capacity).await {
                    Ok(recovered) => {
                        if let Some(recovered) = recovered {
                            needs_capacity_reconcile = true;
                            tracing::info!(recovered, "runtime S3 recovery pass completed");
                            if storage.recovery_has_pending() {
                                // The pass released its gates. Give waiting edits
                                // and foreground owners a scheduling boundary.
                                tokio::task::yield_now().await;
                                retry = RETRY_MIN;
                                continue;
                            }
                        }
                        break;
                    }
                    Err(error) => {
                        needs_capacity_reconcile = true;
                        storage.runtime_recovery_failed(error.public_message().as_ref(), retry);
                        tracing::warn!(
                            %error,
                            retry_seconds = retry.as_secs(),
                            "runtime S3 recovery remains pending"
                        );
                    }
                }
                if storage.recovery_worker_stopped() {
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep(retry) => {}
                    _ = storage.wait_for_recovery_shutdown() => return,
                }
                retry = (retry * 2).min(RETRY_MAX);
            }
            // A foreground owner can settle the remaining journals between
            // passes. Do not leave an earlier recovery's capacity uncertainty
            // stranded just because the final call had no work left.
            if needs_capacity_reconcile {
                schedule_s3_capacity_reconcile(capacity.clone(), storage.clone());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capacity::load_capacity_ledger, storage_backend::StorageBackend,
        test_support::TestDirectory,
    };

    async fn local_fixture(label: &str) -> (TestDirectory, StorageBackend) {
        let fixture = TestDirectory::new(label);
        let root = fixture.path().join("files");
        let storage = StorageService::new(root.clone(), 1024, 1, 100, 0)
            .await
            .unwrap();
        tokio::fs::write(root.join("data.txt"), b"data")
            .await
            .unwrap();
        (fixture, StorageBackend::local(storage))
    }

    fn start_local_worker(backend: &mut StorageBackend) -> tokio::task::JoinHandle<()> {
        let wake = Arc::new(Notify::new());
        let active = Arc::get_mut(&mut backend.active).unwrap();
        active.capacity.set_reconcile_wake(&wake);
        active.local_reconcile_wake = Some(wake.clone());
        spawn_local_capacity_reconciler(
            Arc::downgrade(&backend.active),
            wake,
            Duration::from_millis(10),
        )
    }

    async fn join_worker(worker: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("local maintenance worker did not exit")
            .unwrap();
    }

    #[tokio::test]
    async fn failed_local_scan_releases_admission_and_preserves_last_known_usage() {
        let (fixture, backend) = local_fixture("maintenance-ledger-failure").await;
        let blocker = fixture.path().join("blocked-ledger");
        tokio::fs::write(&blocker, b"not a directory")
            .await
            .unwrap();
        let ledger = blocker.join("state.json");
        let capacity = CapacityTracker::new_with_ledger(Some(100), 91, false, Some(ledger.clone()));
        let StorageBackendKind::Local(storage) = &backend.active.kind else {
            panic!("expected local fixture");
        };

        assert!(reconcile_local_capacity(&capacity, storage).await.is_err());
        let status = capacity.status();
        assert_eq!(status.used, 91);
        assert!(!status.accurate);
        assert!(!status.reconciling);

        tokio::fs::remove_file(&blocker).await.unwrap();
        reconcile_local_capacity(&capacity, storage).await.unwrap();
        let status = capacity.status();
        assert_eq!(status.used, 4);
        assert!(status.accurate);
        assert!(!status.reconciling);
        assert_eq!(load_capacity_ledger(&ledger).await.unwrap(), Some(4));
    }

    #[tokio::test]
    async fn duplicate_local_scan_does_not_settle_the_current_owner() {
        let (fixture, backend) = local_fixture("maintenance-scan-owner").await;
        let blocker = fixture.path().join("blocked-ledger");
        tokio::fs::write(&blocker, b"not a directory")
            .await
            .unwrap();
        let capacity = CapacityTracker::new_with_ledger(
            Some(100),
            91,
            false,
            Some(blocker.join("state.json")),
        );
        assert!(capacity.begin_reconciliation());
        let before = capacity.status();
        let StorageBackendKind::Local(storage) = &backend.active.kind else {
            panic!("expected local fixture");
        };

        reconcile_local_capacity(&capacity, storage).await.unwrap();
        assert_eq!(capacity.status(), before);
        assert_eq!(tokio::fs::read(blocker).await.unwrap(), b"not a directory");
        capacity.reconciliation_failed();
    }

    #[tokio::test]
    async fn local_worker_waits_for_edit_without_retaining_the_instance_during_retry() {
        let (_fixture, mut backend) = local_fixture("maintenance-edit-gate").await;
        let worker = start_local_worker(&mut backend);
        let edit = backend.active.lifecycle.clone().write_owned().await;
        backend.active.capacity.mark_uncertain();
        // The current-thread runtime polls the worker into its retry wait.
        tokio::task::yield_now().await;
        assert!(!backend.capacity_status().accurate);
        assert!(!backend.capacity_status().reconciling);
        assert_eq!(Arc::strong_count(&backend.active), 1);

        drop(edit);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !backend.capacity_status().accurate {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("local scan did not resume after the edit gate opened");
        assert_eq!(backend.capacity_status().used, 4);
        drop(backend);
        join_worker(worker).await;
    }

    #[tokio::test]
    async fn retired_local_instance_does_not_scan_pending_capacity() {
        let (_fixture, mut backend) = local_fixture("maintenance-retired").await;
        let worker = start_local_worker(&mut backend);
        backend.active.retired.store(true, Ordering::Release);
        backend.active.capacity.mark_uncertain();
        join_worker(worker).await;

        assert_eq!(backend.capacity_status().used, 0);
        assert!(!backend.capacity_status().accurate);
        assert!(!backend.capacity_status().reconciling);
    }

    #[tokio::test]
    async fn dropping_idle_local_instance_wakes_and_finishes_its_worker() {
        let (_fixture, mut backend) = local_fixture("maintenance-idle-drop").await;
        let worker = start_local_worker(&mut backend);
        let instance = Arc::downgrade(&backend.active);
        tokio::task::yield_now().await;
        assert!(!worker.is_finished());
        assert_eq!(Arc::strong_count(&backend.active), 1);

        drop(backend);
        join_worker(worker).await;
        assert!(instance.upgrade().is_none());
    }
}
