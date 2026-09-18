//! Explicit abandoned staging ownership. Never scan active uploads or copies.
use super::atomic_write::UploadResources;
#[cfg(test)]
use crate::storage_transaction::remove_any_bounded;
use crate::{
    error::{AppError, AppResult},
    storage_transaction::{RemovalProgress, TransactionPaths, DELETION_NODES_PER_PASS},
};
#[cfg(test)]
use std::path::Path;
use std::{
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    fs::File,
    sync::{mpsc, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

const MAX_OWNERS: usize = 128;
const BATCH_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UploadCleanupStatus {
    /// Abandoned jobs, including pending I/O and failures; not a byte ledger.
    pub pending_uploads: usize,
    /// Completed copy I/O awaiting staging removal, separate from uploads.
    pub pending_copies: usize,
    pub failed_attempts: u64,
}

#[derive(Default)]
struct Counters {
    pending: AtomicUsize,
    copies: AtomicUsize,
    failed: AtomicU64,
}

#[derive(Clone)]
pub(super) struct UploadCleanupWorker {
    sender: mpsc::Sender<AbandonedUpload>,
    slots: Arc<Semaphore>,
    counters: Arc<Counters>,
}

pub(super) struct UploadCleanupTicket {
    delivery: mpsc::OwnedPermit<AbandonedUpload>,
    slot: OwnedSemaphorePermit,
    counters: Arc<Counters>,
}

struct AbandonedUpload {
    kind: StagingKind,
    path: PathBuf,
    file: Option<File>,
    resources: Option<UploadResources>,
    _slot: OwnedSemaphorePermit,
}

#[derive(Clone, Copy)]
enum StagingKind {
    Upload,
    Copy,
}

impl StagingKind {
    fn counter(self, counters: &Counters) -> &AtomicUsize {
        match self {
            Self::Upload => &counters.pending,
            Self::Copy => &counters.copies,
        }
    }
}

struct PendingUpload {
    upload: AbandonedUpload,
    next: Instant,
    delay: Duration,
}

impl UploadCleanupWorker {
    pub(super) fn start(paths: Arc<TransactionPaths>, io: Arc<Semaphore>) -> Self {
        let removal_paths = paths.clone();
        Self::start_with_limits(
            paths,
            io,
            MAX_OWNERS,
            Duration::from_secs(1),
            move |path, kind| {
                let paths = removal_paths.clone();
                async move {
                    match kind {
                        StagingKind::Upload => paths.remove_staging_file(&path).await,
                        StagingKind::Copy => {
                            let progress =
                                paths.remove_bounded(&path, DELETION_NODES_PER_PASS).await?;
                            if progress.complete {
                                paths.sync_parent_of(&path).await?;
                            }
                            Ok(progress)
                        }
                    }
                }
            },
        )
    }

    fn start_with_limits<F, Fut>(
        paths: Arc<TransactionPaths>,
        io: Arc<Semaphore>,
        limit: usize,
        retry: Duration,
        remove: F,
    ) -> Self
    where
        F: FnMut(PathBuf, StagingKind) -> Fut + Send + 'static,
        Fut: Future<Output = AppResult<RemovalProgress>> + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel(limit);
        let counters = Arc::new(Counters::default());
        tokio::spawn(run(paths, io, receiver, counters.clone(), retry, remove));
        Self {
            sender,
            slots: Arc::new(Semaphore::new(limit)),
            counters,
        }
    }

    // Reserve before creating any file. Uploads and copies, active or retrying,
    // consume the same bound; dequeueing a failure does not free admission.
    pub(super) fn reserve(&self) -> AppResult<UploadCleanupTicket> {
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
            AppError::ServiceUnavailable(
                "Staging cleanup capacity is busy; retry after pending operations finish".into(),
            )
        })?;
        let delivery =
            self.sender.clone().try_reserve_owned().map_err(|_| {
                AppError::ServiceUnavailable("Staging cleanup is unavailable".into())
            })?;
        Ok(UploadCleanupTicket {
            delivery,
            slot,
            counters: self.counters.clone(),
        })
    }

    pub(super) fn status(&self) -> UploadCleanupStatus {
        UploadCleanupStatus {
            pending_uploads: self.counters.pending.load(Ordering::Relaxed),
            pending_copies: self.counters.copies.load(Ordering::Relaxed),
            failed_attempts: self.counters.failed.load(Ordering::Relaxed),
        }
    }
}

impl UploadCleanupTicket {
    pub(super) fn abandon(
        self,
        path: PathBuf,
        file: Option<File>,
        resources: Option<UploadResources>,
    ) {
        self.counters.pending.fetch_add(1, Ordering::Relaxed);
        // The reserved channel permit makes Drop handoff infallible while the
        // receiver lives. No allocation of a task, blocking I/O or queue wait.
        self.delivery.send(AbandonedUpload {
            kind: StagingKind::Upload,
            path,
            file,
            resources,
            _slot: self.slot,
        });
    }

