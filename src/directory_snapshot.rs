use std::{
    collections::HashMap,
    mem::size_of,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

use crate::directory_listing::{
    normalized_search, BackendEntry, DirectoryEntryFilter, DirectoryListRequest, DirectoryPage,
    DirectoryPageCollector, DirectorySort, SortDirection,
};

const SNAPSHOT_TTL: Duration = Duration::from_secs(30);
const MAX_SNAPSHOTS: usize = 8;
const MAX_SNAPSHOT_ENTRIES: usize = 100_000;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONCURRENT_BUILDS: usize = 2;

pub(crate) enum DirectorySnapshotCandidateResult {
    Snapshot(Vec<BackendEntry>),
    Page(DirectoryPage),
}

pub(crate) struct DirectorySnapshotCandidate {
    request: DirectoryListRequest,
    search: Option<String>,
    max_entries: usize,
    max_bytes: usize,
    state: CandidateState,
}

enum CandidateState {
    Snapshot {
        entries: Vec<BackendEntry>,
        estimated_bytes: usize,
    },
    Page(DirectoryPageCollector),
}

impl DirectorySnapshotCandidate {
    pub(crate) fn new(request: DirectoryListRequest) -> Self {
        Self::with_limits(request, MAX_SNAPSHOT_ENTRIES, MAX_SNAPSHOT_BYTES)
    }

    fn with_limits(request: DirectoryListRequest, max_entries: usize, max_bytes: usize) -> Self {
        Self {
            search: normalized_search(request.search.as_deref()),
            request,
            max_entries,
            max_bytes,
            state: CandidateState::Snapshot {
                entries: Vec::new(),
                estimated_bytes: 0,
            },
        }
    }

    pub(crate) fn consider(&mut self, entry: BackendEntry) {
        if !self.request.filter.accepts(&entry) {
            return;
        }
        if self
            .search
            .as_ref()
            .is_some_and(|search| !entry.name.to_lowercase().contains(search))
        {
            return;
        }
        let CandidateState::Snapshot {
            entries,
            estimated_bytes,
        } = &mut self.state
        else {
            if let CandidateState::Page(collector) = &mut self.state {
                collector.consider(entry);
            }
            return;
        };
        let entry_bytes = estimate_entry(&entry);
        if entries.len() < self.max_entries
            && estimated_bytes.saturating_add(entry_bytes) <= self.max_bytes
        {
            entries.push(entry);
            *estimated_bytes = estimated_bytes.saturating_add(entry_bytes);
            return;
        }

        let mut collector = DirectoryPageCollector::new(self.request.clone());
        for retained in entries.drain(..) {
            collector.consider(retained);
        }
        collector.consider(entry);
        self.state = CandidateState::Page(collector);
    }

    pub(crate) fn finish(self) -> DirectorySnapshotCandidateResult {
        match self.state {
            CandidateState::Snapshot { entries, .. } => {
                DirectorySnapshotCandidateResult::Snapshot(entries)
            }
            CandidateState::Page(collector) => {
                DirectorySnapshotCandidateResult::Page(collector.finish())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DirectorySnapshotKey {
    directory: String,
    search: Option<String>,
    sort: DirectorySort,
    direction: SortDirection,
    filter: DirectoryEntryFilter,
}

impl DirectorySnapshotKey {
    pub(crate) fn new(directory: &str, request: &DirectoryListRequest) -> Self {
        Self {
            directory: directory.to_string(),
            search: normalized_search(request.search.as_deref()),
            sort: request.sort,
            direction: request.direction,
            filter: request.filter,
        }
    }

    pub(crate) fn sort(&self) -> DirectorySort {
        self.sort
    }

    pub(crate) fn direction(&self) -> SortDirection {
        self.direction
    }
}

#[derive(Clone)]
pub(crate) struct DirectorySnapshotStore {
    inner: Arc<SnapshotInner>,
}

struct SnapshotInner {
    state: Mutex<SnapshotState>,
    build_gate: Arc<Semaphore>,
    ttl: Duration,
    max_snapshots: usize,
    max_entries: usize,
    max_bytes: usize,
}

#[derive(Default)]
struct SnapshotState {
    generation: u64,
    next_build_id: u64,
    retained_bytes: usize,
    snapshots: HashMap<DirectorySnapshotKey, RetainedSnapshot>,
    builds: HashMap<DirectorySnapshotKey, ActiveBuild>,
}

struct RetainedSnapshot {
    entries: Arc<Vec<BackendEntry>>,
    created_at: Instant,
    last_used: Instant,
    estimated_bytes: usize,
}

struct ActiveBuild {
    id: u64,
    completed: watch::Sender<bool>,
}

pub(crate) enum SnapshotClaim {
    Ready(Arc<Vec<BackendEntry>>),
    Wait(watch::Receiver<bool>),
    Build(SnapshotBuild),
}

pub(crate) struct SnapshotBuild {
    inner: Arc<SnapshotInner>,
    key: DirectorySnapshotKey,
    id: u64,
    generation: u64,
    completed: bool,
}

pub(crate) struct SnapshotInvalidation {
    store: DirectorySnapshotStore,
}

impl DirectorySnapshotStore {
    pub(crate) fn new() -> Self {
        Self::with_limits(
            SNAPSHOT_TTL,
            MAX_SNAPSHOTS,
            MAX_SNAPSHOT_ENTRIES,
            MAX_SNAPSHOT_BYTES,
            MAX_CONCURRENT_BUILDS,
        )
    }

    fn with_limits(
        ttl: Duration,
        max_snapshots: usize,
        max_entries: usize,
        max_bytes: usize,
        max_concurrent_builds: usize,
    ) -> Self {
        Self {
            inner: Arc::new(SnapshotInner {
                state: Mutex::new(SnapshotState::default()),
                build_gate: Arc::new(Semaphore::new(max_concurrent_builds.max(1))),
                ttl,
                max_snapshots: max_snapshots.max(1),
                max_entries: max_entries.max(1),
                max_bytes: max_bytes.max(1),
            }),
        }
    }

    pub(crate) fn claim(&self, key: DirectorySnapshotKey) -> SnapshotClaim {
        let now = Instant::now();
        let mut state = self.lock_state();
        prune_expired(&mut state, self.inner.ttl, now);
        if let Some(snapshot) = state.snapshots.get_mut(&key) {
            snapshot.last_used = now;
            return SnapshotClaim::Ready(snapshot.entries.clone());
        }
        if let Some(build) = state.builds.get(&key) {
            return SnapshotClaim::Wait(build.completed.subscribe());
        }
        let id = state.next_build_id;
        state.next_build_id = state.next_build_id.wrapping_add(1);
        let generation = state.generation;
        let (completed, _) = watch::channel(false);
        state.builds.insert(
            key.clone(),
            ActiveBuild {
                id,
                completed: completed.clone(),
            },
        );
        SnapshotClaim::Build(SnapshotBuild {
            inner: self.inner.clone(),
            key,
            id,
            generation,
            completed: false,
        })
    }

    pub(crate) async fn acquire_build_permit(&self) -> OwnedSemaphorePermit {
        self.inner
            .build_gate
            .clone()
            .acquire_owned()
            .await
            .expect("directory snapshot build semaphore remains open")
    }

    pub(crate) fn invalidate_on_drop(&self) -> SnapshotInvalidation {
        SnapshotInvalidation {
            store: self.clone(),
        }
    }

    pub(crate) fn invalidate(&self) {
        let mut state = self.lock_state();
        state.generation = state.generation.wrapping_add(1);
        state.snapshots.clear();
        state.retained_bytes = 0;
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SnapshotState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SnapshotBuild {
    pub(crate) fn publish(mut self, entries: Arc<Vec<BackendEntry>>) {
        let estimated_bytes = estimate_entries(&entries);
        let now = Instant::now();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let is_current = state
            .builds
            .get(&self.key)
            .is_some_and(|build| build.id == self.id);
        let sender = if is_current {
            state.builds.remove(&self.key).map(|build| build.completed)
        } else {
            None
        };
        if is_current
            && state.generation == self.generation
            && entries.len() <= self.inner.max_entries
            && estimated_bytes <= self.inner.max_bytes
        {
            while state.snapshots.len() >= self.inner.max_snapshots
                || state.retained_bytes.saturating_add(estimated_bytes) > self.inner.max_bytes
            {
                let Some(oldest) = state
                    .snapshots
                    .iter()
                    .min_by_key(|(_, snapshot)| snapshot.last_used)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                if let Some(removed) = state.snapshots.remove(&oldest) {
                    state.retained_bytes =
                        state.retained_bytes.saturating_sub(removed.estimated_bytes);
                }
            }
            if state.retained_bytes.saturating_add(estimated_bytes) <= self.inner.max_bytes {
                state.retained_bytes = state.retained_bytes.saturating_add(estimated_bytes);
                state.snapshots.insert(
                    self.key.clone(),
                    RetainedSnapshot {
                        entries,
                        created_at: now,
                        last_used: now,
                        estimated_bytes,
                    },
                );
            }
        }
        self.completed = true;
        drop(state);
        if let Some(sender) = sender {
            let _ = sender.send(true);
        }
    }
}

impl Drop for SnapshotBuild {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let sender = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .builds
                .get(&self.key)
                .is_some_and(|build| build.id == self.id)
            {
                state.builds.remove(&self.key).map(|build| build.completed)
            } else {
                None
            }
        };
        if let Some(sender) = sender {
            let _ = sender.send(true);
        }
    }
}

impl Drop for SnapshotInvalidation {
    fn drop(&mut self) {
        self.store.invalidate();
    }
}

fn prune_expired(state: &mut SnapshotState, ttl: Duration, now: Instant) {
    let expired = state
        .snapshots
        .iter()
        .filter(|(_, snapshot)| now.duration_since(snapshot.created_at) >= ttl)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in expired {
        if let Some(removed) = state.snapshots.remove(&key) {
            state.retained_bytes = state.retained_bytes.saturating_sub(removed.estimated_bytes);
        }
    }
}

fn estimate_entries(entries: &[BackendEntry]) -> usize {
    entries.iter().fold(0_usize, |total, entry| {
        total.saturating_add(estimate_entry(entry))
    })
}

fn estimate_entry(entry: &BackendEntry) -> usize {
    size_of::<BackendEntry>()
        .saturating_add(entry.name.len())
        .saturating_add(entry.relative.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> DirectorySnapshotKey {
        DirectorySnapshotKey::new(
            path,
            &DirectoryListRequest {
                limit: 20,
                search: None,
                sort: DirectorySort::Name,
                direction: SortDirection::Asc,
                filter: DirectoryEntryFilter::All,
                after: None,
            },
        )
    }

    fn entries(count: usize) -> Arc<Vec<BackendEntry>> {
        Arc::new(
            (0..count)
                .map(|index| BackendEntry {
                    name: format!("entry-{index}"),
                    relative: format!("entry-{index}"),
                    is_dir: false,
                    size: index as u64,
                    modified_unix: None,
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn identical_cold_queries_share_one_build_and_cancel_wakes_waiters() {
        let store = DirectorySnapshotStore::new();
        let SnapshotClaim::Build(build) = store.claim(key("shared")) else {
            panic!("first query must own the build");
        };
        let SnapshotClaim::Wait(mut waiter) = store.claim(key("shared")) else {
            panic!("second query must wait for the same build");
        };
        drop(build);
        waiter.changed().await.unwrap();
        assert!(matches!(
            store.claim(key("shared")),
            SnapshotClaim::Build(_)
        ));
    }

    #[test]
    fn invalidation_discards_an_old_generation_and_budget_falls_back_without_rejection() {
        let store = DirectorySnapshotStore::with_limits(
            Duration::from_secs(60),
            1,
            1,
            size_of::<BackendEntry>() * 2,
            1,
        );
        let SnapshotClaim::Build(build) = store.claim(key("old")) else {
            panic!("expected build");
        };
        store.invalidate();
        build.publish(entries(1));
        assert!(matches!(store.claim(key("old")), SnapshotClaim::Build(_)));

        let SnapshotClaim::Build(build) = store.claim(key("large")) else {
            panic!("expected build");
        };
        build.publish(entries(2));
        assert!(matches!(store.claim(key("large")), SnapshotClaim::Build(_)));
    }

    #[test]
    fn candidate_switches_to_bounded_page_collection_as_soon_as_budget_is_exceeded() {
        let request = DirectoryListRequest {
            limit: 2,
            search: None,
            sort: DirectorySort::Name,
            direction: SortDirection::Asc,
            filter: DirectoryEntryFilter::All,
            after: None,
        };
        let mut candidate = DirectorySnapshotCandidate::with_limits(request, 2, usize::MAX);
        for name in ["c", "a", "b"] {
            candidate.consider(BackendEntry {
                name: name.into(),
                relative: name.into(),
                is_dir: false,
                size: 0,
                modified_unix: None,
            });
        }
        let DirectorySnapshotCandidateResult::Page(page) = candidate.finish() else {
            panic!("the third entry must switch to the bounded page collector");
        };
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(page.next_position.is_some());
    }
}
