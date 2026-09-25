use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit},
};

#[cfg(any(not(target_os = "linux"), test))]
use super::is_link_or_reparse_point;
use super::StorageService;
use crate::{
    error::{AppError, AppResult},
    storage_transaction::{TransactionId, TransactionPaths, UploadOwnership},
};

mod publication;
#[cfg(test)]
mod tests;

impl StorageService {
    pub async fn begin_atomic_write(&self, path: &str) -> AppResult<AtomicFileWriter> {
        self.begin_atomic_write_internal(path, None, create_temporary)
            .await
    }

    pub async fn begin_atomic_write_with_expected(
        &self,
        path: &str,
        expected_bytes: u64,
    ) -> AppResult<AtomicFileWriter> {
        self.begin_atomic_write_internal(path, Some(expected_bytes), create_temporary)
            .await
    }

    async fn begin_atomic_write_internal<F>(
        &self,
        path: &str,
        expected_bytes: Option<u64>,
        create: F,
    ) -> AppResult<AtomicFileWriter>
    where
        F: FnOnce(&Path) -> std::io::Result<std::fs::File> + Send + 'static,
    {
        let max_upload_bytes = self.max_upload_bytes();
        if expected_bytes.is_some_and(|bytes| bytes > max_upload_bytes) {
            return Err(AppError::PayloadTooLarge);
        }
        let destination = self.resolve_for_write(path).await?;
        if destination.is_root() {
            return Err(AppError::BadRequest(
                "The storage root cannot be overwritten".into(),
            ));
        }
        #[cfg(any(not(target_os = "linux"), test))]
        {
            let parent = destination
                .absolute()
                .parent()
                .ok_or_else(|| AppError::BadRequest("Invalid destination path".into()))?;
            let parent_metadata =
                tokio::fs::symlink_metadata(parent)
                    .await
                    .map_err(|error| match error.kind() {
                        std::io::ErrorKind::NotFound => {
                            AppError::BadRequest("Destination directory does not exist".into())
                        }
                        _ => {
                            AppError::with_source("failed to inspect destination directory", error)
                        }
                    })?;
            if !parent_metadata.is_dir() || is_link_or_reparse_point(&parent_metadata) {
                return Err(AppError::Forbidden);
            }
        }

        // Re-check after directory inspection so a concurrently swapped link
        // cannot silently redirect the final write outside the storage root.
        self.resolve_for_write(destination.relative()).await?;

        // Waiting for admission must not create a temporary file or reserve
        // physical capacity. Cancellation here has no resource to abandon.
        let cleanup_ticket = self.upload_cleanup.reserve()?;
        let permit = self
            .io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;
        let reservation = self
            .reserve_physical_bytes(expected_bytes.unwrap_or(0))
            .await?;
        let temporary = self.transactions.upload_path(&TransactionId::new());
        let transactions = self.transactions.clone();
        let mutation_gate = self.mutation_gate.clone();

        // The owned task holds both budgets until open has really finished and
        // a writer owns the file. Dropping the waiter cannot clean a path before
        // a still-running open creates it later.
        #[cfg(all(target_os = "linux", not(test)))]
        {
            drop(create);
            tokio::spawn(async move {
                let file = transactions.create_upload_file(&temporary).await?;
                Ok(AtomicFileWriter {
                    destination: destination.absolute,
                    temporary,
                    file: Some(File::from_std(file)),
                    bytes_written: 0,
                    expected_bytes,
                    max_bytes: max_upload_bytes,
                    ownership: UploadOwnership::Writer,
                    resources: Some(UploadResources {
                        _permit: permit,
                        reservation,
                    }),
                    cleanup_ticket: Some(cleanup_ticket),
                    relative: destination.relative,
                    transactions,
                    mutation_gate,
                })
            })
            .await
            .map_err(|error| AppError::with_source("upload preparation task failed", error))?
        }

        #[cfg(any(not(target_os = "linux"), test))]
        {
            tokio::task::spawn_blocking(move || {
                let file = create(&temporary).map_err(|error| {
                    AppError::with_source("failed to create temporary file", error)
                })?;
                Ok(AtomicFileWriter {
                    destination: destination.absolute,
                    temporary,
                    file: Some(File::from_std(file)),
                    bytes_written: 0,
                    expected_bytes,
                    max_bytes: max_upload_bytes,
                    ownership: UploadOwnership::Writer,
                    resources: Some(UploadResources {
                        _permit: permit,
                        reservation,
                    }),
                    cleanup_ticket: Some(cleanup_ticket),
                    relative: destination.relative,
                    transactions,
                    mutation_gate,
                })
            })
            .await
            .map_err(|error| AppError::with_source("upload preparation task failed", error))?
        }
    }

