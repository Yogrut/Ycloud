use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

use tokio::sync::Notify;

use super::S3RecoveryStatus;

const MAX_RECORDED_ERROR_CHARS: usize = 512;

// Related status fields are published together, rather than assembled from
// independent atomic loads and an error-message lock during a transition.
#[derive(Default)]
struct RecoveryState {
    pending: HashMap<String, i64>,
    recovering: bool,
    consecutive_failures: u64,
    last_failure_unix: i64,
    next_retry_unix: i64,
    last_failure: Option<String>,
}

impl RecoveryState {
    fn status_at(
        &self,
        now: i64,
        orphan_uploads: usize,
        orphan_backups: usize,
    ) -> S3RecoveryStatus {
        let oldest_pending_age_seconds = self
            .pending
            .values()
            .min()
            .map(|created| now.saturating_sub(*created).max(0) as u64);
        S3RecoveryStatus {
            orphan_uploads,
            orphan_backups,
            pending_records: self.pending.len(),
            oldest_pending_age_seconds,
            consecutive_failures: self.consecutive_failures,
            last_failure: self.last_failure.clone(),
            last_failure_unix: nonzero(self.last_failure_unix),
            next_retry_unix: nonzero(self.next_retry_unix),
            recovering: self.recovering,
        }
    }
}

pub(super) struct RecoveryRuntime {
    work: Notify,
    shutdown: Notify,
    stopped: AtomicBool,
    worker_started: AtomicBool,
    state: Mutex<RecoveryState>,
}

impl RecoveryRuntime {
    pub(super) fn new() -> Self {
        Self {
            work: Notify::new(),
            shutdown: Notify::new(),
            stopped: AtomicBool::new(false),
            worker_started: AtomicBool::new(false),
            state: Mutex::new(RecoveryState::default()),
        }
    }

