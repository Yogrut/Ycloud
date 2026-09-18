use super::*;
use crate::test_support::TestDirectory;
use std::{sync::Arc, time::Duration};

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("copy ownership settles within test deadline");
}

async fn close(storage: StorageService) {
    let paths = storage.transactions.clone();
    drop(storage);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[tokio::test]
async fn admitted_copy_retains_budgets_and_finishes_after_waiter_cancels() {
    let fixture = TestDirectory::new("owned-copy-cancel");
    let root = fixture.path().join("files");
    let ledger = fixture.path().join("capacity.json");
    let storage = StorageService::new(root.clone(), 100, 1, 100, 0)
        .await
        .unwrap();
    fs::write(root.join("notes.txt"), b"note").await.unwrap();
    let source = storage.resolve_existing("notes.txt").await.unwrap();
    let target = storage.resolve_for_write("copy.txt").await.unwrap();
    let capacity = CapacityTracker::new_with_ledger(Some(8), 4, true, Some(ledger.clone()));
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn({
        let storage = storage.clone();
        let capacity = capacity.clone();
        async move {
            storage
                .copy_owned(
                    &source,
                    &target,
                    4,
                    Some(capacity),
                    move |source, staging, directory| async move {
                        copy_staging(source, staging, directory).await?;
                        entered.send(()).unwrap();
                        blocked.await.unwrap();
                        Ok(())
                    },
                )
                .await
        }
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(storage.io_gate.available_permits(), 0);
    assert!(storage.mutation_gate.try_lock().is_err());
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 4);
    assert_eq!(capacity.status().reserved, 4);
    assert!(capacity.reserve_replacement(0, 1).is_err());
    assert!(!root.join("copy.txt").exists());
    release.send(()).unwrap();
    wait_until(|| storage.io_gate.available_permits() == 1).await;
    assert_eq!(fs::read(root.join("copy.txt")).await.unwrap(), b"note");
    assert_eq!(capacity.status().used, 8);
    assert_eq!(capacity.status().reserved, 0);
    assert!(capacity.status().accurate);
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
    assert_eq!(storage.upload_cleanup_status().pending_copies, 0);
    assert_eq!(
        crate::capacity::load_capacity_ledger(&ledger)
            .await
            .unwrap(),
        Some(8)
    );
    close(storage).await;
}

#[tokio::test]
async fn cancellation_before_admission_creates_no_copy_or_quota() {
    let fixture = TestDirectory::new("owned-copy-admission");
    let storage = StorageService::new(fixture.path().into(), 100, 1, 100, 0)
        .await
        .unwrap();
    fs::write(fixture.path().join("notes.txt"), b"note")
        .await
        .unwrap();
    let source = storage.resolve_existing("notes.txt").await.unwrap();
    let target = storage.resolve_for_write("copy.txt").await.unwrap();
    let capacity = CapacityTracker::new(Some(8), 4);
    let permit = storage.acquire_io().await.unwrap();
    assert!(tokio::time::timeout(
        Duration::from_millis(30),
        storage.copy_path_with_capacity(&source, &target, 4, capacity.clone())
    )
    .await
    .is_err());
    assert_eq!(capacity.status().reserved, 0);
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
    assert_eq!(
        std::fs::read_dir(&storage.transactions.copies)
            .unwrap()
            .count(),
        0
    );
    assert!(!fixture.path().join("copy.txt").exists());
    drop(permit);
    close(storage).await;
}

#[tokio::test]
async fn failed_partial_copy_hands_off_its_tree_and_releases_quota() {
    let fixture = TestDirectory::new("owned-copy-partial");
    let storage = StorageService::new(fixture.path().into(), 100, 1, 100, 0)
        .await
        .unwrap();
    fs::create_dir(fixture.path().join("notes")).await.unwrap();
    fs::write(fixture.path().join("notes/a.txt"), b"note")
        .await
        .unwrap();
    let source = storage.resolve_existing("notes").await.unwrap();
    let target = storage.resolve_for_write("copy").await.unwrap();
    let capacity = CapacityTracker::new(Some(8), 4);
    let result = storage
        .copy_owned(
            &source,
            &target,
            4,
            Some(capacity.clone()),
            |_, staging, _| async move {
                fs::create_dir(&staging).await.unwrap();
                fs::write(staging.join("a.txt"), b"no").await.unwrap();
                Err(AppError::internal("injected ordinary copy I/O failure"))
            },
        )
        .await;
    assert!(result.is_err());
    wait_until(|| storage.upload_cleanup_status().pending_copies == 0).await;
    assert_eq!(
        std::fs::read_dir(&storage.transactions.copies)
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fs::read(fixture.path().join("notes/a.txt")).await.unwrap(),
        b"note"
    );
    assert!(!fixture.path().join("copy").exists());
    assert_eq!(capacity.status().used, 4);
    assert_eq!(capacity.status().reserved, 0);
    assert!(capacity.status().accurate);
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
    close(storage).await;
}

#[tokio::test]
async fn directory_copy_publishes_contents_and_charges_capacity_once() {
    let fixture = TestDirectory::new("owned-copy-directory");
    let storage = StorageService::new(fixture.path().into(), 100, 1, 100, 0)
        .await
        .unwrap();
    fs::create_dir_all(fixture.path().join("notes/sub"))
        .await
        .unwrap();
    fs::write(fixture.path().join("notes/a.txt"), b"abc")
        .await
        .unwrap();
    fs::write(fixture.path().join("notes/sub/b.txt"), b"de")
        .await
        .unwrap();
    let source = storage.resolve_existing("notes").await.unwrap();
    let target = storage.resolve_for_write("copy").await.unwrap();
    let capacity = CapacityTracker::new(Some(10), 5);
    storage
        .copy_path_with_capacity(&source, &target, 5, capacity.clone())
        .await
        .unwrap();
    assert_eq!(
        fs::read(fixture.path().join("copy/sub/b.txt"))
            .await
            .unwrap(),
        b"de"
    );
    assert_eq!(capacity.status().used, 10);
    assert_eq!(capacity.status().reserved, 0);
    assert!(storage
        .copy_path_with_capacity(&source, &target, 5, capacity.clone())
        .await
        .is_err());
    assert_eq!(capacity.status().used, 10);
    assert_eq!(
        std::fs::read_dir(&storage.transactions.copies)
            .unwrap()
            .count(),
        0
    );
    close(storage).await;
}

#[test]
fn publication_accounting_retains_uncertainty_until_ledger_settles() {
    let capacity = CapacityTracker::new(Some(8), 4);
    let mut accounting = CopyAccounting::reserve(Some(capacity.clone()), 4).unwrap();
    accounting.publication_started = true;
    drop(accounting);
    assert!(!capacity.status().accurate);
    assert_eq!(capacity.status().reserved, 0);
    assert!(capacity.reserve_replacement(0, 1).is_err());

    let capacity = CapacityTracker::new(Some(8), 4);
    let mut accounting = CopyAccounting::reserve(Some(capacity.clone()), 4).unwrap();
    accounting.publication_started = true;
    accounting.published(4);
    drop(accounting);
    assert!(!capacity.status().accurate);
    assert_eq!(capacity.status().used, 8);
    assert_eq!(capacity.status().reserved, 0);
}