    pub(super) async fn reserve_physical_bytes(&self, bytes: u64) -> AppResult<UploadReservation> {
        #[cfg(target_os = "linux")]
        let available = self.linux_root.available_space().await?;
        #[cfg(not(target_os = "linux"))]
        let available = {
            let root = self.root.clone();
            tokio::task::spawn_blocking(move || fs4::available_space(root.as_path()))
                .await
                .map_err(|error| {
                    AppError::with_source("failed to inspect storage capacity", error)
                })?
                .map_err(|error| {
                    AppError::with_source("failed to inspect storage capacity", error)
                })?
        };
        let mut reserved = self
            .reserved_upload_bytes
            .lock()
            .map_err(|_| AppError::internal("storage reservation state is unavailable"))?;
        if available
            < self
                .disk_reserve_bytes
                .saturating_add(*reserved)
                .saturating_add(bytes)
        {
            return Err(AppError::InsufficientStorage);
        }
        *reserved = reserved.saturating_add(bytes);
        Ok(UploadReservation {
            reserved_upload_bytes: self.reserved_upload_bytes.clone(),
            remaining: bytes,
            #[cfg(not(target_os = "linux"))]
            root: self.root.clone(),
            #[cfg(target_os = "linux")]
            linux_root: self.linux_root.clone(),
            disk_reserve_bytes: self.disk_reserve_bytes,
        })
    }
}

pub struct AtomicFileWriter {
    destination: PathBuf,
    pub(super) temporary: PathBuf,
    file: Option<File>,
    bytes_written: u64,
    expected_bytes: Option<u64>,
    max_bytes: u64,
    ownership: UploadOwnership,
    resources: Option<UploadResources>,
    cleanup_ticket: Option<super::upload_cleanup::UploadCleanupTicket>,
    relative: String,
    transactions: Arc<TransactionPaths>,
    mutation_gate: Arc<AsyncMutex<()>>,
}

impl AtomicFileWriter {
    pub async fn write_chunk(&mut self, chunk: &Bytes) -> AppResult<()> {
        let new_size = self.bytes_written.saturating_add(chunk.len() as u64);
        if new_size > self.max_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        let resources = self
            .resources
            .as_mut()
            .ok_or_else(|| AppError::internal("upload resources already transferred"))?;
        resources.reservation.ensure(chunk.len() as u64).await?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.write_all(chunk)
            .await
            .map_err(|error| AppError::with_source("failed to write upload", error))?;
        // Tokio can acknowledge a buffered write before blocking disk I/O has
        // completed. Keep its reservation until completion, and surface late
        // write failures here rather than counting them as uploaded bytes.
        file.flush()
            .await
            .map_err(|error| AppError::with_source("failed to complete upload chunk", error))?;
        self.bytes_written = new_size;
        resources.reservation.consume(chunk.len() as u64);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomicWriteResult {
    pub size: u64,
    pub previous_size: u64,
}

impl Drop for AtomicFileWriter {
    fn drop(&mut self) {
        if self.ownership == UploadOwnership::Writer {
            if let Some(ticket) = self.cleanup_ticket.take() {
                ticket.abandon(
                    std::mem::take(&mut self.temporary),
                    self.file.take(),
                    self.resources.take(),
                );
            }
        }
    }
}

fn create_temporary(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub(super) struct UploadResources {
    _permit: OwnedSemaphorePermit,
    reservation: UploadReservation,
}

pub(super) struct UploadReservation {
    reserved_upload_bytes: Arc<Mutex<u64>>,
    remaining: u64,
    #[cfg(not(target_os = "linux"))]
    root: Arc<PathBuf>,
    #[cfg(target_os = "linux")]
    linux_root: Arc<super::linux_root::LinuxRoot>,
    disk_reserve_bytes: u64,
}

impl UploadReservation {
    async fn ensure(&mut self, bytes: u64) -> AppResult<()> {
        if self.remaining >= bytes {
            return Ok(());
        }
        const CHUNK: u64 = 8 * 1024 * 1024;
        let shortage = bytes - self.remaining;
        let additional = shortage.saturating_add(CHUNK - 1) / CHUNK * CHUNK;
        #[cfg(target_os = "linux")]
        let available = self.linux_root.available_space().await?;
        #[cfg(not(target_os = "linux"))]
        let available = {
            let root = self.root.clone();
            tokio::task::spawn_blocking(move || fs4::available_space(root.as_path()))
                .await
                .map_err(|error| {
                    AppError::with_source("failed to inspect storage capacity", error)
                })?
                .map_err(|error| {
                    AppError::with_source("failed to inspect storage capacity", error)
                })?
        };
        let mut reserved = self
            .reserved_upload_bytes
            .lock()
            .map_err(|_| AppError::internal("upload reservation state is unavailable"))?;
        if available
            < self
                .disk_reserve_bytes
                .saturating_add(*reserved)
                .saturating_add(additional)
        {
            return Err(AppError::InsufficientStorage);
        }
        *reserved = reserved.saturating_add(additional);
        self.remaining = self.remaining.saturating_add(additional);
        Ok(())
    }

    fn consume(&mut self, bytes: u64) {
        let consumed = bytes.min(self.remaining);
        if let Ok(mut reserved) = self.reserved_upload_bytes.lock() {
            *reserved = reserved.saturating_sub(consumed);
        }
        self.remaining -= consumed;
    }
}

impl Drop for UploadReservation {
    fn drop(&mut self) {
        if let Ok(mut reserved) = self.reserved_upload_bytes.lock() {
            *reserved = reserved.saturating_sub(self.remaining);
        }
    }
}
