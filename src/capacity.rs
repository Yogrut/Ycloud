use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::{AppError, AppResult};

#[derive(Clone, Debug)]
pub struct CapacityTracker {
    state: Arc<Mutex<CapacityState>>,
    ledger_path: Option<Arc<PathBuf>>,
    persist_gate: Arc<tokio::sync::Mutex<()>>,
    reconcile_wake: Arc<Mutex<Option<Weak<tokio::sync::Notify>>>>,
}

#[derive(Debug)]
struct CapacityState {
    limit: Option<u64>,
    used: u64,
    reserved: u64,
    accurate: bool,
    reconciling: bool,
}

#[derive(Debug)]
pub struct CapacityReservation {
    tracker: CapacityTracker,
    old_size: u64,
    reserved: u64,
    completed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityStatus {
    pub limit: Option<u64>,
    pub used: u64,
    pub reserved: u64,
    pub accurate: bool,
    pub reconciling: bool,
}

const CAPACITY_LEDGER_VERSION: u8 = 1;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapacityLedger {
    version: u8,
    used: u64,
}

impl CapacityTracker {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, CapacityState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn new(limit: Option<u64>, used: u64) -> Self {
        Self::new_with_ledger(limit, used, true, None)
    }

    pub fn new_with_ledger(
        limit: Option<u64>,
        used: u64,
        accurate: bool,
        ledger_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(CapacityState {
                limit,
                used,
                reserved: 0,
                accurate,
                reconciling: false,
            })),
            ledger_path: ledger_path.map(Arc::new),
            persist_gate: Arc::new(tokio::sync::Mutex::new(())),
            reconcile_wake: Arc::new(Mutex::new(None)),
        }
    }

    pub fn reserve_replacement(
        &self,
        old_size: u64,
        expected_new_size: u64,
    ) -> AppResult<CapacityReservation> {
        let mut reservation = CapacityReservation {
            tracker: self.clone(),
            old_size,
            reserved: 0,
            completed: false,
        };
        reservation.ensure_new_size(expected_new_size)?;
        Ok(reservation)
    }

    pub fn remove_used(&self, bytes: u64) {
        let mut state = self.lock_state();
        state.used = state.used.saturating_sub(bytes);
    }

    pub fn reconcile(&self, used: u64) {
        let mut state = self.lock_state();
        state.used = used;
        state.accurate = true;
        state.reconciling = false;
    }

    pub fn mark_uncertain(&self) {
        self.lock_state().accurate = false;
        let wake = self
            .reconcile_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(wake) = wake {
            wake.notify_one();
        }
    }

    pub(crate) fn set_reconcile_wake(&self, wake: &Arc<tokio::sync::Notify>) {
        *self
            .reconcile_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(wake));
    }

    pub fn begin_reconciliation(&self) -> bool {
        let mut state = self.lock_state();
        if state.reconciling {
            return false;
        }
        state.reconciling = true;
        true
    }

    pub fn reconciliation_failed(&self) {
        let mut state = self.lock_state();
        state.accurate = false;
        state.reconciling = false;
    }

    pub fn status(&self) -> CapacityStatus {
        let state = self.lock_state();
        CapacityStatus {
            limit: state.limit,
            used: state.used,
            reserved: state.reserved,
            accurate: state.accurate,
            reconciling: state.reconciling,
        }
    }

    pub async fn persist(&self) -> AppResult<()> {
        let Some(path) = self.ledger_path.as_deref() else {
            return Ok(());
        };
        // Serialize writers and read the latest state only after acquiring the
        // gate. Concurrent mutations can finish in either order, but an older
        // snapshot must never overwrite a newer capacity value.
        let _persist = self.persist_gate.lock().await;
        let status = self.status();
        if !status.accurate {
            return Ok(());
        }
        save_capacity_ledger(path, status.used).await
    }

    /// Persist a freshly scanned value before advertising it as accurate.
    /// The caller must hold the storage mutation boundary until this returns,
    /// so a local publication cannot change `used` between the scan and ledger
    /// commit.
    pub(crate) async fn persist_reconciled(&self, used: u64) -> AppResult<()> {
        let _persist = self.persist_gate.lock().await;
        if let Some(path) = self.ledger_path.as_deref() {
            save_capacity_ledger(path, used).await?;
        }
        self.reconcile(used);
        Ok(())
    }
}

impl CapacityReservation {
    pub(crate) fn tracker(&self) -> CapacityTracker {
        self.tracker.clone()
    }

    /// Recheck growth against the file actually replaced under the mutation lock.
    pub(crate) fn rebase_replacement(
        &mut self,
        previous_size: u64,
        new_size: u64,
    ) -> AppResult<()> {
        self.old_size = previous_size;
        self.ensure_new_size(new_size)
    }

    pub fn ensure_new_size(&mut self, new_size: u64) -> AppResult<()> {
        let desired = new_size.saturating_sub(self.old_size);
        if desired <= self.reserved {
            return Ok(());
        }
        let additional = desired - self.reserved;
        let mut state = self
            .tracker
            .state
            .lock()
            .map_err(|_| AppError::internal("storage capacity state is unavailable"))?;
        if state.limit.is_some() && !state.accurate {
            return Err(AppError::ServiceUnavailable(
                "存储容量状态需要核对，写入暂时不可用".into(),
            ));
        }
        if state.limit.is_some_and(|limit| {
            state
                .used
                .saturating_add(state.reserved)
                .saturating_add(additional)
                > limit
        }) {
            return Err(AppError::InsufficientStorage);
        }
        state.reserved = state.reserved.saturating_add(additional);
        self.reserved = self.reserved.saturating_add(additional);
        Ok(())
    }

