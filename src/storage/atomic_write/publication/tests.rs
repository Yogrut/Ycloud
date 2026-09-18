use super::*;
use crate::{
    storage::StorageService,
    storage_transaction::{TransactionId, TransactionPaths, UploadOwnership},
    test_support::TestDirectory,
};
use bytes::Bytes;
use std::{sync::Arc, time::Duration};
use tokio::fs;

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("upload publication settles within test deadline");
}

async fn close(storage: StorageService) {
    let paths = storage.transactions.clone();
    drop(storage);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[tokio::test]
async fn cancelled_waiter_keeps_publication_budgets_and_persists_quota() {
    let fixture = TestDirectory::new("publication-cancel");
    let root = fixture.path().join("files");
    let ledger = fixture.path().join("capacity.json");
    let storage = StorageService::new(root.clone(), 16, 1, 100, 0)
        .await
        .unwrap();
    let capacity = CapacityTracker::new_with_ledger(Some(4), 0, true, Some(ledger.clone()));
    let reservation = capacity.reserve_replacement(0, 4).unwrap();
    let mut writer = storage.begin_atomic_write("notes.txt").await.unwrap();
    writer
        .write_chunk(&Bytes::from_static(b"note"))
        .await
        .unwrap();
    let physical = *storage.reserved_upload_bytes.lock().unwrap();
    assert!(physical > 0);
    let mutation = storage.mutation_gate.lock().await;
    assert!(tokio::time::timeout(
        Duration::from_millis(30),
        writer.commit_with_capacity(reservation)
    )
    .await
    .is_err());
    assert_eq!(storage.io_gate.available_permits(), 0);
    assert_eq!(capacity.status().reserved, 4);
    assert_eq!(capacity.status().used, 0);
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), physical);
    assert!(!root.join("notes.txt").exists());
    drop(mutation);
    wait_until(|| storage.io_gate.available_permits() == 1).await;
    assert_eq!(fs::read(root.join("notes.txt")).await.unwrap(), b"note");
    assert_eq!(capacity.status().used, 4);
    assert_eq!(capacity.status().reserved, 0);
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
    assert_eq!(
        crate::capacity::load_capacity_ledger(&ledger)
            .await
            .unwrap(),
        Some(4)
    );
    close(storage).await;
}

