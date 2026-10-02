//! Online cleanup of committed staged deletions. Disk entries, not wakeup
//! messages, are the durable work list; startup recovery remains the fallback.
use crate::{
    error::AppResult,
    storage_transaction::{
        RemovalProgress, StagedDeletion, TransactionPaths, DELETION_NODES_PER_PASS,
    },
};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Semaphore};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CleanupStatus {
    /// Failures in the last batch, not a byte count or total capacity debt.
    pub pending_deletions: usize,
    pub more_pending: bool,
    pub failed_passes: u64,
    pub scan_failed: bool,
    /// Upper bound recorded when a user-visible deletion was published. It is
    /// reduced only after physical removal is confirmed.
    pub pending_bytes_upper_bound: u64,
    /// False when the online inventory contains untracked/legacy work, is
    /// truncated, or cannot be scanned. The byte value remains useful as the
    /// known portion but must not be presented as the entire debt.
    pub pending_bytes_complete: bool,
}

// The reported total and its source entries share one lock. No stale total
// may be published after a cleanup pass has retired the corresponding debt.
struct CleanupState {
    status: CleanupStatus,
    debt: HashMap<PathBuf, u64>,
}

impl Default for CleanupState {
    fn default() -> Self {
        Self {
            status: CleanupStatus {
                pending_bytes_complete: true,
                ..CleanupStatus::default()
            },
            debt: HashMap::new(),
        }
    }
}

impl CleanupState {
    fn from_inventory(candidates: &[StagedDeletion], more: bool) -> Self {
        let mut state = Self::default();
        state.status.more_pending = more || !candidates.is_empty();
        state.status.pending_bytes_complete = !more
            && candidates
                .iter()
                .all(|item| item.bytes_upper_bound.is_some());
        for item in candidates {
            if let Some(bytes) = item.bytes_upper_bound {
                state.debt.insert(item.path.clone(), bytes);
            }
        }
        state.update_pending_bytes();
        state
    }

    fn record_deletion(&mut self, path: PathBuf, bytes: u64) {
        self.debt.insert(path, bytes);
        self.update_pending_bytes();
    }

    // Recompute instead of subtracting from a saturated total: once an upper
    // bound overflows, subtraction alone cannot recover the remaining bytes.
    fn update_pending_bytes(&mut self) {
        self.status.pending_bytes_upper_bound = self
            .debt
            .values()
            .fold(0_u64, |total, value| total.saturating_add(*value));
    }

    // Only apply observed completions, not an entire scan snapshot. Foreground
    // deletions recorded after the scan must keep their accounting entries.
    fn apply_pass(&mut self, result: AppResult<CleanupPass>) -> CleanupStatus {
        match result {
            Ok(pass) => {
                for (path, bytes) in pass.tracked {
                    self.debt.entry(path).or_insert(bytes);
                }
                for path in pass.completed {
                    self.debt.remove(&path);
                }
                let all_pending_tracked = pass
                    .failed
                    .iter()
                    .chain(&pass.partial)
                    .all(|path| self.debt.contains_key(path));
                self.status.pending_deletions = pass.failed.len();
                self.status.more_pending = pass.more;
                self.status.scan_failed = false;
                self.update_pending_bytes();
                self.status.pending_bytes_complete = !pass.more && all_pending_tracked;
            }
            Err(_) => {
                self.status.scan_failed = true;
                self.status.pending_bytes_complete = false;
            }
        }
        if self.status.scan_failed || self.status.pending_deletions != 0 {
            self.status.failed_passes = self.status.failed_passes.saturating_add(1);
        }
        self.status
    }
}

struct CleanupPass {
    failed: Vec<PathBuf>,
    partial: Vec<PathBuf>,
    completed: Vec<PathBuf>,
    tracked: Vec<(PathBuf, u64)>,
    more: bool,
}

#[cfg(test)]
impl CleanupPass {
    fn summary(&self) -> (usize, bool) {
        (self.failed.len(), self.more)
    }
}

#[derive(Clone)]
pub(super) struct CleanupWorker {
    wake: mpsc::Sender<()>,
    state: Arc<Mutex<CleanupState>>,
}