    // Only call after the owned copy executor has awaited all staging I/O.
    // This is an explicit handoff, never a cancellation-time directory scan.
    pub(super) fn abandon_completed_copy(self, path: PathBuf) {
        self.counters.copies.fetch_add(1, Ordering::Relaxed);
        self.delivery.send(AbandonedUpload {
            kind: StagingKind::Copy,
            path,
            file: None,
            resources: None,
            _slot: self.slot,
        });
    }
}

impl AbandonedUpload {
    fn release_io_if_finished(&mut self) -> bool {
        if let Some(file) = self.file.take() {
            // Unlike into_std().await, this also safely refuses conversion
            // while a cancelled sync operation still owns a cloned handle.
            match file.try_into_std() {
                Ok(file) => drop(file),
                Err(file) => {
                    self.file = Some(file);
                    return false;
                }
            }
        }
        self.resources.take();
        true
    }
}

#[cfg(test)]
async fn remove_upload(path: &Path) -> AppResult<RemovalProgress> {
    let removed_nodes = match tokio::fs::remove_file(path).await {
        Ok(()) => 1,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(AppError::with_source(
                "failed to remove abandoned upload",
                error,
            ))
        }
    };
    super::sync_parent_directory(path).await?;
    Ok(RemovalProgress {
        removed_nodes,
        complete: true,
    })
}

#[cfg(test)]
async fn remove_staging(path: &Path, kind: StagingKind) -> AppResult<RemovalProgress> {
    match kind {
        StagingKind::Upload => remove_upload(path).await,
        StagingKind::Copy => {
            let progress = remove_any_bounded(path, DELETION_NODES_PER_PASS).await?;
            if progress.complete {
                super::sync_parent_directory(path).await?;
            }
            Ok(progress)
        }
    }
}

async fn run<F, Fut>(
    _paths: Arc<TransactionPaths>,
    io: Arc<Semaphore>,
    mut receiver: mpsc::Receiver<AbandonedUpload>,
    counters: Arc<Counters>,
    retry_min: Duration,
    mut remove: F,
) where
    F: FnMut(PathBuf, StagingKind) -> Fut,
    Fut: Future<Output = AppResult<RemovalProgress>>,
{
    let mut pending: Vec<PendingUpload> = Vec::new();
    let mut closing = false;
    loop {
        if closing && pending.is_empty() {
            break;
        }
        let next = pending
            .iter()
            .map(|job| job.next)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        tokio::select! {
            message = receiver.recv(), if !closing => match message {
                Some(upload) => pending.push(PendingUpload { upload, next: Instant::now(), delay: retry_min }),
                None => { closing = true; for job in &mut pending { job.next = Instant::now(); } },
            },
            _ = tokio::time::sleep_until(next), if !pending.is_empty() => {},
        }
        for _ in 0..BATCH_SIZE {
            let Some(index) = pending.iter().position(|job| job.next <= Instant::now()) else {
                break;
            };
            let mut job = pending.swap_remove(index);
            if job.upload.release_io_if_finished() {
                // Other abandoned uploads may own every remaining I/O permit.
                // Do not block receiving/releasing them while waiting for one.
                let result = match io.try_acquire() {
                    Ok(_permit) => Some(remove(job.upload.path.clone(), job.upload.kind).await),
                    Err(tokio::sync::TryAcquireError::NoPermits) => None,
                    Err(tokio::sync::TryAcquireError::Closed) => Some(Err(
                        AppError::ServiceUnavailable("Storage is shutting down".into()),
                    )),
                };
                match result {
                    Some(Ok(progress)) if progress.complete => {
                        job.upload
                            .kind
                            .counter(&counters)
                            .fetch_sub(1, Ordering::Relaxed);
                        continue;
                    }
                    Some(Ok(_)) => {
                        // Release the I/O permit between chunks. During normal
                        // service life continue promptly; graceful close leaves
                        // the remaining owned tree for bounded startup recovery.
                        if closing {
                            continue;
                        }
                        job.next = Instant::now() + Duration::from_millis(10);
                        pending.push(job);
                        continue;
                    }
                    Some(Err(_)) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("abandoned staging cleanup incomplete; retained for retry/startup recovery");
                        // One final cleanup attempt after last sender closes.
                        if closing {
                            continue;
                        }
                    }
                    None => {}
                }
            }
            // Unfinished file I/O retains both budgets, even during graceful
            // close. OS I/O and runtime shutdown still have no hard guarantee.
            job.next = Instant::now() + job.delay;
            job.delay = (job.delay * 2).min(Duration::from_secs(60));
            pending.push(job);
        }
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests;