    pub(super) fn journal_write_started(&self, key: &str) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .entry(key.to_owned())
            .or_insert_with(|| chrono::Utc::now().timestamp());
        self.work.notify_one();
    }

    pub(super) fn journal_settled(&self, key: &str) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .remove(key);
    }

    pub(super) fn has_pending(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .is_empty()
    }

    pub(super) fn pending_keys(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .keys()
            .cloned()
            .collect()
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
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.recovering = true;
        state.next_retry_unix = 0;
    }

    pub(super) fn recovery_pass_succeeded(&self) {
        // Success ends this pass, not all recovery. Only confirmed journal
        // deletion/absence may retire its individual pending entry.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.recovering = false;
        state.consecutive_failures = 0;
        state.next_retry_unix = 0;
    }

    pub(super) fn recovery_failed(&self, message: &str, retry_after: Duration) {
        let now = chrono::Utc::now().timestamp();
        let retry_seconds = i64::try_from(retry_after.as_secs()).unwrap_or(i64::MAX);
        let message = message.chars().take(MAX_RECORDED_ERROR_CHARS).collect();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.last_failure = Some(message);
        state.last_failure_unix = now;
        state.next_retry_unix = now.saturating_add(retry_seconds);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.recovering = false;
    }

    pub(super) fn recovery_wait_stopped(&self) {
        // Retirement observed after acquiring the gate, before remote work.
        // Do not clear pending debt or failure history.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recovering = false;
    }

    pub(super) fn status(&self, orphan_uploads: usize, orphan_backups: usize) -> S3RecoveryStatus {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .status_at(
                chrono::Utc::now().timestamp(),
                orphan_uploads,
                orphan_backups,
            )
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
    fn status_projection_handles_backward_clock_and_extreme_ages() {
        let mut state = super::RecoveryState::default();
        state.pending.insert("older".into(), 100);
        state.pending.insert("newer".into(), 200);
        assert_eq!(
            state.status_at(90, 2, 3).oldest_pending_age_seconds,
            Some(0)
        );
        let status = state.status_at(300, 2, 3);
        assert_eq!(status.oldest_pending_age_seconds, Some(200));
        assert_eq!(status.pending_records, 2);
        assert_eq!(status.orphan_uploads, 2);
        assert_eq!(status.orphan_backups, 3);
        state.pending.remove("older");
        assert_eq!(
            state.status_at(300, 0, 0).oldest_pending_age_seconds,
            Some(100)
        );
        state.pending.clear();
        assert_eq!(state.status_at(300, 0, 0).oldest_pending_age_seconds, None);
        state.pending.insert("extreme".into(), i64::MIN);
        state.last_failure_unix = -1;
        state.next_retry_unix = 1;
        let extreme = state.status_at(i64::MAX, 0, 0);
        assert_eq!(extreme.oldest_pending_age_seconds, Some(i64::MAX as u64));
        assert_eq!(extreme.last_failure_unix, None);
        assert_eq!(extreme.next_retry_unix, Some(1));
    }

    #[test]
    fn failure_counter_saturates_without_erasing_pending_or_history() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("journal");
        runtime.state.lock().unwrap().consecutive_failures = u64::MAX;
        runtime.recovery_started();
        runtime.recovery_failed("still pending", Duration::from_secs(4));
        let failed = runtime.status(0, 0);
        assert_eq!(failed.consecutive_failures, u64::MAX);
        assert_eq!(failed.pending_records, 1);
        assert_eq!(failed.last_failure.as_deref(), Some("still pending"));
        assert!(failed.last_failure_unix.is_some());
        assert!(failed.next_retry_unix.is_some());
        assert!(!failed.recovering);
        runtime.recovery_pass_succeeded();
        assert_eq!(runtime.status(0, 0).consecutive_failures, 0);
    }

    #[test]
    fn concurrent_status_reads_keep_failure_details_and_retry_transition_together() {
        let runtime = std::sync::Arc::new(RecoveryRuntime::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let writer = runtime.clone();
            let writer_barrier = barrier.clone();
            scope.spawn(move || {
                writer_barrier.wait();
                for _ in 0..2_000 {
                    writer.recovery_failed("short", Duration::from_secs(4));
                    writer.recovery_started();
                    writer.recovery_failed("long", Duration::from_secs(8));
                    writer.recovery_started();
                }
            });
            barrier.wait();
            for _ in 0..10_000 {
                let status = runtime.status(0, 0);
                if status.recovering {
                    assert_eq!(status.next_retry_unix, None);
                } else if let Some(next) = status.next_retry_unix {
                    let delay = match status.last_failure.as_deref() {
                        Some("short") => 4,
                        Some("long") => 8,
                        other => panic!("retry without matching failure: {other:?}"),
                    };
                    assert_eq!(next, status.last_failure_unix.unwrap() + delay);
                    assert!(status.consecutive_failures > 0);
                }
            }
        });
        let status = runtime.status(0, 0);
        assert_eq!(status.consecutive_failures, 4_000);
        assert!(status.recovering);
        assert_eq!(status.next_retry_unix, None);
    }

    #[test]
    fn concurrent_worker_claims_remain_exclusive_and_stopping_preserves_pending_work() {
        let runtime = std::sync::Arc::new(RecoveryRuntime::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let claimed = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let claimant = runtime.clone();
                let claimant_barrier = barrier.clone();
                handles.push(scope.spawn(move || {
                    claimant_barrier.wait();
                    usize::from(claimant.begin_worker())
                }));
            }
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .sum::<usize>()
        });
        assert_eq!(claimed, 1);
        runtime.journal_write_started("journal");
        runtime.recovery_failed("retained", Duration::from_secs(4));
        let now = chrono::Utc::now().timestamp();
        let before = runtime.state.lock().unwrap().status_at(now, 1, 2);
        runtime.stop_worker();
        assert!(runtime.is_stopped());
        assert!(!runtime.begin_worker());
        assert!(runtime.has_pending());
        assert_eq!(runtime.state.lock().unwrap().status_at(now, 1, 2), before);
    }

    #[test]
    fn a_new_journal_after_success_keeps_history_but_starts_a_new_failure_count() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("old-journal");
        runtime.recovery_failed("old failure", Duration::from_secs(4));
        let previous = runtime.status(0, 0);
        runtime.journal_settled("old-journal");
        runtime.recovery_pass_succeeded();
        runtime.journal_write_started("new-journal");
        let fresh = runtime.status(0, 0);
        assert_eq!(fresh.pending_records, 1);
        assert_eq!(fresh.consecutive_failures, 0);
        assert!(!fresh.recovering);
        assert_eq!(fresh.next_retry_unix, None);
        assert_eq!(fresh.last_failure, previous.last_failure);
        runtime.recovery_failed("new failure", Duration::from_secs(8));
        let next = runtime.status(0, 0);
        assert_eq!(next.pending_records, 1);
        assert_eq!(next.consecutive_failures, 1);
        assert_eq!(next.last_failure.as_deref(), Some("new failure"));
        assert_eq!(previous.last_failure.as_deref(), Some("old failure"));
    }

    #[test]
    fn stopped_gate_wait_ends_recovering_without_erasing_debt_or_retry_history() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("journal");
        runtime.recovery_failed("retained failure", Duration::from_secs(4));
        runtime.recovery_started();
        let before = runtime.status(0, 0);
        runtime.stop_worker();
        runtime.recovery_wait_stopped();
        let after = runtime.status(0, 0);
        assert!(!after.recovering);
        assert_eq!(after.pending_records, before.pending_records);
        assert_eq!(after.consecutive_failures, before.consecutive_failures);
        assert_eq!(after.last_failure, before.last_failure);
        assert_eq!(after.last_failure_unix, before.last_failure_unix);
        assert_eq!(after.next_retry_unix, before.next_retry_unix);
    }

    #[test]
    fn partial_success_preserves_unprocessed_journals_and_snapshot_is_not_live_state() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("journal-a");
        let snapshot = runtime.pending_keys();
        runtime.journal_write_started("journal-b");
        runtime.recovery_started();
        runtime.journal_settled("journal-a");
        runtime.recovery_pass_succeeded();
        assert_eq!(snapshot, vec!["journal-a"]);
        assert_eq!(runtime.pending_keys(), vec!["journal-b"]);
        assert_eq!(runtime.status(0, 0).pending_records, 1);
        assert!(!runtime.status(0, 0).recovering);
    }

    #[test]
    fn repeated_journal_registration_preserves_original_age_and_settlement_is_idempotent() {
        let runtime = RecoveryRuntime::new();
        let created = chrono::Utc::now().timestamp() - 60;
        runtime
            .state
            .lock()
            .unwrap()
            .pending
            .insert("journal-a".into(), created);
        runtime.journal_write_started("journal-a");
        runtime.journal_write_started("journal-b");
        let active = runtime.status(0, 0);
        assert_eq!(active.pending_records, 2);
        assert!(active.oldest_pending_age_seconds.is_some());
        assert_eq!(
            runtime
                .state
                .lock()
                .unwrap()
                .status_at(created + 60, 0, 0)
                .oldest_pending_age_seconds,
            Some(60)
        );
        assert_eq!(runtime.state.lock().unwrap().pending["journal-a"], created);
        runtime.journal_settled("journal-a");
        runtime.journal_settled("journal-a");
        runtime.journal_settled("missing");
        assert_eq!(runtime.status(0, 0).pending_records, 1);
        assert!(runtime.has_pending());
        runtime.journal_settled("journal-b");
        assert!(!runtime.has_pending());
        assert!(runtime.status(0, 0).oldest_pending_age_seconds.is_none());
    }

    #[test]
    fn failure_history_uses_character_limit_and_survives_restart_of_recovery() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("journal");
        runtime.recovery_failed(&"故障🙂".repeat(200), Duration::MAX);
        let failed = runtime.status(0, 0);
        assert_eq!(failed.last_failure.as_ref().unwrap().chars().count(), 512);
        assert_eq!(failed.next_retry_unix, Some(i64::MAX));
        assert_eq!(failed.consecutive_failures, 1);
        assert_eq!(failed.pending_records, 1);
        assert!(failed.last_failure_unix.is_some());
        runtime.recovery_started();
        let retrying = runtime.status(0, 0);
        assert!(retrying.recovering);
        assert_eq!(retrying.next_retry_unix, None);
        assert_eq!(retrying.last_failure, failed.last_failure);
        assert_eq!(retrying.consecutive_failures, 1);
        runtime.journal_settled("journal");
        runtime.recovery_pass_succeeded();
        let succeeded = runtime.status(0, 0);
        assert!(!succeeded.recovering);
        assert_eq!(succeeded.pending_records, 0);
        assert_eq!(succeeded.consecutive_failures, 0);
        assert_eq!(succeeded.last_failure, failed.last_failure);
        assert_eq!(succeeded.last_failure_unix, failed.last_failure_unix);
        assert_eq!(succeeded.next_retry_unix, None);
    }

    #[tokio::test]
    async fn work_notifications_before_wait_are_retained_and_coalesced() {
        let runtime = RecoveryRuntime::new();
        runtime.journal_write_started("journal-a");
        runtime.journal_write_started("journal-b");
        tokio::time::timeout(Duration::from_millis(100), runtime.wait_for_work())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), runtime.wait_for_work())
                .await
                .is_err()
        );
        assert_eq!(runtime.status(0, 0).pending_records, 2);
        runtime.journal_write_started("journal-b");
        tokio::time::timeout(Duration::from_millis(100), runtime.wait_for_work())
            .await
            .unwrap();
        assert_eq!(runtime.status(0, 0).pending_records, 2);
    }

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
        runtime.recovery_pass_succeeded();
        let recovered = runtime.status(0, 0);
        assert_eq!(recovered.pending_records, 1);
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
