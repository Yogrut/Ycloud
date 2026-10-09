//! An admitted copy owns execution and quota through publication/accounting.
//! Dropping the request waiter does not cancel filesystem I/O or its budgets.
#[cfg(any(not(target_os = "linux"), test))]
use super::require_plain_directory;
use super::{
    copy_directory_iterative, copy_file_synced, reject_root_or_descendant, ResolvedPath,
    StorageService,
};
use crate::{
    capacity::{CapacityReservation, CapacityTracker},
    error::{AppError, AppResult, CleanupState, CommitState},
    storage_transaction::TransactionId,
};
use std::{future::Future, path::PathBuf};
#[cfg(any(not(target_os = "linux"), test))]
use tokio::fs;

impl StorageService {
    pub async fn copy_path(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
    ) -> AppResult<()> {
        let expected = self.path_size(source).await?;
        self.copy_path_with_expected_size(source, destination, expected)
            .await
    }

    pub async fn copy_path_with_expected_size(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
        expected: u64,
    ) -> AppResult<()> {
        self.copy_owned(source, destination, expected, None, copy_staging)
            .await
    }

    pub(crate) async fn copy_path_with_capacity(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
        expected: u64,
        capacity: CapacityTracker,
    ) -> AppResult<()> {
        self.copy_owned(source, destination, expected, Some(capacity), copy_staging)
            .await
    }

    pub(crate) async fn transfer_with_capacity(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
        copy: bool,
        overwrite: bool,
        capacity: CapacityTracker,
    ) -> AppResult<bool> {
        reject_root_or_descendant(source, destination)?;
        if destination.is_root() {
            return Err(AppError::Forbidden);
        }
        if source
            .relative()
            .starts_with(&format!("{}/", destination.relative()))
        {
            return Err(AppError::Forbidden);
        }
        let ticket = self.upload_cleanup.reserve()?;
        let permit = self.acquire_io().await?;
        let storage = self.clone();
        let (source, destination) = (source.clone(), destination.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let _mutation = storage.mutation_gate.lock().await;
            storage.transactions.settle_publication().await?;
            let metadata = storage.metadata(&source).await?;
            let existing = match storage.metadata(&destination).await {
                Ok(metadata) => Some(metadata),
                Err(AppError::NotFound) => None,
                Err(error) => return Err(error),
            };
            if !overwrite && existing.is_some() {
                return Err(AppError::PreconditionFailed);
            }
            let old_size = if existing.is_some() {
                storage.path_size(&destination).await?
            } else {
                0
            };
            let size = if copy {
                storage.path_size(&source).await?
            } else {
                0
            };
            let mut accounting =
                CopyAccounting::reserve_replacement(Some(capacity), old_size, size)?;
            let _physical = storage.reserve_physical_bytes(size).await?;
            let temporary = if copy {
                storage.transactions.copy_path(&TransactionId::new())
            } else {
                source.absolute().to_path_buf()
            };
            let mut ownership = crate::storage_transaction::UploadOwnership::Writer;
            let result = async {
                if copy {
                    #[cfg(all(target_os = "linux", not(test)))]
                    storage
                        .linux_root
                        .copy_path(
                            source.relative(),
                            &storage.transactions.rooted_relative(&temporary)?,
                            metadata.is_dir(),
                        )
                        .await?;
                    #[cfg(any(not(target_os = "linux"), test))]
                    copy_staging(
                        source.absolute().to_path_buf(),
                        temporary.clone(),
                        metadata.is_dir(),
                    )
                    .await?;
                }
                storage
                    .transactions
                    .commit_transfer(
                        &temporary,
                        destination.relative(),
                        overwrite,
                        &mut ownership,
                        &mut accounting,
                        old_size,
                    )
                    .await
            }
            .await;
            if copy
                && result.is_err()
                && ownership == crate::storage_transaction::UploadOwnership::Writer
            {
                ticket.abandon_completed_copy(temporary);
            }
            let persisted = accounting.persist().await;
            storage.cleanup.notify();
            let commit = if accounting.was_published {
                CommitState::Committed
            } else if accounting.publication_started {
                CommitState::Unknown
            } else {
                CommitState::NotCommitted
            };
            match (result, persisted) {
                (Ok(created), Ok(())) => Ok(created),
                (Ok(_), Err(error)) => {
                    Err(error.with_operation(CommitState::Committed, CleanupState::Pending))
                }
                (Err(error), _) => Err(error.with_operation(
                    commit,
                    if commit == CommitState::NotCommitted {
                        CleanupState::Complete
                    } else {
                        CleanupState::Pending
                    },
                )),
            }
        })
        .await
        .map_err(|error| {
            AppError::with_source("local transfer task failed", error)
                .with_operation(CommitState::Unknown, CleanupState::Unknown)
        })?
    }

    async fn copy_owned<F, Fut>(
        &self,
        source: &ResolvedPath,
        destination: &ResolvedPath,
        expected: u64,
        capacity: Option<CapacityTracker>,
        copy: F,
    ) -> AppResult<()>
    where
        F: FnOnce(PathBuf, PathBuf, bool) -> Fut + Send + 'static,
        Fut: Future<Output = AppResult<()>> + Send + 'static,
    {
        reject_root_or_descendant(source, destination)?;
        // No task/file/quota exists while waiting for admission. Both waiting
        // and executing copies share the staging responsibility bound.
        let ticket = self.upload_cleanup.reserve()?;
        let permit = self.acquire_io().await?;
        let storage = self.clone();
        let source = source.clone();
        let destination = destination.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _mutation = storage.mutation_gate.lock().await;
            storage.transactions.settle_publication().await?;
            #[cfg(any(not(target_os = "linux"), test))]
            {
                require_plain_directory(destination.absolute().parent()).await?;
                ensure_destination_absent(destination.absolute()).await?;
            }
            let metadata = storage.metadata(&source).await?;
            if !metadata.is_dir() && !metadata.is_file() {
                return Err(AppError::Forbidden);
            }
            let size = storage.path_size(&source).await?;
            if size != expected {
                return Err(AppError::Conflict(
                    "Source changed while preparing the copy".into(),
                ));
            }
            let mut accounting = CopyAccounting::reserve(capacity, size)?;
            let _physical = storage.reserve_physical_bytes(size).await?;
            let temporary = storage.transactions.copy_path(&TransactionId::new());
            let result: AppResult<()> = async {
                #[cfg(all(target_os = "linux", not(test)))]
                {
                    drop(copy);
                    let temporary_relative = storage.transactions.rooted_relative(&temporary)?;
                    storage
                        .linux_root
                        .copy_path(source.relative(), &temporary_relative, metadata.is_dir())
                        .await?;
                }
                #[cfg(any(not(target_os = "linux"), test))]
                copy(
                    source.absolute().to_path_buf(),
                    temporary.clone(),
                    metadata.is_dir(),
                )
                .await?;
                #[cfg(any(not(target_os = "linux"), test))]
                {
                    require_plain_directory(destination.absolute().parent()).await?;
                    ensure_destination_absent(destination.absolute()).await?;
                }
                accounting.publication_started = true;
                storage
                    .transactions
                    .publish_noreplace(&temporary, destination.absolute())
                    .await?;
                // No await between successful publication and logical accounting.
                accounting.published(size);
                Ok(())
            }
            .await;
            // All per-copy I/O has completed before transferring a tree for
            // deletion. On success only the unused ticket is released.
            if result.is_err() {
                ticket.abandon_completed_copy(temporary);
            }
            let persisted = accounting.persist().await;
            let commit = if accounting.was_published {
                CommitState::Committed
            } else if accounting.publication_started {
                CommitState::Unknown
            } else {
                CommitState::NotCommitted
            };
            let result =
                result.map_err(|error| error.with_operation(commit, CleanupState::Pending));
            if result.is_err() {
                tracing::warn!(
                    "local copy failed; staging handed off and publication accounting retained"
                );
            }
            match (result, persisted) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(error)) => {
                    Err(error.with_operation(CommitState::Committed, CleanupState::Complete))
                }
                (Err(error), _) => Err(error),
            }
        })
        .await
        .map_err(|error| {
            AppError::with_source("copy execution task failed", error)
                .with_operation(CommitState::Unknown, CleanupState::Unknown)
        })?
    }
}

