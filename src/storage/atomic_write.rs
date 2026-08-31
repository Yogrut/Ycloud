use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit},
};
use uuid::Uuid;

use super::{is_link_or_reparse_point, StorageService};
use crate::{
    error::{AppError, AppResult},
    storage_transaction::TransactionPaths,
};

impl StorageService {
    pub async fn begin_atomic_write(&self, path: &str) -> AppResult<AtomicFileWriter> {
        self.begin_atomic_write_internal(path, None).await
    }

    pub async fn begin_atomic_write_with_expected(
        &self,
        path: &str,
        expected_bytes: u64,
    ) -> AppResult<AtomicFileWriter> {
        self.begin_atomic_write_internal(path, Some(expected_bytes))
            .await
    }

    async fn begin_atomic_write_internal(
        &self,
        path: &str,
        expected_bytes: Option<u64>,
    ) -> AppResult<AtomicFileWriter> {
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
                    _ => AppError::with_source("failed to inspect destination directory", error),
                })?;
        if !parent_metadata.is_dir() || is_link_or_reparse_point(&parent_metadata) {
            return Err(AppError::Forbidden);
        }

        // Re-check after directory inspection so a concurrently swapped link
        // cannot silently redirect the final write outside the storage root.
        self.resolve_for_write(destination.relative()).await?;

        let initially_reserved = expected_bytes.unwrap_or(0);
        let reservation = self.reserve_physical_bytes(initially_reserved).await?;

        let transaction_id = Uuid::new_v4().to_string();
        let temporary = self.transactions.upload_path(&transaction_id);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let file = options
            .open(&temporary)
            .await
            .map_err(|error| AppError::with_source("failed to create temporary file", error))?;
        let permit = self
            .io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;

        Ok(AtomicFileWriter {
            destination: destination.absolute,
            temporary,
            file: Some(file),
            bytes_written: 0,
            expected_bytes,
            max_bytes: max_upload_bytes,
            committed: false,
            _permit: permit,
            reservation,
            relative: destination.relative,
            transactions: self.transactions.clone(),
            mutation_gate: self.mutation_gate.clone(),
        })
    }

    pub(super) async fn reserve_physical_bytes(&self, bytes: u64) -> AppResult<UploadReservation> {
        let root = self.root.clone();
        let available = tokio::task::spawn_blocking(move || fs4::available_space(root.as_path()))
            .await
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?;
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
            root: self.root.clone(),
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
    committed: bool,
    _permit: OwnedSemaphorePermit,
    reservation: UploadReservation,
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
        self.reservation.ensure(chunk.len() as u64).await?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.write_all(chunk)
            .await
            .map_err(|error| AppError::with_source("failed to write upload", error))?;
        self.bytes_written = new_size;
        self.reservation.consume(chunk.len() as u64);
        Ok(())
    }

    pub async fn commit(mut self) -> AppResult<AtomicWriteResult> {
        if self
            .expected_bytes
            .is_some_and(|expected| expected != self.bytes_written)
        {
            return Err(AppError::BadRequest(
                "Uploaded size does not match Content-Length".into(),
            ));
        }
        let file = self
            .file
            .take()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.sync_all()
            .await
            .map_err(|error| AppError::with_source("failed to flush upload", error))?;
        drop(file);
        let _mutation = self.mutation_gate.lock().await;
        let previous_size = self
            .transactions
            .commit_file(&self.relative, &self.temporary, &self.destination)
            .await?;
        self.committed = true;
        Ok(AtomicWriteResult {
            size: self.bytes_written,
            previous_size,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomicWriteResult {
    pub size: u64,
    pub previous_size: u64,
}

impl Drop for AtomicFileWriter {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

pub(super) struct UploadReservation {
    reserved_upload_bytes: Arc<Mutex<u64>>,
    remaining: u64,
    root: Arc<PathBuf>,
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
        let root = self.root.clone();
        let available = tokio::task::spawn_blocking(move || fs4::available_space(root.as_path()))
            .await
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?
            .map_err(|error| AppError::with_source("failed to inspect storage capacity", error))?;
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
