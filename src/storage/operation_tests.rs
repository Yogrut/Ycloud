use super::*;
use crate::{
    capacity::CapacityTracker,
    error::{CleanupState, CommitState},
    storage_backend::StorageBackend,
    test_support::TestDirectory,
};
use axum::body::Body;
use std::time::Duration;

#[tokio::test]
async fn cancelled_delete_waiter_does_not_interrupt_deletion_or_capacity_update() {
    let fixture = TestDirectory::new("delete-result-cancel");
    let root = fixture.path().join("files");
    let storage = StorageService::new(root.clone(), 100, 1, 100, 0)
        .await
        .unwrap();
    fs::write(root.join("old.txt"), b"old").await.unwrap();
    let backend = StorageBackend::local_configured(
        storage.clone(),
        Some(100),
        fixture.path().join("usage.json"),
    )
    .await
    .unwrap();
    let mutation = storage.mutation_gate.lock().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), backend.remove("old.txt"))
            .await
            .is_err()
    );
    assert!(root.join("old.txt").exists());
    assert_eq!(backend.capacity_status().used, 3);
    drop(mutation);
    tokio::time::timeout(Duration::from_secs(4), async {
        while storage.io_gate.available_permits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!root.join("old.txt").exists());
    assert_eq!(backend.capacity_status().used, 0);
    let paths = storage.transactions.clone();
    drop(backend);
    drop(storage);
    tokio::time::timeout(Duration::from_secs(4), async {
        while Arc::strong_count(&paths) > 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn deletion_record_failure_is_definitely_uncommitted_and_keeps_capacity_accurate() {
    let fixture = TestDirectory::new("delete-record-not-committed");
    let storage = StorageService::new(fixture.path().into(), 100, 1, 100, 0)
        .await
        .unwrap();
    fs::write(fixture.path().join("old.txt"), b"old")
        .await
        .unwrap();
    let path = storage.resolve_existing("old.txt").await.unwrap();
    let capacity = CapacityTracker::new(Some(100), 3);
    let records = storage.transactions.deletions.clone();
    fs::remove_dir(&records).await.unwrap();
    fs::write(&records, b"temporarily unavailable")
        .await
        .unwrap();

    let error = storage
        .remove_with_capacity(&path, Some(capacity.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.operation().unwrap().commit, CommitState::NotCommitted);
    assert_eq!(
        fs::read(fixture.path().join("old.txt")).await.unwrap(),
        b"old"
    );
    assert_eq!(capacity.status().used, 3);
    assert!(capacity.status().accurate);

    fs::remove_file(&records).await.unwrap();
    fs::create_dir(&records).await.unwrap();
    let paths = storage.transactions.clone();
    drop(storage);
    tokio::time::timeout(Duration::from_secs(4), async {
        while Arc::strong_count(&paths) > 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn published_delete_without_ledger_settlement_marks_capacity_uncertain_on_drop() {
    let fixture = TestDirectory::new("delete-unsettled-ledger");
    let storage = StorageService::new(fixture.path().into(), 100, 1, 100, 0)
        .await
        .unwrap();
    let source = fixture.path().join("old.txt");
    fs::write(&source, b"old").await.unwrap();
    let capacity = CapacityTracker::new(Some(100), 3);
    let mut accounting = DeleteAccounting {
        capacity: Some(capacity.clone()),
        cleanup: storage.cleanup.clone(),
        removed_size: 3,
        publication_started: false,
        was_published: false,
        ledger_settled: false,
    };
    let trash = storage
        .transactions
        .stage_delete(&source, 3, &mut accounting)
        .await
        .unwrap();
    assert_eq!(capacity.status().used, 0);
    assert!(capacity.status().accurate);
    drop(accounting);
    assert!(!capacity.status().accurate);

    storage.cleanup.notify();
    tokio::time::timeout(Duration::from_secs(4), async {
        while trash.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let paths = storage.transactions.clone();
    drop(storage);
    tokio::time::timeout(Duration::from_secs(4), async {
        while Arc::strong_count(&paths) > 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn committed_upload_copy_delete_report_ledger_failure_without_repeating_mutations() {
    for operation in ["upload", "copy", "delete"] {
        let fixture = TestDirectory::new("committed-operation-ledger");
        let root = fixture.path().join("files");
        let ledger_dir = fixture.path().join("ledger");
        let ledger = ledger_dir.join("state.json");
        let storage = StorageService::new(root.clone(), 100, 1, 100, 0)
            .await
            .unwrap();
        fs::write(root.join("old.txt"), b"old").await.unwrap();
        let backend = StorageBackend::local_configured(storage.clone(), Some(100), ledger.clone())
            .await
            .unwrap();
        assert!(backend.capacity_status().accurate);
        // Replace only this fixture's now-empty ledger directory with a regular
        // file to inject an ordinary persistence failure, not a storage failure.
        fs::remove_file(&ledger).await.unwrap();
        fs::remove_dir(&ledger_dir).await.unwrap();
        fs::write(&ledger_dir, b"unavailable").await.unwrap();
        let error = match operation {
            "upload" => backend
                .upload_file("new.txt", Body::from("note"), Some(4), 100, None)
                .await
                .map(|_| ())
                .unwrap_err(),
            "copy" => backend.copy_path("old.txt", "new.txt").await.unwrap_err(),
            _ => backend.remove("old.txt").await.unwrap_err(),
        };
        let outcome = error.operation().unwrap();
        assert_eq!(outcome.commit, CommitState::Committed);
        assert_eq!(outcome.retry, "do_not_repeat");
        assert_eq!(error.code(), "operation_committed_pending");
        assert!(error.blocks_retry());
        assert!(!backend.capacity_status().accurate);
        if operation == "delete" {
            assert!(!root.join("old.txt").exists());
            assert_eq!(backend.capacity_status().used, 0);
            assert_eq!(outcome.cleanup, CleanupState::Pending);
        } else {
            assert_eq!(fs::read(root.join("old.txt")).await.unwrap(), b"old");
            let expected = if operation == "upload" {
                b"note".as_slice()
            } else {
                b"old".as_slice()
            };
            assert_eq!(fs::read(root.join("new.txt")).await.unwrap(), expected);
            assert_eq!(backend.capacity_status().used, 3 + expected.len() as u64);
        }
        let paths = storage.transactions.clone();
        drop(backend);
        drop(storage);
        tokio::time::timeout(Duration::from_secs(4), async {
            while Arc::strong_count(&paths) > 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