#[cfg(any(not(target_os = "linux"), test))]
async fn ensure_destination_absent(path: &std::path::Path) -> AppResult<()> {
    if fs::try_exists(path)
        .await
        .map_err(|error| AppError::with_source("failed to inspect copy destination", error))?
    {
        return Err(AppError::Conflict("Destination already exists".into()));
    }
    Ok(())
}

async fn copy_staging(source: PathBuf, destination: PathBuf, directory: bool) -> AppResult<()> {
    if directory {
        copy_directory_iterative(&source, &destination).await
    } else {
        copy_file_synced(&source, &destination).await
    }
}

struct CopyAccounting {
    capacity: Option<CapacityTracker>,
    reservation: Option<CapacityReservation>,
    publication_started: bool,
    was_published: bool,
    ledger_settled: bool,
    previous_size: u64,
    size: u64,
}

impl CopyAccounting {
    fn reserve(capacity: Option<CapacityTracker>, size: u64) -> AppResult<Self> {
        Self::reserve_replacement(capacity, 0, size)
    }

    fn reserve_replacement(
        capacity: Option<CapacityTracker>,
        previous_size: u64,
        size: u64,
    ) -> AppResult<Self> {
        let reservation = capacity
            .as_ref()
            .map(|tracker| tracker.reserve_replacement(previous_size, size))
            .transpose()?;
        Ok(Self {
            capacity,
            reservation,
            publication_started: false,
            was_published: false,
            ledger_settled: false,
            previous_size,
            size,
        })
    }

    fn published(&mut self, size: u64) {
        if let Some(reservation) = self.reservation.take() {
            reservation.commit(self.previous_size, size);
        }
        self.publication_started = false;
        self.was_published = true;
    }

    async fn persist(&mut self) -> AppResult<()> {
        if self.publication_started {
            if let Some(capacity) = &self.capacity {
                capacity.mark_uncertain();
            }
            return Ok(());
        }
        if !self.was_published {
            self.ledger_settled = true;
            return Ok(());
        }
        if let Some(capacity) = &self.capacity {
            if let Err(error) = capacity.persist().await {
                capacity.mark_uncertain();
                tracing::error!("failed to persist local copy capacity ledger");
                return Err(error);
            }
        }
        self.ledger_settled = true;
        Ok(())
    }
}

impl crate::storage_transaction::ReplacementObserver for CopyAccounting {
    fn prepare(&mut self, previous_size: u64) -> AppResult<()> {
        if let Some(reservation) = &mut self.reservation {
            reservation.rebase_replacement(previous_size, self.size)?;
        }
        self.previous_size = previous_size;
        Ok(())
    }
    fn recovery_owned(&mut self) {
        self.publication_started = true;
    }
    fn rollback_completed(&mut self) {
        self.publication_started = false;
    }
    fn published(&mut self, _: u64) {
        self.published(self.size);
    }
}

impl Drop for CopyAccounting {
    fn drop(&mut self) {
        if self.publication_started || self.was_published && !self.ledger_settled {
            if let Some(capacity) = &self.capacity {
                capacity.mark_uncertain();
            }
        }
    }
}

#[cfg(test)]
mod tests;