impl CleanupWorker {
    pub(super) async fn start(
        paths: Arc<TransactionPaths>,
        io: Arc<Semaphore>,
        mutation: Arc<AsyncMutex<()>>,
    ) -> AppResult<Self> {
        // Recovery may leave a bounded cleanup backlog. Seed truthful debt
        // before exposing the service, then wake the existing worker promptly.
        let (candidates, more) = paths.staged_deletions().await?;
        let state = CleanupState::from_inventory(&candidates, more);
        let pending = state.status.more_pending;
        let worker = Self::start_with_state(paths, io, mutation, Duration::from_secs(1), state);
        if pending {
            worker.notify();
        }
        Ok(worker)
    }

    fn start_with_state(
        paths: Arc<TransactionPaths>,
        io: Arc<Semaphore>,
        mutation: Arc<AsyncMutex<()>>,
        retry_min: Duration,
        initial_state: CleanupState,
    ) -> Self {
        // Coalesce wakeups. A large delete batch must not allocate one queued
        // message per file; every pass discovers work from its owned directory.
        let (wake, receiver) = mpsc::channel(1);
        let state = Arc::new(Mutex::new(initial_state));
        tokio::spawn(run(paths, io, mutation, receiver, state.clone(), retry_min));
        Self { wake, state }
    }

    pub(super) fn notify(&self) {
        let _ = self.wake.try_send(());
    }

    pub(super) fn record_deletion(&self, path: PathBuf, bytes: u64) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_deletion(path, bytes);
    }

    pub(super) fn status(&self) -> CleanupStatus {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .status
    }
}

async fn cleanup_pass<F, Fut>(paths: &TransactionPaths, mut remove: F) -> AppResult<CleanupPass>
where
    F: FnMut(PathBuf, usize) -> Fut,
    Fut: Future<Output = AppResult<RemovalProgress>>,
{
    // Inventory validates every bounded identifier/type before any removal.
    let (candidates, more) = paths.staged_deletions().await?;
    let mut failed = Vec::new();
    let mut partial = Vec::new();
    let mut completed = Vec::new();
    let mut tracked = Vec::new();
    let mut remaining = DELETION_NODES_PER_PASS;
    let mut budget_exhausted = false;
    for item in candidates {
        if let Some(bytes) = item.bytes_upper_bound {
            tracked.push((item.path.clone(), bytes));
        }
        if remaining == 0 {
            partial.push(item.path);
            budget_exhausted = true;
            continue;
        }
        match remove(item.path.clone(), remaining).await {
            Ok(progress) if progress.complete => {
                remaining = remaining.saturating_sub(progress.removed_nodes);
                // The physical deletion must reach its directory durability
                // boundary before its durable debt record is retired.
                if paths.sync_parent_of(&item.path).await.is_err()
                    || paths.finish_staged_deletion(&item).await.is_err()
                {
                    failed.push(item.path);
                } else {
                    completed.push(item.path);
                }
            }
            Ok(progress) => {
                remaining = remaining.saturating_sub(progress.removed_nodes);
                partial.push(item.path);
                budget_exhausted = true;
            }
            Err(_) => failed.push(item.path),
        }
    }
    // If this fails, keep reporting an incomplete pass and retry the directory
    // sync even when all entries have already disappeared.
    paths
        .sync_parent_of(&paths.trash.join("sync-boundary"))
        .await?;
    Ok(CleanupPass {
        failed,
        partial,
        completed,
        tracked,
        more: more || budget_exhausted,
    })
}

