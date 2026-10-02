//! Local transaction recovery: validate a bounded inventory before mutating it.
//! Callers retain exclusive ownership. Formal recovery completes before one bounded
//! staging-cleanup pass; remaining trees stay on disk for the existing cleanup worker.
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use tokio::{fs, io::AsyncReadExt};

use super::{
    metadata::{MAX_JOURNAL_BYTES, MAX_RECOVERY_ENTRIES},
    resource_id, DeletionJournal, InventoryKind, ReplaceJournal, TransactionId, TransactionPaths,
    DELETION_NODES_PER_PASS,
};
use crate::error::{AppError, AppResult};

// Owned only by one recovery attempt. Validation is not a commit proof:
// destination and publication identity must still be inspected during execution.
struct RecoveryInventory {
    journals: Vec<(PathBuf, ReplaceJournal)>,
    pending_files: Vec<PathBuf>,
    uploads: Vec<PathBuf>,
    copies: Vec<PathBuf>,
    trash: Vec<PathBuf>,
    anchors: Vec<PathBuf>,
    deletion_records: HashMap<TransactionId, (PathBuf, DeletionJournal)>,
    pending_deletion_records: Vec<PathBuf>,
}

impl TransactionPaths {
    pub(crate) async fn recover(&self, root: &Path) -> AppResult<()> {
        self.settle_publication().await?;
        let RecoveryInventory {
            journals,
            pending_files,
            uploads,
            copies,
            mut trash,
            anchors,
            mut deletion_records,
            pending_deletion_records,
        } = self.read_recovery_inventory().await?;
        for (path, journal) in journals {
            self.recover_replacement(root, path, journal).await?;
        }
        for path in pending_files.into_iter().chain(uploads) {
            self.rooted_remove_file_idempotent(&path).await?;
            self.rooted_sync_parent(&path).await?;
        }
        for path in anchors {
            if !self.rooted_exists(&path).await? {
                continue;
            }
            self.rooted_remove_file(&path).await?;
            self.rooted_sync_parent(&path).await?;
        }
        for path in copies {
            if let Some(staged) = self.handoff_recovered_copy(&path).await? {
                trash.push(staged);
            }
        }
        let mut remaining = DELETION_NODES_PER_PASS;
        let mut retained_debt = HashSet::new();
        for path in trash {
            let id = resource_id(&path)?;
            if remaining == 0 {
                retained_debt.insert(id);
                continue;
            }
            let progress = self.remove_bounded(&path, remaining).await?;
            remaining = remaining.saturating_sub(progress.removed_nodes);
            if !progress.complete {
                retained_debt.insert(id);
                continue;
            }
            self.rooted_sync_parent(&path).await?;
            if let Some((record_path, _)) = deletion_records.remove(&id) {
                self.rooted_remove_file_idempotent(&record_path).await?;
                self.rooted_sync_parent(&record_path).await?;
            }
        }
        for (id, (record_path, _)) in deletion_records {
            // A trash page is not an absence proof for unmatched debt.
            let staged = self.trash.join(&id);
            if !retained_debt.contains(&id) && !self.rooted_exists(&staged).await? {
                self.rooted_sync_parent(&staged).await?;
                self.rooted_remove_file_idempotent(&record_path).await?;
                self.rooted_sync_parent(&record_path).await?;
            }
        }
        for path in pending_deletion_records {
            let id = resource_id(&path.with_extension(""))?;
            let staged = self.trash.join(&id);
            if !retained_debt.contains(&id) && !self.rooted_exists(&staged).await? {
                self.rooted_sync_parent(&staged).await?;
                self.rooted_remove_file_idempotent(&path).await?;
                self.rooted_sync_parent(&path).await?;
            }
        }
        Ok(())
    }

