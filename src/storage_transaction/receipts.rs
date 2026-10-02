//! Durable upload results, separate from the in-flight replacement journal.
//! A receipt proves one operation, not merely that a same-sized file exists.

use std::{collections::HashSet, path::PathBuf, sync::atomic::Ordering};

use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncReadExt};

use super::{metadata::MAX_JOURNAL_BYTES, TransactionPaths};
use crate::{
    error::{AppError, AppResult},
    storage::{is_link_or_reparse_point, StorageService},
};

pub(super) const MAX_UPLOAD_RECEIPTS: usize = 100_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadReceipt {
    version: u32,
    operation_id: String,
    destination: String,
    size: u64,
    confirmed: bool,
}

impl TransactionPaths {
    pub(super) async fn check_receipt_capacity(&self, operation_id: &str) -> AppResult<()> {
        if self.read_receipt(operation_id).await?.is_none()
            && self.receipt_count.load(Ordering::Relaxed) >= MAX_UPLOAD_RECEIPTS
        {
            return Err(AppError::TooManyRequests);
        }
        Ok(())
    }

    fn receipt_path(&self, operation_id: &str) -> AppResult<PathBuf> {
        if !super::metadata::valid_operation_id(operation_id) {
            return Err(AppError::BadRequest("invalid upload operation ID".into()));
        }
        Ok(self.receipts.join(format!("{operation_id}.json")))
    }

    pub(super) async fn load_receipt_count(&self) -> AppResult<()> {
        let mut entries = fs::read_dir(&self.receipts)
            .await
            .map_err(|error| AppError::with_source("failed to inspect upload receipts", error))?;
        let mut count = 0_usize;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| AppError::with_source("failed to inspect upload receipts", error))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".tmp") {
                self.receipt_path(id)?;
                let metadata = fs::symlink_metadata(entry.path()).await.map_err(|error| {
                    AppError::with_source("failed to inspect upload receipt staging", error)
                })?;
                if !metadata.is_file() || is_link_or_reparse_point(&metadata) {
                    return Err(AppError::Conflict("invalid upload receipt staging".into()));
                }
                self.rooted_remove_file(&entry.path()).await?;
                self.rooted_sync_parent(&entry.path()).await?;
                continue;
            }
            let id = name
                .strip_suffix(".json")
                .ok_or_else(|| AppError::Conflict("unrecognized upload receipt entry".into()))?;
            self.receipt_path(id)?;
            let metadata = fs::symlink_metadata(entry.path()).await.map_err(|error| {
                AppError::with_source("failed to inspect upload receipt", error)
            })?;
            if !metadata.is_file() || is_link_or_reparse_point(&metadata) {
                return Err(AppError::Conflict("invalid upload receipt entry".into()));
            }
            count = count.checked_add(1).ok_or(AppError::TooManyRequests)?;
        }
        self.receipt_count.store(count, Ordering::Relaxed);
        Ok(())
    }

    async fn read_receipt(&self, operation_id: &str) -> AppResult<Option<UploadReceipt>> {
        let path = self.receipt_path(operation_id)?;
        let Some(metadata) = self.rooted_metadata(&path).await? else {
            return Ok(None);
        };
        if !metadata.is_file() || metadata.len() > MAX_JOURNAL_BYTES {
            return Err(AppError::Conflict("invalid upload receipt".into()));
        }
        #[cfg(target_os = "linux")]
        let file = self
            .linux_root
            .open_file_for_read(&self.rooted_relative(&path)?)
            .await?;
        #[cfg(not(target_os = "linux"))]
        let file = fs::File::open(&path)
            .await
            .map_err(|error| AppError::with_source("failed to read upload receipt", error))?;
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| AppError::with_source("failed to read upload receipt", error))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(AppError::Conflict("invalid upload receipt".into()));
        }
        let receipt: UploadReceipt = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::with_source("invalid upload receipt", error))?;
        if receipt.version != 1
            || receipt.operation_id != operation_id
            || StorageService::normalize_relative(&receipt.destination)? != receipt.destination
        {
            return Err(AppError::Conflict("invalid upload receipt identity".into()));
        }
        Ok(Some(receipt))
    }

    pub(super) async fn ensure_receipt(
        &self,
        operation_id: &str,
        destination: &str,
        size: u64,
        confirmed: bool,
    ) -> AppResult<()> {
        if let Some(existing) = self.read_receipt(operation_id).await? {
            if existing.destination == destination
                && existing.size == size
                && existing.confirmed == confirmed
            {
                return Ok(());
            }
            return Err(AppError::Conflict(
                "upload receipt identity collision".into(),
            ));
        }
        if self.receipt_count.load(Ordering::Relaxed) >= MAX_UPLOAD_RECEIPTS {
            return Err(AppError::TooManyRequests);
        }
        let receipt = UploadReceipt {
            version: 1,
            operation_id: operation_id.into(),
            destination: destination.into(),
            size,
            confirmed,
        };
        let path = self.receipt_path(operation_id)?;
        self.rooted_remove_file_idempotent(&path.with_extension("tmp"))
            .await?;
        self.write_json_atomic(&path, &receipt).await?;
        self.receipt_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) async fn receipt_matches(
        &self,
        destination: &str,
        size: u64,
        operation_id: &str,
    ) -> AppResult<bool> {
        match self.read_receipt(operation_id).await? {
            Some(receipt) if !receipt.confirmed => {
                Err(AppError::Conflict("上传发布结果未确认，保留待核查".into()))
            }
            Some(receipt) => Ok(receipt.destination == destination && receipt.size == size),
            None => Ok(false),
        }
    }

    pub(crate) async fn prune_receipts(&self, retained: &HashSet<String>) -> AppResult<()> {
        let mut entries = fs::read_dir(&self.receipts)
            .await
            .map_err(|error| AppError::with_source("failed to scan upload receipts", error))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| AppError::with_source("failed to scan upload receipts", error))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let operation_id = name
                .strip_suffix(".json")
                .ok_or_else(|| AppError::Conflict("unrecognized upload receipt entry".into()))?;
            let path = self.receipt_path(operation_id)?;
            if retained.contains(operation_id) {
                continue;
            }
            self.read_receipt(operation_id).await?;
            self.rooted_remove_file(&path).await?;
            self.receipt_count.fetch_sub(1, Ordering::Relaxed);
            self.rooted_sync_parent(&path).await?;
        }
        Ok(())
    }

    pub(crate) async fn prune_receipt(&self, operation_id: &str) -> AppResult<()> {
        let path = self.receipt_path(operation_id)?;
        if self.read_receipt(operation_id).await?.is_none() {
            return Ok(());
        }
        self.rooted_remove_file(&path).await?;
        self.receipt_count.fetch_sub(1, Ordering::Relaxed);
        self.rooted_sync_parent(&path).await?;
        Ok(())
    }
}
