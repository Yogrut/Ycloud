use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Mutex, RwLock,
    },
    time::Duration,
};

use tokio::sync::Notify;

use super::S3RecoveryStatus;

const MAX_RECORDED_ERROR_CHARS: usize = 512;

pub(super) struct RecoveryRuntime {
    work: Notify,
    shutdown: Notify,
    stopped: AtomicBool,
    worker_started: AtomicBool,
    pending: Mutex<HashMap<String, i64>>,
    recovering: AtomicBool,
    consecutive_failures: AtomicU64,
    last_failure_unix: AtomicI64,
    next_retry_unix: AtomicI64,
    last_failure: RwLock<Option<String>>,
}

impl RecoveryRuntime {
    pub(super) fn new() -> Self {
        Self {
            work: Notify::new(),
            shutdown: Notify::new(),
            stopped: AtomicBool::new(false),
            worker_started: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            recovering: AtomicBool::new(false),
            consecutive_failures: AtomicU64::new(0),
            last_failure_unix: AtomicI64::new(0),
            next_retry_unix: AtomicI64::new(0),
            last_failure: RwLock::new(None),
        }
    }

    pub(super) fn journal_write_started(&self, key: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key.to_owned())
            .or_insert_with(|| chrono::Utc::now().timestamp());
        self.work.notify_one();
    }

    pub(super) fn journal_settled(&self, key: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }

    pub(super) fn has_pending(&self) -> bool {
        !self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    pub(super) fn begin_worker(&self) -> bool {
        self.worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn stop_worker(&self) {
        self.stopped.store(true, Ordering::Release);
        // There is exactly one claimed worker. notify_one stores a permit if
        // the task has not reached its select yet, so shutdown cannot be lost.
        self.shutdown.notify_one();
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(super) async fn wait_for_work(&self) {
        self.work.notified().await;
    }

    pub(super) async fn wait_for_shutdown(&self) {
        self.shutdown.notified().await;
    }

    pub(super) fn recovery_started(&self) {
        self.recovering.store(true, Ordering::Release);
        self.next_retry_unix.store(0, Ordering::Release);
    }

    pub(super) fn recovery_succeeded(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.recovering.store(false, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
        self.next_retry_unix.store(0, Ordering::Release);
    }

    pub(super) fn recovery_failed(&self, message: &str, retry_after: Duration) {
        let now = chrono::Utc::now().timestamp();
        let retry_seconds = i64::try_from(retry_after.as_secs()).unwrap_or(i64::MAX);
        let message = message.chars().take(MAX_RECORDED_ERROR_CHARS).collect();
        *self
            .last_failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message);
        self.last_failure_unix.store(now, Ordering::Release);
        self.next_retry_unix
            .store(now.saturating_add(retry_seconds), Ordering::Release);
        self.consecutive_failures.fetch_add(1, Ordering::AcqRel);
        self.recovering.store(false, Ordering::Release);
    }

    pub(super) fn status(&self, orphan_uploads: usize, orphan_backups: usize) -> S3RecoveryStatus {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = chrono::Utc::now().timestamp();
        let oldest_pending_age_seconds = pending
            .values()
            .min()
            .map(|created| now.saturating_sub(*created).max(0) as u64);
        let last_failure_unix = nonzero(self.last_failure_unix.load(Ordering::Acquire));
        let next_retry_unix = nonzero(self.next_retry_unix.load(Ordering::Acquire));
        let last_failure = self
            .last_failure
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        S3RecoveryStatus {
            orphan_uploads,
            orphan_backups,
            pending_records: pending.len(),
            oldest_pending_age_seconds,
            consecutive_failures: self.consecutive_failures.load(Ordering::Acquire),
            last_failure,
            last_failure_unix,
            next_retry_unix,
            recovering: self.recovering.load(Ordering::Acquire),
        }
    }
}

fn nonzero(value: i64) -> Option<i64> {
    (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::RecoveryRuntime;

    #[test]
    fn runtime_tracks_owned_journals_and_retry_state() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("journal-a");
        runtime.journal_write_started("journal-a");
        runtime.journal_write_started("journal-b");
        runtime.recovery_started();
        let active = runtime.status(2, 1);
        assert_eq!(active.pending_records, 2);
        assert!(active.recovering);
        assert_eq!(active.orphan_uploads, 2);
        assert_eq!(active.orphan_backups, 1);

        runtime.recovery_failed(&"x".repeat(600), Duration::from_secs(4));
        let failed = runtime.status(2, 1);
        assert_eq!(failed.consecutive_failures, 1);
        assert_eq!(failed.last_failure.as_deref().map(str::len), Some(512));
        assert!(failed.next_retry_unix.is_some());
        assert!(!failed.recovering);

        runtime.journal_settled("journal-a");
        assert_eq!(runtime.status(0, 0).pending_records, 1);
        runtime.recovery_succeeded();
        let recovered = runtime.status(0, 0);
        assert_eq!(recovered.pending_records, 0);
        assert_eq!(recovered.consecutive_failures, 0);
        assert!(recovered.next_retry_unix.is_none());
        assert!(recovered.last_failure.is_some());
    }

    #[tokio::test]
    async fn shutdown_before_wait_stores_a_permit_and_only_one_worker_is_claimed() {
        let runtime = RecoveryRuntime::new();
        assert!(runtime.begin_worker());
        assert!(!runtime.begin_worker());
        runtime.stop_worker();
        tokio::time::timeout(Duration::from_millis(100), runtime.wait_for_shutdown())
            .await
            .expect("shutdown notification was lost before the worker waited");
        assert!(runtime.is_stopped());
    }
}
