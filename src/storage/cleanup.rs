//! Online cleanup of committed staged deletions. Disk entries, not wakeup
//! messages, are the durable work list; startup recovery remains the fallback.
use crate::{
    error::AppResult,
    storage_transaction::{RemovalProgress, TransactionPaths, DELETION_NODES_PER_PASS},
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

#[derive(Default)]
struct CleanupDebt {
    entries: HashMap<PathBuf, u64>,
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
    status: Arc<Mutex<CleanupStatus>>,
    debt: Arc<Mutex<CleanupDebt>>,
}

impl CleanupWorker {
    pub(super) fn start(
        paths: Arc<TransactionPaths>,
        io: Arc<Semaphore>,
        mutation: Arc<AsyncMutex<()>>,
    ) -> Self {
        Self::start_with_delay(paths, io, mutation, Duration::from_secs(1))
    }

    fn start_with_delay(
        paths: Arc<TransactionPaths>,
        io: Arc<Semaphore>,
        mutation: Arc<AsyncMutex<()>>,
        retry_min: Duration,
    ) -> Self {
        // Coalesce wakeups. A large delete batch must not allocate one queued
        // message per file; every pass discovers work from its owned directory.
        let (wake, receiver) = mpsc::channel(1);
        let status = Arc::new(Mutex::new(CleanupStatus {
            pending_bytes_complete: true,
            ..CleanupStatus::default()
        }));
        let debt = Arc::new(Mutex::new(CleanupDebt::default()));
        tokio::spawn(run(
            paths,
            io,
            mutation,
            receiver,
            status.clone(),
            debt.clone(),
            retry_min,
        ));
        Self { wake, status, debt }
    }

    pub(super) fn notify(&self) {
        let _ = self.wake.try_send(());
    }

    pub(super) fn record_deletion(&self, path: PathBuf, bytes: u64) {
        let total = {
            let mut debt = self
                .debt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            debt.entries.insert(path, bytes);
            debt.entries
                .values()
                .fold(0_u64, |total, value| total.saturating_add(*value))
        };
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending_bytes_upper_bound = total;
    }

    pub(super) fn status(&self) -> CleanupStatus {
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    status: Arc<Mutex<CleanupStatus>>,
    debt: Arc<Mutex<CleanupDebt>>,
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
        let (failed, more_pending) = {
            let mut state = status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match result {
                Ok(pass) => {
                    let (tracked_bytes, all_pending_tracked) = {
                        let mut debt = debt.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        for (path, bytes) in &pass.tracked {
                            debt.entries.entry(path.clone()).or_insert(*bytes);
                        }
                        for path in &pass.completed {
                            debt.entries.remove(path);
                        }
                        let all_pending_tracked = pass
                            .failed
                            .iter()
                            .chain(&pass.partial)
                            .all(|path| debt.entries.contains_key(path));
                        let tracked_bytes = debt
                            .entries
                            .values()
                            .fold(0_u64, |total, value| total.saturating_add(*value));
                        (tracked_bytes, all_pending_tracked)
                    };
                    state.pending_deletions = pass.failed.len();
                    state.more_pending = pass.more;
                    state.scan_failed = false;
                    state.pending_bytes_upper_bound = tracked_bytes;
                    state.pending_bytes_complete = !pass.more && all_pending_tracked;
                }
                Err(_) => {
                    state.scan_failed = true;
                    state.pending_bytes_complete = false;
                }
            }
            let failed = state.scan_failed || state.pending_deletions != 0;
            if failed {
                state.failed_passes = state.failed_passes.saturating_add(1);
                tracing::warn!(
                    pending_deletions = state.pending_deletions,
                    scan_failed = state.scan_failed,
                    "local deletion cleanup incomplete; staged data retained for retry/restart"
                );
            }
            (failed, state.more_pending)
        };
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
        let worker = CleanupWorker::start_with_delay(
            paths.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(AsyncMutex::new(())),
            Duration::from_millis(20),
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
        let worker = CleanupWorker::start_with_delay(
            paths.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(AsyncMutex::new(())),
            Duration::from_millis(20),
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
            CleanupWorker::start(paths.clone(), Arc::new(Semaphore::new(1)), mutation.clone());
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
        );
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