#[tokio::test]
async fn incomplete_body_does_not_publish_or_charge_capacity() {
    let fixture = TestDirectory::new("publication-short-body");
    let storage = StorageService::new(fixture.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    let capacity = CapacityTracker::new(Some(4), 0);
    let reservation = capacity.reserve_replacement(0, 4).unwrap();
    let mut writer = storage
        .begin_atomic_write_with_expected("notes.txt", 4)
        .await
        .unwrap();
    writer
        .write_chunk(&Bytes::from_static(b"no"))
        .await
        .unwrap();
    assert!(writer.commit_with_capacity(reservation).await.is_err());
    wait_until(|| storage.upload_cleanup_status().pending_uploads == 0).await;
    assert!(!fixture.path().join("notes.txt").exists());
    assert_eq!(capacity.status().used, 0);
    assert_eq!(capacity.status().reserved, 0);
    assert!(capacity.status().accurate);
    assert_eq!(
        std::fs::read_dir(&storage.transactions.journals)
            .unwrap()
            .count(),
        0
    );
    close(storage).await;
}

#[tokio::test]
async fn journal_io_failure_retains_recovery_resources_and_uncertain_capacity() {
    let fixture = TestDirectory::new("publication-journal-error");
    let storage = StorageService::new(fixture.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    fs::write(fixture.path().join("notes.txt"), b"old")
        .await
        .unwrap();
    let capacity = CapacityTracker::new(Some(8), 3);
    let reservation = capacity.reserve_replacement(3, 4).unwrap();
    let mut writer = storage
        .begin_atomic_write_with_expected("notes.txt", 4)
        .await
        .unwrap();
    writer
        .write_chunk(&Bytes::from_static(b"note"))
        .await
        .unwrap();
    let temporary = writer.temporary.clone();
    let journals = storage.transactions.journals.clone();
    // Only this fixture's empty directory: ordinary unavailable-directory I/O.
    std::fs::remove_dir(&journals).unwrap();
    assert!(writer.commit_with_capacity(reservation).await.is_err());
    assert_eq!(fs::read(&temporary).await.unwrap(), b"note");
    assert_eq!(
        fs::read(fixture.path().join("notes.txt")).await.unwrap(),
        b"old"
    );
    assert_eq!(storage.upload_cleanup_status().pending_uploads, 0);
    assert_eq!(capacity.status().reserved, 0);
    assert!(!capacity.status().accurate);
    assert!(capacity.reserve_replacement(0, 1).is_err());
    std::fs::create_dir(&journals).unwrap();
    close(storage).await;
    TransactionPaths::initialize(fixture.path()).await.unwrap();
    assert!(!temporary.exists());
    assert_eq!(
        fs::read(fixture.path().join("notes.txt")).await.unwrap(),
        b"old"
    );
}

#[tokio::test]
async fn quota_is_rechecked_against_the_current_replacement_size() {
    let fixture = TestDirectory::new("publication-current-quota");
    let storage = StorageService::new(fixture.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    fs::write(fixture.path().join("notes.txt"), b"original")
        .await
        .unwrap();
    let capacity = CapacityTracker::new(Some(10), 8);
    let reservation = capacity.reserve_replacement(8, 5).unwrap();
    let mut writer = storage
        .begin_atomic_write_with_expected("notes.txt", 5)
        .await
        .unwrap();
    writer
        .write_chunk(&Bytes::from_static(b"draft"))
        .await
        .unwrap();
    // Represent two ordinary completed changes while this upload was receiving.
    fs::write(fixture.path().join("notes.txt"), b"hi")
        .await
        .unwrap();
    fs::write(fixture.path().join("other.txt"), b"second")
        .await
        .unwrap();
    capacity.reconcile(8);
    let error = writer.commit_with_capacity(reservation).await.unwrap_err();
    assert_eq!(error.code(), "insufficient_storage");
    assert_eq!(error.operation().unwrap().commit, CommitState::NotCommitted);
    wait_until(|| storage.upload_cleanup_status().pending_uploads == 0).await;
    assert_eq!(
        fs::read(fixture.path().join("notes.txt")).await.unwrap(),
        b"hi"
    );
    assert_eq!(capacity.status().used, 8);
    assert_eq!(capacity.status().reserved, 0);
    assert!(capacity.status().accurate);
    assert_eq!(
        std::fs::read_dir(&storage.transactions.journals)
            .unwrap()
            .count(),
        0
    );
    close(storage).await;
}

struct CheckPublication {
    accounting: PublicationAccounting,
    paths: Arc<TransactionPaths>,
    checked: bool,
}

impl ReplacementObserver for CheckPublication {
    fn prepare(&mut self, previous: u64) -> AppResult<()> {
        self.accounting.prepare(previous)
    }
    fn recovery_owned(&mut self) {
        self.accounting.recovery_owned();
    }
    fn published(&mut self, previous: u64) {
        self.accounting.published(previous);
        assert_eq!(self.accounting.capacity.as_ref().unwrap().status().used, 4);
        assert_eq!(std::fs::read_dir(&self.paths.backups).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&self.paths.journals).unwrap().count(), 1);
        self.checked = true;
    }
}

#[tokio::test]
async fn published_capacity_is_updated_before_backup_and_journal_cleanup() {
    let fixture = TestDirectory::new("publication-accounting-boundary");
    let paths = Arc::new(TransactionPaths::initialize(fixture.path()).await.unwrap());
    let destination = fixture.path().join("notes.txt");
    fs::write(&destination, b"old").await.unwrap();
    let temporary = paths.upload_path(&TransactionId::new());
    fs::write(&temporary, b"note").await.unwrap();
    let capacity = CapacityTracker::new(Some(4), 3);
    let reservation = capacity.reserve_replacement(3, 4).unwrap();
    let mut observer = CheckPublication {
        accounting: PublicationAccounting::new(Some(reservation), 4),
        paths: paths.clone(),
        checked: false,
    };
    let result = paths
        .commit_file(
            "notes.txt",
            &temporary,
            &destination,
            &mut UploadOwnership::Writer,
            &mut observer,
        )
        .await
        .unwrap();
    assert_eq!(result, 3);
    assert!(observer.checked);
    drop(observer);
    assert_eq!(capacity.status().used, 4);
    assert_eq!(capacity.status().reserved, 0);
    assert!(!capacity.status().accurate);
    assert_eq!(fs::read(&destination).await.unwrap(), b"note");
}