    /// Validate all bounded formal records and one trash page before mutation.
    /// In-memory pending publication is settled separately beforehand.
    async fn read_recovery_inventory(&self) -> AppResult<RecoveryInventory> {
        let journal_files = self
            .inventory(&self.journals, InventoryKind::Journal)
            .await?;
        let uploads = self.inventory(&self.uploads, InventoryKind::File).await?;
        let copies = self.inventory(&self.copies, InventoryKind::Tree).await?;
        // Copy handoff can enlarge trash beyond one inventory page. It contains
        // cleanup-only resources, so defer later pages instead of blocking restart.
        let (trash, _) = self
            .inventory_page(&self.trash, InventoryKind::Tree, MAX_RECOVERY_ENTRIES)
            .await?;
        let backups = self.inventory(&self.backups, InventoryKind::File).await?;
        let anchors = self.inventory(&self.anchors, InventoryKind::File).await?;
        let deletion_files = self
            .inventory(&self.deletions, InventoryKind::Journal)
            .await?;
        let mut journals = Vec::new();
        let mut pending_files = Vec::new();
        let mut journal_anchors = HashMap::new();
        let mut destinations = HashSet::new();
        let mut deletion_records = HashMap::new();
        let mut pending_deletion_records = Vec::new();
        for path in journal_files {
            if path.extension().and_then(|ext| ext.to_str()) == Some("tmp") {
                pending_files.push(path);
                continue;
            }
            let journal = self.read_journal(&path).await.inspect_err(|_| {
                // Inventory has already validated this filename; do not log
                // journal contents or user destination paths.
                if let Ok(id) = resource_id(&path.with_extension("")) {
                    tracing::warn!(transaction_id = %id, "journal validation failed; recovery stopped with resources retained");
                }
            })?;
            if !journal.published {
                self.validate_destination(&journal.destination).await?;
            }
            if journal_anchors
                .insert(journal.id.clone(), journal.anchored)
                .is_some()
                || (!journal.published && !destinations.insert(journal.destination.clone()))
            {
                return Err(AppError::Conflict(
                    "Ambiguous transaction journals; recovery stopped".into(),
                ));
            }
            journals.push((path, journal));
        }
        for path in deletion_files {
            if path.extension().and_then(|ext| ext.to_str()) == Some("tmp") {
                pending_deletion_records.push(path);
                continue;
            }
            let record = self.read_deletion_journal(&path).await?;
            if deletion_records
                .insert(record.id.clone(), (path, record))
                .is_some()
            {
                return Err(AppError::Conflict(
                    "Ambiguous deletion debt records; recovery stopped".into(),
                ));
            }
        }
        for path in &backups {
            if !journal_anchors.contains_key(&resource_id(path)?) {
                return Err(AppError::Conflict(
                    "Unclaimed replacement backup; recovery stopped".into(),
                ));
            }
        }
        for path in &anchors {
            let id = resource_id(path)?;
            // A journal-owned anchor must be declared by that journal. An
            // orphan with no journal remains eligible for staging cleanup.
            if journal_anchors.get(&id) == Some(&false) {
                return Err(AppError::Conflict(
                    "Unclaimed upload anchor; recovery stopped".into(),
                ));
            }
        }
        let trash_ids = trash
            .iter()
            .map(|path| resource_id(path))
            .collect::<AppResult<HashSet<_>>>()?;
        let incomplete_deletion_ids = pending_deletion_records
            .iter()
            .map(|path| resource_id(&path.with_extension("")))
            .collect::<AppResult<HashSet<_>>>()?;
        for path in &copies {
            let id = resource_id(path)?;
            if trash_ids.contains(&id)
                || deletion_records.contains_key(&id)
                || incomplete_deletion_ids.contains(&id)
                || self.rooted_exists(&self.trash.join(&id)).await?
            {
                return Err(AppError::Conflict(
                    "Ambiguous copy cleanup ownership; recovery stopped".into(),
                ));
            }
        }
        Ok(RecoveryInventory {
            journals,
            pending_files,
            uploads,
            copies,
            trash,
            anchors,
            deletion_records,
            pending_deletion_records,
        })
    }