    pub fn commit(mut self, previous_size: u64, new_size: u64) {
        let mut state = self.tracker.lock_state();
        state.reserved = state.reserved.saturating_sub(self.reserved);
        state.used = state
            .used
            .saturating_sub(previous_size)
            .saturating_add(new_size);
        drop(state);
        self.reserved = 0;
        self.completed = true;
    }
}

pub async fn load_capacity_ledger(path: &Path) -> AppResult<Option<u64>> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::with_source(
                "failed to read capacity ledger",
                error,
            ))
        }
    };
    let ledger: CapacityLedger = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::with_source("invalid capacity ledger", error))?;
    if ledger.version != CAPACITY_LEDGER_VERSION {
        return Err(AppError::ServiceUnavailable(
            "存储容量记录版本不受支持".into(),
        ));
    }
    Ok(Some(ledger.used))
}

async fn save_capacity_ledger(path: &Path, used: u64) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal("capacity ledger has no parent directory"))?;
    tokio::fs::create_dir_all(parent).await.map_err(|error| {
        AppError::with_source("failed to create capacity ledger directory", error)
    })?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(&CapacityLedger {
        version: CAPACITY_LEDGER_VERSION,
        used,
    })
    .map_err(|error| AppError::with_source("failed to encode capacity ledger", error))?;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(|error| AppError::with_source("failed to create capacity ledger", error))?;
    if let Err(error) = async {
        file.write_all(&bytes).await?;
        file.sync_all().await
    }
    .await
    {
        drop(file);
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(AppError::with_source(
            "failed to flush capacity ledger",
            error,
        ));
    }
    drop(file);
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        if error.kind() == std::io::ErrorKind::AlreadyExists
            || tokio::fs::try_exists(path).await.unwrap_or(false)
        {
            tokio::fs::remove_file(path).await.map_err(|remove| {
                AppError::with_source("failed to replace capacity ledger", remove)
            })?;
            tokio::fs::rename(&temporary, path)
                .await
                .map_err(|rename| {
                    AppError::with_source("failed to commit capacity ledger", rename)
                })?;
        } else {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(AppError::with_source(
                "failed to commit capacity ledger",
                error,
            ));
        }
    }
    sync_parent_directory(parent).await
}

#[cfg(unix)]
async fn sync_parent_directory(path: &Path) -> AppResult<()> {
    let directory = tokio::fs::File::open(path).await.map_err(|error| {
        AppError::with_source("failed to open capacity ledger directory", error)
    })?;
    directory
        .sync_all()
        .await
        .map_err(|error| AppError::with_source("failed to flush capacity ledger directory", error))
}

#[cfg(not(unix))]
async fn sync_parent_directory(_path: &Path) -> AppResult<()> {
    Ok(())
}

impl Drop for CapacityReservation {
    fn drop(&mut self) {
        if self.completed || self.reserved == 0 {
            return;
        }
        let mut state = self.tracker.lock_state();
        state.reserved = state.reserved.saturating_sub(self.reserved);
    }
}

#[cfg(test)]
mod tests {
    use super::{load_capacity_ledger, CapacityTracker};

    #[test]
    fn replacement_reserves_only_growth_and_releases_on_drop() {
        let tracker = CapacityTracker::new(Some(100), 80);
        let reservation = tracker.reserve_replacement(30, 45).unwrap();
        assert_eq!(tracker.status().reserved, 15);
        drop(reservation);
        assert_eq!(tracker.status().reserved, 0);
        assert_eq!(tracker.status().used, 80);
    }

    #[test]
    fn concurrent_reservations_cannot_exceed_limit() {
        let tracker = CapacityTracker::new(Some(100), 80);
        let first = tracker.reserve_replacement(0, 15).unwrap();
        assert!(tracker.reserve_replacement(0, 6).is_err());
        first.commit(0, 15);
        assert_eq!(tracker.status().used, 95);
    }

    #[test]
    fn smaller_replacement_releases_used_capacity() {
        let tracker = CapacityTracker::new(Some(100), 90);
        tracker.reserve_replacement(40, 10).unwrap().commit(40, 10);
        assert_eq!(tracker.status().used, 60);
    }

    #[test]
    fn commit_uses_the_size_seen_inside_the_mutation_lock() {
        let tracker = CapacityTracker::new(Some(200), 80);
        let reservation = tracker.reserve_replacement(30, 45).unwrap();
        reservation.commit(40, 45);
        assert_eq!(tracker.status().used, 85);
    }

    #[test]
    fn status_distinguishes_waiting_from_active_reconciliation() {
        let tracker = CapacityTracker::new(Some(100), 80);
        tracker.mark_uncertain();
        assert!(!tracker.status().accurate);
        assert!(!tracker.status().reconciling);
        assert!(tracker.begin_reconciliation());
        assert!(tracker.status().reconciling);
        tracker.reconciliation_failed();
        assert!(!tracker.status().reconciling);
    }

    #[tokio::test]
    async fn ledger_round_trip_keeps_latest_capacity() {
        let root =
            std::env::temp_dir().join(format!("ycloud-capacity-ledger-{}", uuid::Uuid::new_v4()));
        let path = root.join("capacity.json");
        let tracker = CapacityTracker::new_with_ledger(None, 10, true, Some(path.clone()));
        tracker.persist().await.unwrap();
        tracker.reserve_replacement(0, 7).unwrap().commit(0, 7);
        tracker.persist().await.unwrap();

        assert_eq!(load_capacity_ledger(&path).await.unwrap(), Some(17));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_ledger_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "ycloud-invalid-capacity-ledger-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("capacity.json");
        tokio::fs::write(&path, br#"{"version":2,"used":1}"#)
            .await
            .unwrap();

        assert!(load_capacity_ledger(&path).await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
