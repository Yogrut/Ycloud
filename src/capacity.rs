use std::sync::{Arc, Mutex};

use crate::error::{AppError, AppResult};

#[derive(Clone, Debug)]
pub struct CapacityTracker {
    state: Arc<Mutex<CapacityState>>,
}

#[derive(Debug)]
struct CapacityState {
    limit: Option<u64>,
    used: u64,
    reserved: u64,
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
}

impl CapacityTracker {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, CapacityState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn new(limit: Option<u64>, used: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(CapacityState {
                limit,
                used,
                reserved: 0,
            })),
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
        self.lock_state().used = used;
    }

    pub fn status(&self) -> CapacityStatus {
        let state = self.lock_state();
        CapacityStatus {
            limit: state.limit,
            used: state.used,
            reserved: state.reserved,
        }
    }

    pub fn set_limit(&self, limit: Option<u64>) -> AppResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AppError::internal("storage capacity state is unavailable"))?;
        if limit.is_some_and(|limit| state.used.saturating_add(state.reserved) > limit) {
            return Err(AppError::InsufficientStorage);
        }
        state.limit = limit;
        Ok(())
    }
}

impl CapacityReservation {
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
    use super::CapacityTracker;

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
    fn capacity_limit_cannot_be_lowered_below_committed_and_reserved_bytes() {
        let tracker = CapacityTracker::new(None, 80);
        let reservation = tracker.reserve_replacement(0, 15).unwrap();
        assert!(tracker.set_limit(Some(94)).is_err());
        tracker.set_limit(Some(95)).unwrap();
        assert_eq!(tracker.status().limit, Some(95));
        drop(reservation);
    }
}