    async fn recover_replacement(
        &self,
        root: &Path,
        path: PathBuf,
        journal: ReplaceJournal,
    ) -> AppResult<()> {
        let destination = root.join(&journal.destination);
        let backup = self.backups.join(&journal.id);
        let upload = self.upload_path(&journal.id);
        let anchor = self.anchors.join(&journal.id);
        // Never convert inspection failures into "the destination is absent".
        let destination_exists = if journal.published {
            false
        } else {
            self.rooted_exists(&destination).await?
        };
        let backup_exists = self.rooted_exists(&backup).await?;
        let upload_exists = self.rooted_exists(&upload).await?;
        if !journal.published && upload_exists && backup_exists && destination_exists {
            return Err(AppError::Conflict(
                "Unexpected destination during upload recovery; original backup retained".into(),
            ));
        }
        #[cfg(target_os = "linux")]
        let anchor_exists = self.rooted_exists(&anchor).await?;
        #[cfg(target_os = "linux")]
        let anchored_publication = if journal.anchored
            && !journal.published
            && anchor_exists
            && destination_exists
            && !upload_exists
        {
            use std::os::unix::fs::MetadataExt;
            let anchored = self.rooted_metadata(&anchor).await?.ok_or_else(|| {
                AppError::Conflict("Upload anchor disappeared during recovery".into())
            })?;
            let destination = self.rooted_metadata(&destination).await?.ok_or_else(|| {
                AppError::Conflict("Upload destination disappeared during recovery".into())
            })?;
            anchored.dev() == destination.dev() && anchored.ino() == destination.ino()
        } else {
            false
        };
        #[cfg(not(target_os = "linux"))]
        let anchored_publication = false;
        if journal.anchored
            && !journal.published
            && destination_exists
            && !upload_exists
            && !anchored_publication
        {
            return Err(AppError::Conflict(
                "Upload publication identity cannot be verified; resources retained".into(),
            ));
        }
        let was_committed = journal.published || (destination_exists && !upload_exists);
        if was_committed {
            if let (Some(operation_id), Some(size)) =
                (&journal.operation_id, journal.operation_size)
            {
                self.rooted_sync_parent(&destination).await?;
                self.ensure_receipt(
                    operation_id,
                    &journal.destination,
                    size,
                    journal.published || anchored_publication,
                )
                .await?;
            }
        }
        if journal.version < 2
            && !destination_exists
            && backup_exists
            && !self.rooted_exists(&upload).await?
        {
            return Err(AppError::Conflict(
                "旧版替换记录缺少发布证明；已保留备份，不自动恢复旧文件".into(),
            ));
        }
        if !destination_exists && backup_exists && !journal.published {
            self.rooted_rename_noreplace(&backup, &destination).await?;
            self.rooted_sync_parent(&destination).await?;
            self.rooted_sync_parent(&backup).await?;
        } else if backup_exists {
            self.rooted_remove_file_idempotent(&backup).await?;
            self.rooted_sync_parent(&backup).await?;
        }
        // The journal remains the recovery owner until cleanup really succeeds.
        self.rooted_remove_file_idempotent(&upload).await?;
        self.rooted_sync_parent(&upload).await?;
        if journal.anchored {
            self.rooted_remove_file_idempotent(&anchor).await?;
            self.rooted_sync_parent(&anchor).await?;
        }
        self.rooted_remove_file_idempotent(&path).await?;
        self.rooted_sync_parent(&path).await?;
        Ok(())
    }

    // Only the validated, exclusively recovered copy inventory is handed off.
    // The online worker must never scan copies and adopt an active copy itself.
    // A rename keeps ownership restart-visible without walking the tree or
    // inventing a byte debt for an unpublished copy of unknown physical size.
    async fn handoff_recovered_copy(&self, path: &Path) -> AppResult<Option<PathBuf>> {
        if !self.rooted_exists(path).await? {
            return Ok(None);
        }
        let staged = self.trash.join(resource_id(path)?);
        self.rooted_rename_noreplace(path, &staged).await?;
        self.rooted_sync_parent(path).await?;
        self.rooted_sync_parent(&staged).await?;
        Ok(Some(staged))
    }

    async fn read_journal(&self, path: &Path) -> AppResult<ReplaceJournal> {
        #[cfg(target_os = "linux")]
        let file = self
            .linux_root
            .open_file_for_read(&self.rooted_relative(path)?)
            .await?;
        #[cfg(not(target_os = "linux"))]
        let file = fs::File::open(path)
            .await
            .map_err(|error| AppError::with_source("failed to open transaction journal", error))?;
        decode_replace_journal(path, file).await
    }
}

