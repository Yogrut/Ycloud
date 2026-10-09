//! COPY/MOVE destination replacement uses the existing publication journal.
use std::path::{Path, PathBuf};

use super::{
    resource_id, ReplaceJournal, ReplacementObserver, TransactionId, TransactionPaths,
    UploadOwnership,
};
use crate::error::{AppError, AppResult};

impl TransactionPaths {
    pub(super) fn replacement_source(&self, journal: &ReplaceJournal) -> PathBuf {
        if let Some(source) = &journal.transfer_source {
            self.root.join(source)
        } else if journal.staged_copy {
            self.copy_path(&journal.id)
        } else {
            self.upload_path(&journal.id)
        }
    }

    pub(crate) async fn commit_transfer(
        &self,
        source: &Path,
        destination: &str,
        overwrite: bool,
        ownership: &mut UploadOwnership,
        observer: &mut impl ReplacementObserver,
        previous_size: u64,
    ) -> AppResult<bool> {
        self.settle_publication().await?;
        if self.rooted_metadata(source).await?.is_none() {
            return Err(AppError::NotFound);
        }
        self.validate_destination_kind(destination, true).await?;
        let target = self.root.join(destination);
        let existing = self.rooted_metadata(&target).await?;
        if !overwrite && existing.is_some() {
            return Err(AppError::PreconditionFailed);
        }
        let staged_copy = source.parent() == Some(self.copies.as_path());
        let id = if staged_copy {
            resource_id(source)?
        } else {
            TransactionId::new()
        };
        let mut journal = ReplaceJournal::new(id, destination.to_owned())?;
        journal.version = 4;
        journal.staged_copy = staged_copy;
        if !staged_copy {
            journal.transfer_source = Some(
                source
                    .strip_prefix(&self.root)
                    .map_err(|_| AppError::Forbidden)?
                    .to_str()
                    .ok_or(AppError::Forbidden)?
                    .replace('\\', "/"),
            );
        }
        journal.replaced_bytes = previous_size;
        journal.validate()?;
        observer.prepare(previous_size)?;
        *ownership = UploadOwnership::Recovery;
        observer.recovery_owned();
        let result = self
            .publish_replacement(
                journal,
                source,
                &target,
                existing.is_some(),
                previous_size,
                observer,
            )
            .await;
        if result.as_ref().is_err_and(|error| {
            error
                .operation()
                .is_some_and(|outcome| outcome.commit == crate::error::CommitState::NotCommitted)
        }) {
            *ownership = UploadOwnership::Writer;
        }
        result?;
        Ok(existing.is_none())
    }

    pub(super) async fn publish_replacement(
        &self,
        journal: ReplaceJournal,
        temporary: &Path,
        destination: &Path,
        replacing: bool,
        previous_size: u64,
        observer: &mut impl ReplacementObserver,
    ) -> AppResult<u64> {
        let journal_path = self.journals.join(format!("{}.json", journal.id));
        self.write_json_atomic(&journal_path, &journal).await?;
        let backup = self.backups.join(&journal.id);
        if replacing {
            self.rooted_rename_noreplace(destination, &backup).await?;
        }
        if let Err(error) = self.rooted_rename_noreplace(temporary, destination).await {
            let restored = if self.rooted_exists(&backup).await? {
                match self.rooted_rename_noreplace(&backup, destination).await {
                    Ok(()) => true,
                    Err(restore_error) => {
                        tracing::warn!(transaction_id = %journal.id, error = %restore_error, "replacement rollback pending; journal retained");
                        false
                    }
                }
            } else {
                !replacing && !self.rooted_exists(destination).await?
            };
            if restored && self.rooted_exists(temporary).await? {
                self.rooted_sync_parent(destination).await?;
                self.rooted_sync_parent(&backup).await?;
                self.rooted_sync_parent(temporary).await?;
                if journal.anchored {
                    let anchor = self.anchors.join(&journal.id);
                    self.rooted_remove_file_idempotent(&anchor).await?;
                    self.rooted_sync_parent(&anchor).await?;
                }
                self.rooted_remove_file(&journal_path).await?;
                self.rooted_sync_parent(&journal_path).await?;
                observer.rollback_completed();
                return Err(error.with_operation(
                    crate::error::CommitState::NotCommitted,
                    crate::error::CleanupState::Pending,
                ));
            }
            return Err(error);
        }
        observer.published(previous_size);
        *self
            .pending_publication
            .lock()
            .expect("publication mutex poisoned") = Some(journal.clone());
        self.settle_publication().await?;
        if let (Some(operation_id), Some(size)) = (&journal.operation_id, journal.operation_size) {
            self.ensure_receipt(operation_id, &journal.destination, size, true)
                .await?;
        }
        self.cleanup_replacement_backup(&backup, previous_size)
            .await?;
        if journal.anchored {
            let anchor = self.anchors.join(&journal.id);
            self.rooted_remove_file_idempotent(&anchor).await?;
            self.rooted_sync_parent(&anchor).await?;
        }
        self.rooted_remove_file(&journal_path).await?;
        self.rooted_sync_parent(&journal_path).await?;
        Ok(previous_size)
    }