async fn run(
    paths: Arc<TransactionPaths>,
    io: Arc<Semaphore>,
    mutation: Arc<AsyncMutex<()>>,
    mut receiver: mpsc::Receiver<()>,
    state: Arc<Mutex<CleanupState>>,
    retry_min: Duration,
) {
    let mut delay = Duration::from_secs(30);
    let mut retry_delay = retry_min;
    loop {
        let closing = tokio::select! {
            event = receiver.recv() => event.is_none(),
            _ = tokio::time::sleep(delay) => false,
        };
        // Same order as foreground mutations; never purge a staged deletion
        // between its rename and the foreground directory synchronization.
        let Ok(permit) = io.acquire().await else {
            tracing::warn!("local cleanup stopped with storage I/O gate closed; remaining staged deletions require startup recovery");
            break;
        };
        let mutation_guard = mutation.lock().await;
        let removal_paths = paths.clone();
        let result = cleanup_pass(&paths, move |path, budget| {
            let paths = removal_paths.clone();
            async move { paths.remove_bounded(&path, budget).await }
        })
        .await;
        drop(mutation_guard);
        drop(permit);
        let status = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .apply_pass(result);
        let failed = status.scan_failed || status.pending_deletions != 0;
        if failed {
            tracing::warn!(
                pending_deletions = status.pending_deletions,
                scan_failed = status.scan_failed,
                "local deletion cleanup incomplete; staged data retained for retry/restart"
            );
        }
        let more_pending = status.more_pending;
        // When the last service owner closes, perform one final pass then stop.
        // A failed final pass leaves owned disk entries for startup recovery.
        if closing {
            if more_pending {
                tracing::warn!(
                    "local cleanup closed with staged deletions remaining for startup recovery"
                );
            }
            break;
        }
        if failed {
            delay = retry_delay;
            retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
            // During backoff coalesce notifications too: repeated deletions
            // must not turn a persistent I/O failure into a tight retry loop.
            if !receiver.is_closed() {
                tokio::time::sleep(delay).await;
            }
            delay = Duration::ZERO;
        } else {
            delay = if more_pending {
                Duration::from_millis(10)
            } else {
                Duration::from_secs(30)
            };
            retry_delay = retry_min;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        error::AppError,
        storage_transaction::{remove_any_bounded, TransactionId},
        test_support::TestDirectory,
    };

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cleanup made progress");
    }

    fn detached_worker(status: CleanupStatus) -> CleanupWorker {
        let (wake, _receiver) = mpsc::channel(1);
        CleanupWorker {
            wake,
            state: Arc::new(Mutex::new(CleanupState {
                status,
                debt: HashMap::new(),
            })),
        }
    }

    #[test]
    fn repeated_deletion_records_replace_bytes_without_double_counting() {
        let worker = detached_worker(CleanupStatus {
            pending_bytes_complete: true,
            ..CleanupStatus::default()
        });
        let first = PathBuf::from("first");
        let second = PathBuf::from("second");
        worker.record_deletion(first.clone(), 5);
        worker.record_deletion(second.clone(), 7);
        assert_eq!(worker.status().pending_bytes_upper_bound, 12);
        worker.record_deletion(first, 3);
        assert_eq!(worker.status().pending_bytes_upper_bound, 10);
        worker.record_deletion(second, 0);
        assert_eq!(worker.status().pending_bytes_upper_bound, 3);
        assert!(worker.status().pending_bytes_complete);
    }

    #[test]
    fn deletion_totals_recover_from_saturation_and_are_shared_by_clones() {
        let worker = detached_worker(CleanupStatus::default());
        let clone = worker.clone();
        worker.record_deletion(PathBuf::from("first"), u64::MAX);
        clone.record_deletion(PathBuf::from("second"), 7);
        assert_eq!(worker.status().pending_bytes_upper_bound, u64::MAX);
        worker.record_deletion(PathBuf::from("first"), 2);
        assert_eq!(clone.status().pending_bytes_upper_bound, 9);
        assert_eq!(worker.status(), clone.status());
    }

    #[test]
    fn recording_deletion_does_not_clear_cleanup_failure_metadata() {
        let initial = CleanupStatus {
            pending_deletions: 2,
            more_pending: true,
            failed_passes: 7,
            scan_failed: true,
            pending_bytes_complete: false,
            ..CleanupStatus::default()
        };
        let worker = detached_worker(initial);
        worker.record_deletion(PathBuf::from("new"), 11);
        assert_eq!(
            worker.status(),
            CleanupStatus {
                pending_bytes_upper_bound: 11,
                ..initial
            }
        );
    }

    fn empty_pass() -> CleanupPass {
        CleanupPass {
            failed: Vec::new(),
            partial: Vec::new(),
            completed: Vec::new(),
            tracked: Vec::new(),
            more: false,
        }
    }

    #[test]
    fn empty_initial_inventory_distinguishes_settled_from_truncated() {
        let settled = CleanupState::from_inventory(&[], false).status;
        assert_eq!(settled.pending_bytes_upper_bound, 0);
        assert!(settled.pending_bytes_complete);
        assert!(!settled.more_pending);
        let truncated = CleanupState::from_inventory(&[], true).status;
        assert!(!truncated.pending_bytes_complete);
        assert!(truncated.more_pending);
    }

    #[tokio::test]
    async fn startup_backlog_is_accounted_before_worker_runs_and_drains_online() {
        let directory = TestDirectory::new("cleanup-restart-backlog");
        let paths = TransactionPaths::initialize(directory.path())
            .await
            .unwrap();
        let formal = directory.path().join("keep.txt");
        tokio::fs::write(&formal, b"formal data").await.unwrap();
        let source = directory.path().join("deleted-tree");
        let copy = paths.copy_path(&TransactionId::new());
        for tree in [&source, &copy] {
            tokio::fs::create_dir(tree).await.unwrap();
            for index in 0..DELETION_NODES_PER_PASS + 2 {
                tokio::fs::write(tree.join(format!("{index}.txt")), b"x")
                    .await
                    .unwrap();
            }
        }
        let bytes = (DELETION_NODES_PER_PASS + 2) as u64;
        paths.stage_delete(&source, bytes, &mut ()).await.unwrap();
        drop(paths);

        let paths = Arc::new(
            TransactionPaths::initialize(directory.path())
                .await
                .unwrap(),
        );
        assert!(
            !copy.exists(),
            "abandoned copy must join the owned trash namespace"
        );
        let io = Arc::new(Semaphore::new(1));
        let permit = io.acquire().await.unwrap();
        let worker = CleanupWorker::start(paths.clone(), io.clone(), Arc::new(AsyncMutex::new(())))
            .await
            .unwrap();
        let initial = worker.status();
        assert_eq!(initial.pending_bytes_upper_bound, bytes);
        assert!(
            !initial.pending_bytes_complete,
            "copy bytes have no durable upper bound"
        );
        assert!(initial.more_pending);
        assert_eq!(initial.failed_passes, 0);
        drop(permit);
        // Startup itself wakes the worker; no request or timer is needed.
        wait_until(|| {
            let status = worker.status();
            !status.more_pending
                && status.pending_bytes_complete
                && status.pending_bytes_upper_bound == 0
        })
        .await;
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
        assert_eq!(tokio::fs::read(&formal).await.unwrap(), b"formal data");
        drop(worker);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
    }

    #[test]
    fn pass_settlement_keeps_failed_partial_and_newly_recorded_debt() {
        let mut state = CleanupState::default();
        let failed = PathBuf::from("failed");
        let partial = PathBuf::from("partial");
        let completed = PathBuf::from("completed");
        let late = PathBuf::from("late");
        state.record_deletion(failed.clone(), 100);
        state.record_deletion(completed.clone(), 30);
        state.record_deletion(late.clone(), 5);
        let status = state.apply_pass(Ok(CleanupPass {
            failed: vec![failed.clone()],
            partial: vec![partial.clone()],
            completed: vec![completed.clone()],
            tracked: vec![
                (failed.clone(), 10),
                (partial.clone(), 20),
                (completed.clone(), 30),
            ],
            more: true,
        }));
        assert_eq!(status.pending_bytes_upper_bound, 125);
        assert_eq!(status.pending_deletions, 1);
        assert_eq!(status.failed_passes, 1);
        assert!(status.more_pending);
        assert!(!status.scan_failed);
        assert!(!status.pending_bytes_complete);
        assert_eq!(state.debt[&failed], 100);
        assert_eq!(state.debt[&partial], 20);
        assert_eq!(state.debt[&late], 5);
        assert!(!state.debt.contains_key(&completed));
    }

    #[test]
    fn concurrent_recording_and_settlement_publish_one_consistent_total() {
        for _ in 0..32 {
            let worker = detached_worker(CleanupStatus {
                pending_bytes_complete: true,
                ..CleanupStatus::default()
            });
            let completed = PathBuf::from("completed");
            let late = PathBuf::from("late");
            worker.record_deletion(completed.clone(), 10);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|scope| {
                let recorder = worker.clone();
                let recording_barrier = barrier.clone();
                let new_path = late.clone();
                scope.spawn(move || {
                    recording_barrier.wait();
                    recorder.record_deletion(new_path, 20);
                });
                barrier.wait();
                worker.state.lock().unwrap().apply_pass(Ok(CleanupPass {
                    completed: vec![completed.clone()],
                    tracked: vec![(completed.clone(), 10)],
                    ..empty_pass()
                }));
            });
            assert_eq!(worker.status().pending_bytes_upper_bound, 20);
            let state = worker.state.lock().unwrap();
            assert_eq!(state.debt.len(), 1);
            assert_eq!(state.debt[&late], 20);
            assert!(!state.debt.contains_key(&completed));
        }
    }

    #[test]
    fn scan_failure_retains_debt_and_last_batch_until_a_successful_pass() {
        let mut state = CleanupState::default();
        let failed = PathBuf::from("failed");
        let late = PathBuf::from("late");
        state.record_deletion(failed.clone(), 8);
        state.apply_pass(Ok(CleanupPass {
            failed: vec![failed.clone()],
            more: true,
            ..empty_pass()
        }));
        state.record_deletion(late.clone(), 7);
        let status = state.apply_pass(Err(AppError::internal("injected scan failure")));
        assert_eq!(status.pending_bytes_upper_bound, 15);
        assert_eq!(status.pending_deletions, 1);
        assert_eq!(status.failed_passes, 2);
        assert!(status.more_pending);
        assert!(status.scan_failed);
        assert!(!status.pending_bytes_complete);
        assert_eq!(state.debt.len(), 2);
        let recovered = state.apply_pass(Ok(CleanupPass {
            completed: vec![failed, late],
            ..empty_pass()
        }));
        assert_eq!(
            recovered,
            CleanupStatus {
                failed_passes: 2,
                pending_bytes_complete: true,
                ..CleanupStatus::default()
            }
        );
        assert!(state.debt.is_empty());
    }

    #[test]
    fn debt_completeness_requires_full_inventory_and_known_pending_bytes() {
        for more in [false, true] {
            for tracked in [false, true] {
                for failed in [false, true] {
                    let mut state = CleanupState::default();
                    let path = PathBuf::from("pending");
                    let status = state.apply_pass(Ok(CleanupPass {
                        failed: if failed {
                            vec![path.clone()]
                        } else {
                            Vec::new()
                        },
                        partial: if failed {
                            Vec::new()
                        } else {
                            vec![path.clone()]
                        },
                        tracked: if tracked { vec![(path, 9)] } else { Vec::new() },
                        // A partial tree always requires another pass.
                        more: more || !failed,
                        ..empty_pass()
                    }));
                    assert_eq!(
                        status.pending_bytes_upper_bound,
                        if tracked { 9 } else { 0 }
                    );
                    assert_eq!(status.pending_bytes_complete, tracked && !more && failed);
                    assert_eq!(status.pending_deletions, usize::from(failed));
                    assert_eq!(status.failed_passes, u64::from(failed));
                    assert!(!status.scan_failed);
                }
            }
        }
    }

    #[test]
    fn completed_debt_recomputes_saturated_bytes_without_counter_overflow() {
        let mut state = CleanupState::default();
        let completed = PathBuf::from("large");
        let pending = PathBuf::from("small");
        state.record_deletion(completed.clone(), u64::MAX);
        state.record_deletion(pending.clone(), 7);
        assert_eq!(state.status.pending_bytes_upper_bound, u64::MAX);
        let status = state.apply_pass(Ok(CleanupPass {
            failed: vec![pending],
            completed: vec![completed],
            ..empty_pass()
        }));
        assert_eq!(status.pending_bytes_upper_bound, 7);
        assert!(status.pending_bytes_complete);
        assert_eq!(status.failed_passes, 1);
        state.status.failed_passes = u64::MAX;
        let status = state.apply_pass(Err(AppError::internal("injected scan failure")));
        assert_eq!(status.failed_passes, u64::MAX);
    }

    #[tokio::test]
    async fn failed_removal_is_retained_then_retried_idempotently() {
        let directory = TestDirectory::new("cleanup-retry");
        let paths = TransactionPaths::initialize(directory.path())
            .await
            .unwrap();
        let trash = paths.trash.join(TransactionId::new());
        let upload = paths.upload_path(&TransactionId::new());
        let copy = paths.copy_path(&TransactionId::new());
        for path in [&trash, &upload, &copy] {
            tokio::fs::write(path, b"ordinary data").await.unwrap();
        }
        let pending = cleanup_pass(&paths, |_, _| async {
            Err(AppError::internal("injected I/O failure"))
        })
        .await
        .unwrap();
        assert_eq!(pending.summary(), (1, false));
        assert_eq!(tokio::fs::read(&trash).await.unwrap(), b"ordinary data");
        assert_eq!(
            cleanup_pass(&paths, |path, budget| async move {
                remove_any_bounded(&path, budget).await
            })
            .await
            .unwrap()
            .summary(),
            (0, false)
        );
        assert_eq!(
            cleanup_pass(&paths, |path, budget| async move {
                remove_any_bounded(&path, budget).await
            })
            .await
            .unwrap()
            .summary(),
            (0, false)
        );
        assert!(upload.exists());
        assert!(copy.exists());
    }

    #[tokio::test]
    async fn worker_retries_a_scan_failure_without_another_notification() {
        let directory = TestDirectory::new("cleanup-worker");
        let paths = Arc::new(
            TransactionPaths::initialize(directory.path())
                .await
                .unwrap(),
        );
        let trash = paths.trash.join(TransactionId::new());
        tokio::fs::write(&trash, b"staged data").await.unwrap();
        // Ordinary temporary I/O unavailability, in an isolated fixture only.
        let held = directory.path().join("temporarily-unavailable-trash");
        tokio::fs::rename(&paths.trash, &held).await.unwrap();
        let worker = CleanupWorker::start_with_state(
            paths.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(AsyncMutex::new(())),
            Duration::from_millis(20),
            CleanupState::default(),
        );
        worker.notify();
        wait_until(|| worker.status().scan_failed).await;
        assert!(worker.status().failed_passes > 0);
        tokio::fs::rename(&held, &paths.trash).await.unwrap();
        wait_until(|| !trash.exists() && !worker.status().scan_failed).await;
        assert_eq!(worker.status().pending_deletions, 0);
        drop(worker);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
    }

    #[tokio::test]
    async fn recorded_cleanup_bytes_remain_until_physical_removal_is_confirmed() {
        let directory = TestDirectory::new("cleanup-debt");
        let paths = Arc::new(
            TransactionPaths::initialize(directory.path())
                .await
                .unwrap(),
        );
        let trash = paths.trash.join(TransactionId::new());
        tokio::fs::write(&trash, b"eight888").await.unwrap();
        let worker = CleanupWorker::start_with_state(
            paths.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(AsyncMutex::new(())),
            Duration::from_millis(20),
            CleanupState::default(),
        );
        worker.record_deletion(trash.clone(), 8);
        let held = directory.path().join("temporarily-unavailable-trash");
        tokio::fs::rename(&paths.trash, &held).await.unwrap();
        worker.notify();
        wait_until(|| worker.status().scan_failed).await;
        assert_eq!(worker.status().pending_bytes_upper_bound, 8);
        assert!(!worker.status().pending_bytes_complete);

        tokio::fs::rename(&held, &paths.trash).await.unwrap();
        wait_until(|| {
            let status = worker.status();
            !trash.exists()
                && status.pending_bytes_upper_bound == 0
                && status.pending_bytes_complete
        })
        .await;
        drop(worker);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
    }

    #[tokio::test]
    async fn worker_waits_for_mutation_and_coalesces_wakeups() {
        let directory = TestDirectory::new("cleanup-mutation");
        let paths = Arc::new(
            TransactionPaths::initialize(directory.path())
                .await
                .unwrap(),
        );
        let trash = paths.trash.join(TransactionId::new());
        tokio::fs::write(&trash, b"committing").await.unwrap();
        let mutation = Arc::new(AsyncMutex::new(()));
        let guard = mutation.lock().await;
        let worker =
            CleanupWorker::start(paths.clone(), Arc::new(Semaphore::new(1)), mutation.clone())
                .await
                .unwrap();
        for _ in 0..100 {
            worker.notify();
        }
        assert_eq!(worker.wake.capacity(), 0);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(trash.exists());
        drop(guard);
        wait_until(|| !trash.exists()).await;
        drop(worker);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
    }

    #[tokio::test]
    async fn last_owner_close_runs_a_final_pass_and_stops_the_worker() {
        let directory = TestDirectory::new("cleanup-close");
        let paths = Arc::new(
            TransactionPaths::initialize(directory.path())
                .await
                .unwrap(),
        );
        let trash = paths.trash.join(TransactionId::new());
        tokio::fs::write(&trash, b"remaining").await.unwrap();
        let worker = CleanupWorker::start(
            paths.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(AsyncMutex::new(())),
        )
        .await
        .unwrap();
        drop(worker);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
        assert!(!trash.exists());
    }

    #[tokio::test]
    async fn online_cleanup_batches_a_backlog_without_losing_progress() {
        let directory = TestDirectory::new("cleanup-pages");
        let paths = TransactionPaths::initialize(directory.path())
            .await
            .unwrap();
        for _ in 0..129 {
            tokio::fs::write(paths.trash.join(TransactionId::new()), b"item")
                .await
                .unwrap();
        }
        let (candidates, more) = paths.staged_deletions().await.unwrap();
        let initial = CleanupState::from_inventory(&candidates, more).status;
        assert!(initial.more_pending);
        assert!(!initial.pending_bytes_complete);
        assert_eq!(
            cleanup_pass(&paths, |path, budget| async move {
                remove_any_bounded(&path, budget).await
            })
            .await
            .unwrap()
            .summary(),
            (0, true)
        );
        assert_eq!(paths.staged_deletions().await.unwrap().0.len(), 1);
        assert_eq!(
            cleanup_pass(&paths, |path, budget| async move {
                remove_any_bounded(&path, budget).await
            })
            .await
            .unwrap()
            .summary(),
            (0, false)
        );
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
    }

    #[tokio::test]
    async fn one_large_directory_is_reclaimed_across_bounded_passes() {
        let directory = TestDirectory::new("cleanup-large-tree");
        let paths = TransactionPaths::initialize(directory.path())
            .await
            .unwrap();
        let source = directory.path().join("large-tree");
        tokio::fs::create_dir(&source).await.unwrap();
        for index in 0..257 {
            tokio::fs::write(source.join(format!("{index}.bin")), b"x")
                .await
                .unwrap();
        }
        let trash = paths.stage_delete(&source, 257, &mut ()).await.unwrap();
        let (candidates, more) = paths.staged_deletions().await.unwrap();
        let initial = CleanupState::from_inventory(&candidates, more).status;
        assert!(initial.more_pending);
        assert!(initial.pending_bytes_complete);
        assert_eq!(initial.pending_bytes_upper_bound, 257);

        let first = cleanup_pass(&paths, |path, budget| async move {
            remove_any_bounded(&path, budget).await
        })
        .await
        .unwrap();
        assert_eq!(first.summary(), (0, true));
        assert!(trash.exists());
        let staged = paths.staged_deletions().await.unwrap().0;
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].bytes_upper_bound, Some(257));

        let second = cleanup_pass(&paths, |path, budget| async move {
            remove_any_bounded(&path, budget).await
        })
        .await
        .unwrap();
        assert_eq!(second.summary(), (0, false));
        assert!(!trash.exists());
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
    }

    #[tokio::test]
    async fn storage_remove_notifies_cleanup_without_changing_user_byte_count() {
        let directory = TestDirectory::new("cleanup-service");
        let storage =
            super::super::StorageService::new(directory.path().to_path_buf(), 1024, 1, 100, 0)
                .await
                .unwrap();
        tokio::fs::write(directory.path().join("notes.txt"), b"notes")
            .await
            .unwrap();
        let source = storage.resolve_existing("notes.txt").await.unwrap();
        assert_eq!(storage.remove(&source).await.unwrap(), 5);
        assert!(!source.absolute().exists());
        let paths = storage.transactions.clone();
        wait_until(|| std::fs::read_dir(&paths.trash).unwrap().next().is_none()).await;
        assert_eq!(storage.user_data_size().await.unwrap(), 0);
        drop(storage);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
    }

    #[tokio::test]
    async fn retained_cleanup_work_is_recovered_after_reinitialization() {
        let directory = TestDirectory::new("cleanup-restart");
        let paths = TransactionPaths::initialize(directory.path())
            .await
            .unwrap();
        let trash = paths.trash.join(TransactionId::new());
        tokio::fs::write(&trash, b"staged data").await.unwrap();
        tokio::fs::write(directory.path().join("keep.txt"), b"user data")
            .await
            .unwrap();
        assert_eq!(
            cleanup_pass(&paths, |_, _| async {
                Err(AppError::internal("injected I/O failure"))
            })
            .await
            .unwrap()
            .summary(),
            (1, false)
        );
        drop(paths);
        let paths = TransactionPaths::initialize(directory.path())
            .await
            .unwrap();
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
        assert_eq!(
            tokio::fs::read(directory.path().join("keep.txt"))
                .await
                .unwrap(),
            b"user data"
        );
    }
}