async fn decode_replace_journal(path: &Path, file: fs::File) -> AppResult<ReplaceJournal> {
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| AppError::with_source("failed to read transaction journal", error))?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(AppError::Conflict(
            "Transaction journal exceeds its size limit; recovery stopped".into(),
        ));
    }
    let journal: ReplaceJournal = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::with_source("invalid transaction journal; recovery stopped", error)
    })?;
    journal.validate()?;
    if journal.id != resource_id(&path.with_extension(""))? {
        return Err(AppError::Conflict(
            "Transaction filename and identifier disagree; recovery stopped".into(),
        ));
    }
    Ok(journal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn recovery_keeps_large_deletion_debt_after_one_shared_cleanup_budget() {
        let root = TestDirectory::new("recovery-cleanup-budget");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        fs::write(root.path().join("keep.txt"), b"formal file")
            .await
            .unwrap();
        let source = root.path().join("deleted-tree");
        fs::create_dir(&source).await.unwrap();
        for index in 0..DELETION_NODES_PER_PASS + 2 {
            fs::write(source.join(format!("{index}.txt")), b"x")
                .await
                .unwrap();
        }
        let trash = paths
            .stage_delete(&source, (DELETION_NODES_PER_PASS + 2) as u64, &mut ())
            .await
            .unwrap();
        let record = paths
            .deletions
            .join(format!("{}.json", resource_id(&trash).unwrap()));

        paths.recover(root.path()).await.unwrap();
        assert!(
            trash.exists(),
            "one recovery must not drain the whole large tree"
        );
        assert!(
            record.exists(),
            "unfinished deletion must retain durable debt"
        );
        assert_eq!(
            fs::read(root.path().join("keep.txt")).await.unwrap(),
            b"formal file"
        );
    }

    #[tokio::test]
    async fn recovery_shares_one_budget_across_all_trash_entries() {
        let root = TestDirectory::new("recovery-many-trash");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        for _ in 0..DELETION_NODES_PER_PASS + 2 {
            fs::write(paths.trash.join(TransactionId::new()), b"x")
                .await
                .unwrap();
        }
        paths.recover(root.path()).await.unwrap();
        let mut remaining = fs::read_dir(&paths.trash).await.unwrap();
        let mut count = 0;
        while remaining.next_entry().await.unwrap().is_some() {
            count += 1;
        }
        assert_eq!(
            count, 2,
            "the budget applies to the recovery, not each entry"
        );
        paths.recover(root.path()).await.unwrap();
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
    }

    #[tokio::test]
    async fn copy_handoff_over_one_trash_page_does_not_block_restart_or_drop_debt() {
        let root = TestDirectory::new("recovery-paged-trash");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        for _ in 0..MAX_RECOVERY_ENTRIES {
            fs::write(paths.trash.join(TransactionId::new()), b"x")
                .await
                .unwrap();
        }
        for _ in 0..DELETION_NODES_PER_PASS + 1 {
            fs::write(paths.copy_path(&TransactionId::new()), b"x")
                .await
                .unwrap();
        }
        paths.recover(root.path()).await.unwrap();
        // 4096 + 257 - 256 entries remain, exceeding one startup trash page.
        let inventory = paths.read_recovery_inventory().await.unwrap();
        assert_eq!(inventory.trash.len(), MAX_RECOVERY_ENTRIES);
        assert!(inventory.copies.is_empty());
        let page: HashSet<_> = inventory.trash.into_iter().collect();
        let mut entries = fs::read_dir(&paths.trash).await.unwrap();
        let deferred = loop {
            let entry = entries
                .next_entry()
                .await
                .unwrap()
                .expect("deferred trash entry");
            if !page.contains(&entry.path()) {
                break entry.path();
            }
        };
        let id = resource_id(&deferred).unwrap();
        let record = paths.deletions.join(format!("{id}.json"));
        fs::write(
            &record,
            serde_json::to_vec(&DeletionJournal::new(id, 1).unwrap()).unwrap(),
        )
        .await
        .unwrap();
        drop(paths);
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        // Enumeration order need not be stable: debt is retired only if its
        // actual resource is gone, whether or not it appeared in this page.
        assert_eq!(record.exists(), deferred.exists());
        assert!(paths
            .read_recovery_inventory()
            .await
            .unwrap()
            .copies
            .is_empty());
    }

    #[tokio::test]
    async fn recovery_retains_incomplete_debt_until_its_tree_is_removed() {
        let root = TestDirectory::new("recovery-incomplete-debt");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let trash = paths.trash.join(&id);
        let record = paths.deletions.join(format!("{id}.tmp"));
        fs::create_dir(&trash).await.unwrap();
        for index in 0..DELETION_NODES_PER_PASS {
            fs::write(trash.join(format!("{index}.txt")), b"x")
                .await
                .unwrap();
        }
        fs::write(&record, b"incomplete record").await.unwrap();
        paths.recover(root.path()).await.unwrap();
        assert!(trash.exists());
        assert!(record.exists());
        paths.recover(root.path()).await.unwrap();
        assert!(!trash.exists());
        assert!(!record.exists());
    }

    #[tokio::test]
    async fn conflicting_copy_cleanup_ids_stop_before_any_disk_cleanup() {
        for kind in ["trash", "debt", "incomplete-debt"] {
            let root = TestDirectory::new("recovery-copy-collision");
            let paths = TransactionPaths::initialize(root.path()).await.unwrap();
            let id = TransactionId::new();
            let copy = paths.copy_path(&id);
            fs::write(&copy, b"copy data").await.unwrap();
            let conflict = match kind {
                "trash" => paths.trash.join(&id),
                "debt" => paths.deletions.join(format!("{id}.json")),
                _ => paths.deletions.join(format!("{id}.tmp")),
            };
            let contents = if kind == "debt" {
                serde_json::to_vec(&DeletionJournal::new(id, 5).unwrap()).unwrap()
            } else {
                b"other owned data".to_vec()
            };
            fs::write(&conflict, &contents).await.unwrap();
            let unrelated = paths.upload_path(&TransactionId::new());
            fs::write(&unrelated, b"unrelated upload").await.unwrap();
            let error = paths.recover(root.path()).await.unwrap_err();
            assert!(matches!(error, AppError::Conflict(ref message)
                if message == "Ambiguous copy cleanup ownership; recovery stopped"));
            assert_eq!(fs::read(&copy).await.unwrap(), b"copy data");
            assert_eq!(fs::read(&conflict).await.unwrap(), contents);
            assert_eq!(fs::read(&unrelated).await.unwrap(), b"unrelated upload");
        }
    }

    #[tokio::test]
    async fn validated_inventory_is_read_only_and_keeps_published_history_separate() {
        let root = TestDirectory::new("recovery-inventory");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let operation = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let mut pending = ReplaceJournal::new(TransactionId::new(), "report.txt".into()).unwrap();
        pending.anchored = true;
        pending.operation_id = Some(operation.into());
        pending.operation_size = Some(7);
        let mut published = ReplaceJournal::new(TransactionId::new(), "report.txt".into()).unwrap();
        published.published = true;
        for journal in [&pending, &published] {
            fs::write(
                paths.journals.join(format!("{}.json", journal.id)),
                serde_json::to_vec(journal).unwrap(),
            )
            .await
            .unwrap();
        }
        let upload = paths.upload_path(&pending.id);
        let backup = paths.backups.join(&pending.id);
        let owned_anchor = paths.anchors.join(&pending.id);
        let orphan_anchor = paths.anchors.join(TransactionId::new());
        let pending_file = paths.journals.join(format!("{}.tmp", TransactionId::new()));
        let pending_deletion = paths
            .deletions
            .join(format!("{}.tmp", TransactionId::new()));
        for path in [
            &upload,
            &backup,
            &owned_anchor,
            &orphan_anchor,
            &pending_file,
            &pending_deletion,
        ] {
            fs::write(path, b"pending").await.unwrap();
        }
        let copy = paths.copy_path(&TransactionId::new());
        fs::create_dir(&copy).await.unwrap();
        let deletion = DeletionJournal::new(TransactionId::new(), 3).unwrap();
        let trash = paths.trash.join(&deletion.id);
        let deletion_path = paths.deletions.join(format!("{}.json", deletion.id));
        fs::write(&trash, b"old").await.unwrap();
        fs::write(&deletion_path, serde_json::to_vec(&deletion).unwrap())
            .await
            .unwrap();

        let inventory = paths.read_recovery_inventory().await.unwrap();
        // A published historical journal does not reserve the destination of
        // the current pending operation, even when they share the same path.
        assert_eq!(inventory.journals.len(), 2);
        assert!(inventory
            .journals
            .iter()
            .any(|(_, item)| item.id == pending.id && item.anchored));
        assert!(inventory
            .journals
            .iter()
            .any(|(_, item)| item.id == published.id && item.published));
        assert_eq!(inventory.uploads, vec![upload.clone()]);
        assert_eq!(inventory.pending_files, vec![pending_file.clone()]);
        assert_eq!(inventory.copies, vec![copy.clone()]);
        assert_eq!(inventory.trash, vec![trash.clone()]);
        assert_eq!(inventory.anchors.len(), 2);
        assert!(inventory.anchors.contains(&owned_anchor));
        assert!(inventory.anchors.contains(&orphan_anchor));
        assert_eq!(
            inventory.pending_deletion_records,
            vec![pending_deletion.clone()]
        );
        assert_eq!(inventory.deletion_records.len(), 1);
        assert_eq!(inventory.deletion_records[&deletion.id].0, deletion_path);
        assert_eq!(
            inventory.deletion_records[&deletion.id].1.bytes_upper_bound,
            3
        );
        for path in [
            &upload,
            &backup,
            &owned_anchor,
            &orphan_anchor,
            &pending_file,
            &pending_deletion,
        ] {
            assert_eq!(fs::read(path).await.unwrap(), b"pending");
        }
        assert!(copy.exists());
        assert_eq!(fs::read(&trash).await.unwrap(), b"old");
        assert!(deletion_path.exists());
        for (path, _) in &inventory.journals {
            assert!(path.exists());
        }
        assert!(!root.path().join("report.txt").exists());
        assert!(!paths.receipts.join(format!("{operation}.json")).exists());
    }

    #[tokio::test]
    async fn duplicate_pending_destinations_stop_before_cleanup() {
        let root = TestDirectory::new("recovery-duplicate-destination");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let mut resources = Vec::new();
        for _ in 0..2 {
            let journal = ReplaceJournal::new(TransactionId::new(), "report.txt".into()).unwrap();
            let journal_path = paths.journals.join(format!("{}.json", journal.id));
            let upload = paths.upload_path(&journal.id);
            fs::write(&journal_path, serde_json::to_vec(&journal).unwrap())
                .await
                .unwrap();
            fs::write(&upload, b"pending").await.unwrap();
            resources.push((journal_path, upload));
        }
        let error = paths.recover(root.path()).await.unwrap_err();
        assert!(matches!(error, AppError::Conflict(ref message)
            if message == "Ambiguous transaction journals; recovery stopped"));
        for (journal_path, upload) in resources {
            assert!(journal_path.exists());
            assert_eq!(fs::read(upload).await.unwrap(), b"pending");
        }
        assert!(!root.path().join("report.txt").exists());
    }

    #[tokio::test]
    async fn recovery_reclaims_multibatch_trees_across_bounded_passes() {
        let root = TestDirectory::new("recovery-multibatch-trees");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let copy = paths.copy_path(&TransactionId::new());
        let source = root.path().join("old-tree");
        for directory in [&copy, &source] {
            fs::create_dir(directory).await.unwrap();
            for index in 0..DELETION_NODES_PER_PASS + 2 {
                fs::write(directory.join(format!("{index}.txt")), b"x")
                    .await
                    .unwrap();
            }
        }
        let trash = paths
            .stage_delete(&source, (DELETION_NODES_PER_PASS + 2) as u64, &mut ())
            .await
            .unwrap();
        let deletion_path = paths
            .deletions
            .join(format!("{}.json", resource_id(&trash).unwrap()));
        assert!(deletion_path.exists());

        paths.recover(root.path()).await.unwrap();
        assert!(!copy.exists());
        assert!(paths.trash.join(resource_id(&copy).unwrap()).exists());
        assert!(trash.exists());
        assert!(!source.exists());
        assert!(deletion_path.exists());
        // Two 259-node trees require three aggregate 256-node passes.
        for _ in 0..2 {
            paths.recover(root.path()).await.unwrap();
        }
        assert!(!trash.exists());
        assert!(!deletion_path.exists());
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
        paths.recover(root.path()).await.unwrap();
    }
}
