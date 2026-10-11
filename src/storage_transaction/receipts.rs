//! Durable upload results, separate from the in-flight replacement journal.
//! A receipt proves one operation, not merely that a same-sized file exists.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use serde::{Deserialize, Serialize};
#[cfg(not(target_os = "linux"))]
use tokio::fs;
use tokio::io::AsyncReadExt;

use super::{metadata::MAX_JOURNAL_BYTES, InventoryKind, TransactionPaths};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState, OperationOutcome},
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
    #[serde(default)]
    rolled_back: bool,
}

impl TransactionPaths {
    pub(super) async fn check_receipt_capacity(&self, operation_id: &str) -> AppResult<()> {
        // A new execution must not inherit the rollback proof of its previous
        // failed attempt. This runs under the transaction mutation owner.
        if let Some(receipt) = self.read_receipt(operation_id).await? {
            if receipt.rolled_back {
                self.remove_receipt(&self.receipt_path(operation_id)?)
                    .await?;
            }
            return Ok(());
        }
        if self.receipt_count.load(Ordering::Relaxed) >= MAX_UPLOAD_RECEIPTS {
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
        outcome: CommitState,
    ) -> AppResult<()> {
        let confirmed = outcome != CommitState::Unknown;
        let rolled_back = outcome == CommitState::NotCommitted;
        let existing = self.read_receipt(operation_id).await?;
        if let Some(existing) = &existing {
            if existing.destination != destination || existing.size != size {
                return Err(AppError::Conflict(
                    "upload receipt identity collision".into(),
                ));
            }
            if existing.confirmed == confirmed && existing.rolled_back == rolled_back {
                return Ok(());
            }
            if !existing.rolled_back && (rolled_back || existing.confirmed) {
                return Err(AppError::Conflict(
                    "upload receipt outcome collision".into(),
                ));
            }
        }
        if existing.is_none() && self.receipt_count.load(Ordering::Relaxed) >= MAX_UPLOAD_RECEIPTS {
            return Err(AppError::TooManyRequests);
        }
        let receipt = UploadReceipt {
            version: 1,
            operation_id: operation_id.into(),
            destination: destination.into(),
            size,
            confirmed,
            rolled_back,
        };
        let path = self.receipt_path(operation_id)?;
        if existing.is_none() {
            self.rooted_remove_file_idempotent(&path.with_extension("tmp"))
                .await?;
        }
        self.write_json_atomic_inner(&path, &receipt, existing.is_some(), || {
            // Publication already created the file, even if directory sync
            // later fails. Its slot must exist before it can be pruned.
            if existing.is_none() {
                self.receipt_count.fetch_add(1, Ordering::Relaxed);
            }
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn receipt_outcome(
        &self,
        destination: &str,
        size: u64,
        operation_id: &str,
    ) -> AppResult<Option<OperationOutcome>> {
        let Some(receipt) = self.read_receipt(operation_id).await? else {
            return Ok(None);
        };
        if receipt.destination != destination || receipt.size != size {
            return Ok(None);
        }
        if !receipt.confirmed {
            return Ok(Some(OperationOutcome::new(
                CommitState::Unknown,
                CleanupState::Unknown,
            )));
        }
        Ok(Some(OperationOutcome::new(
            if receipt.rolled_back {
                CommitState::NotCommitted
            } else {
                CommitState::Committed
            },
            CleanupState::Complete,
        )))
    }

    #[cfg(test)]
    pub(crate) async fn receipt_matches(
        &self,
        destination: &str,
        size: u64,
        operation_id: &str,
    ) -> AppResult<bool> {
        let outcome = self
            .receipt_outcome(destination, size, operation_id)
            .await?;
        if outcome.is_some_and(|outcome| outcome.commit == CommitState::Unknown) {
            return Err(AppError::Conflict("上传发布结果未确认，保留待核查".into()));
        }
        Ok(outcome.is_some_and(|outcome| outcome.commit == CommitState::Committed))
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
            self.remove_receipt(&path).await?;
        }
        Ok(())
    }

    pub(crate) async fn prune_receipt(&self, operation_id: &str) -> AppResult<()> {
        let path = self.receipt_path(operation_id)?;
        if self.read_receipt(operation_id).await?.is_none() {
            return Ok(());
        }
        self.remove_receipt(&path).await
    }

    async fn remove_receipt(&self, path: &Path) -> AppResult<()> {
        self.rooted_remove_file(path).await?;
        self.receipt_count.fetch_sub(1, Ordering::Relaxed);
        self.rooted_sync_parent(path).await
    }
}

#[cfg(test)]
mod tests {
    use super::TransactionPaths;
    use crate::test_support::TestDirectory;
    use std::{collections::HashSet, sync::atomic::Ordering};

    #[tokio::test]
    async fn published_receipt_keeps_its_slot_when_directory_sync_fails() {
        let root = TestDirectory::new("receipt-published-sync-failure");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let operation = "dddddddddddddddddddddddddddddddd";
        paths.metadata_sync_failure.store(true, Ordering::Relaxed);
        assert!(paths
            .ensure_receipt(
                operation,
                "file.txt",
                3,
                crate::error::CommitState::Committed
            )
            .await
            .is_err());
        assert_eq!(paths.receipt_count.load(Ordering::Relaxed), 1);
        paths.metadata_sync_failure.store(false, Ordering::Relaxed);
        paths.prune_receipt(operation).await.unwrap();
        assert_eq!(paths.receipt_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn unconfirmed_receipts_remain_unknown_until_publication_is_proven() {
        let root = TestDirectory::new("receipt-confirmation-upgrade");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let operation = "cccccccccccccccccccccccccccccccc";
        paths
            .ensure_receipt(operation, "file.txt", 3, crate::error::CommitState::Unknown)
            .await
            .unwrap();
        assert_eq!(
            paths
                .receipt_outcome("file.txt", 3, operation)
                .await
                .unwrap()
                .unwrap()
                .commit,
            crate::error::CommitState::Unknown
        );
        paths
            .ensure_receipt(
                operation,
                "file.txt",
                3,
                crate::error::CommitState::Committed,
            )
            .await
            .unwrap();
        assert_eq!(
            paths
                .receipt_outcome("file.txt", 3, operation)
                .await
                .unwrap()
                .unwrap()
                .commit,
            crate::error::CommitState::Committed
        );
        assert_eq!(paths.receipt_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn initialization_counts_durable_receipts_and_pruning_keeps_retained_results() {
        let root = TestDirectory::new("receipt-inventory");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let retained = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let expired = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        paths
            .ensure_receipt(
                retained,
                "first.txt",
                3,
                crate::error::CommitState::Committed,
            )
            .await
            .unwrap();
        paths
            .ensure_receipt(
                expired,
                "second.txt",
                4,
                crate::error::CommitState::Committed,
            )
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