    pub(super) async fn cleanup_replacement_backup(
        &self,
        backup: &Path,
        bytes: u64,
    ) -> AppResult<()> {
        let Some(metadata) = self.rooted_metadata(backup).await? else {
            return Ok(());
        };
        if metadata.is_dir() {
            self.stage_delete(backup, bytes, &mut ()).await?;
        } else {
            self.rooted_remove_file(backup).await?;
            self.rooted_sync_parent(backup).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn recovery_restores_unpublished_directory_targets_and_keeps_move_sources() {
        for moving in [false, true] {
            let root = TestDirectory::new("transfer-unpublished-recovery");
            let paths = TransactionPaths::initialize(root.path()).await.unwrap();
            let id = TransactionId::new();
            let source = if moving {
                root.path().join("source")
            } else {
                paths.copy_path(&id)
            };
            tokio::fs::create_dir(&source).await.unwrap();
            tokio::fs::write(source.join("new.txt"), b"new")
                .await
                .unwrap();
            let target = root.path().join("target");
            tokio::fs::create_dir(&target).await.unwrap();
            tokio::fs::write(target.join("previous.txt"), b"previous")
                .await
                .unwrap();
            tokio::fs::rename(&target, paths.backups.join(&id))
                .await
                .unwrap();
            let mut journal = ReplaceJournal::new(id.clone(), "target".into()).unwrap();
            journal.version = 4;
            journal.transfer_source = moving.then(|| "source".to_owned());
            journal.staged_copy = !moving;
            journal.replaced_bytes = 8;
            paths
                .write_json_atomic(&paths.journals.join(format!("{id}.json")), &journal)
                .await
                .unwrap();
            drop(paths);
            let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
            assert_eq!(
                tokio::fs::read(target.join("previous.txt")).await.unwrap(),
                b"previous"
            );
            assert!(!target.join("new.txt").exists());
            if moving {
                assert_eq!(
                    tokio::fs::read(source.join("new.txt")).await.unwrap(),
                    b"new"
                );
            }
            assert_eq!(std::fs::read_dir(&recovered.journals).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn published_transfer_recovery_cleans_only_old_backup_not_new_source_or_target() {
        let root = TestDirectory::new("transfer-published-recovery");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let backup = paths.backups.join(&id);
        tokio::fs::create_dir(&backup).await.unwrap();
        tokio::fs::write(backup.join("old.txt"), b"old")
            .await
            .unwrap();
        tokio::fs::write(root.path().join("target"), b"committed")
            .await
            .unwrap();
        tokio::fs::write(root.path().join("source"), b"later source")
            .await
            .unwrap();
        let mut journal = ReplaceJournal::new(id.clone(), "target".into()).unwrap();
        journal.version = 4;
        journal.transfer_source = Some("source".into());
        journal.published = true;
        journal.replaced_bytes = 3;
        paths
            .write_json_atomic(&paths.journals.join(format!("{id}.json")), &journal)
            .await
            .unwrap();
        drop(paths);
        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.path().join("target")).await.unwrap(),
            b"committed"
        );
        assert_eq!(
            tokio::fs::read(root.path().join("source")).await.unwrap(),
            b"later source"
        );
        assert!(!backup.exists());
        assert_eq!(std::fs::read_dir(&recovered.journals).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn failed_move_rolls_back_and_releases_its_journal_before_retry() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = TestDirectory::new("transfer-rollback-retry");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let source = root.path().join("source");
        tokio::fs::write(&source, b"new").await.unwrap();
        tokio::fs::write(root.path().join("target"), b"previous")
            .await
            .unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&source)
            .unwrap();
        let mut ownership = UploadOwnership::Writer;
        let error = paths
            .commit_transfer(&source, "target", true, &mut ownership, &mut (), 8)
            .await
            .unwrap_err();
        assert_eq!(
            error.operation().unwrap().commit,
            crate::error::CommitState::NotCommitted
        );
        assert_eq!(ownership, UploadOwnership::Writer);
        assert_eq!(std::fs::read_dir(&paths.journals).unwrap().count(), 0);
        assert_eq!(
            tokio::fs::read(root.path().join("target")).await.unwrap(),
            b"previous"
        );
        drop(locked);
        assert!(!paths
            .commit_transfer(&source, "target", true, &mut ownership, &mut (), 8)
            .await
            .unwrap());
        assert_eq!(
            tokio::fs::read(root.path().join("target")).await.unwrap(),
            b"new"
        );
        assert!(!source.exists());
    }
}
