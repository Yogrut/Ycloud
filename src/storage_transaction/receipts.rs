//! Durable upload results, separate from the in-flight replacement journal.
//! A receipt proves one operation, not merely that a same-sized file exists.

use std::{collections::HashSet, path::PathBuf, sync::atomic::Ordering};

use serde::{Deserialize, Serialize};
#[cfg(not(target_os = "linux"))]
use tokio::fs;
use tokio::io::AsyncReadExt;

use super::{metadata::MAX_JOURNAL_BYTES, InventoryKind, TransactionPaths};
use crate::{
    error::{AppError, AppResult},
    storage::StorageService,
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
        let mut count = 0_usize;
        for path in self.receipt_inventory().await? {
            if path.extension().and_then(|extension| extension.to_str()) == Some("tmp") {
                self.rooted_remove_file(&path).await?;
                self.rooted_sync_parent(&path).await?;
                continue;
            }
            count = count.checked_add(1).ok_or(AppError::TooManyRequests)?;
        }
        self.receipt_count.store(count, Ordering::Relaxed);
        Ok(())
    }

    async fn receipt_inventory(&self) -> AppResult<Vec<PathBuf>> {
        // One durable file plus one interrupted staging file per operation.
        // Use the receipt budget, not the smaller replacement-journal budget.
        // On Linux this also binds enumeration and metadata to the open root.
        let (entries, more) = self
            .inventory_page(
                &self.receipts,
                InventoryKind::Receipt,
                MAX_UPLOAD_RECEIPTS * 2,
            )
            .await?;
        if more {
            return Err(AppError::Conflict(
                "Upload receipt inventory exceeds its budget; resources retained".into(),
            ));
        }
        Ok(entries)
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
        for path in self.receipt_inventory().await? {
            let operation_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| AppError::Conflict("unrecognized upload receipt entry".into()))?;
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                return Err(AppError::Conflict(
                    "unrecognized upload receipt entry".into(),
                ));
            }
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

#[cfg(test)]
mod tests {
    use super::TransactionPaths;
    use crate::test_support::TestDirectory;
    use std::{collections::HashSet, sync::atomic::Ordering};

    #[tokio::test]
    async fn initialization_counts_durable_receipts_and_pruning_keeps_retained_results() {
        let root = TestDirectory::new("receipt-inventory");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let retained = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let expired = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        paths
            .ensure_receipt(retained, "first.txt", 3, true)
            .await
            .unwrap();
        paths
            .ensure_receipt(expired, "second.txt", 4, true)
            .await
            .unwrap();
        let staging = paths.receipt_path(retained).unwrap().with_extension("tmp");
        tokio::fs::write(&staging, b"partial").await.unwrap();
        drop(paths);

        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert_eq!(recovered.receipt_count.load(Ordering::Relaxed), 2);
        assert!(!staging.exists());
        recovered
            .prune_receipts(&HashSet::from([retained.into()]))
            .await
            .unwrap();
        assert_eq!(recovered.receipt_count.load(Ordering::Relaxed), 1);
        assert!(recovered
            .receipt_matches("first.txt", 3, retained)
            .await
            .unwrap());
        assert!(!recovered
            .receipt_matches("second.txt", 4, expired)
            .await
            .unwrap());

        recovered.prune_receipts(&HashSet::new()).await.unwrap();
        assert_eq!(recovered.receipt_count.load(Ordering::Relaxed), 0);
    }
}
