use super::*;
use crate::{
    storage::StorageService, storage_transaction::TransactionId, test_support::TestDirectory,
};
use std::sync::atomic::{AtomicBool, AtomicUsize};

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cleanup completes within test deadline");
}

#[tokio::test]
async fn completed_copy_retries_without_scanning_other_copies() {
    let fixture = TestDirectory::new("copy-cleanup-retry");
    let paths = Arc::new(TransactionPaths::initialize(fixture.path()).await.unwrap());
    let discarded = paths.copy_path(&TransactionId::new());
    let active = paths.copy_path(&TransactionId::new());
    for path in [&discarded, &active] {
        tokio::fs::create_dir(path).await.unwrap();
        tokio::fs::write(path.join("note.txt"), b"ordinary data")
            .await
            .unwrap();
    }
    let fail = Arc::new(AtomicBool::new(true));
    let worker = UploadCleanupWorker::start_with_limits(
        paths.clone(),
        Arc::new(Semaphore::new(1)),
        1,
        Duration::from_millis(20),
        {
            let fail = fail.clone();
            move |path, kind| {
                let fail = fail.clone();
                async move {
                    if fail.load(Ordering::Relaxed) {
                        Err(AppError::internal("injected ordinary tree removal failure"))
                    } else {
                        remove_staging(&path, kind).await
                    }
                }
            }
        },
    );
    worker
        .reserve()
        .unwrap()
        .abandon_completed_copy(discarded.clone());
    wait_until(|| worker.status().failed_attempts > 0).await;
    assert_eq!(worker.status().pending_copies, 1);
    assert_eq!(worker.status().pending_uploads, 0);
    assert!(worker.reserve().is_err());
    fail.store(false, Ordering::Relaxed);
    wait_until(|| worker.status().pending_copies == 0).await;
    assert!(!discarded.exists());
    assert_eq!(
        tokio::fs::read(active.join("note.txt")).await.unwrap(),
        b"ordinary data"
    );
    drop(worker);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[tokio::test]
async fn large_copy_cleanup_uses_multiple_bounded_io_turns() {
    let fixture = TestDirectory::new("copy-cleanup-bounded");
    let paths = Arc::new(TransactionPaths::initialize(fixture.path()).await.unwrap());
    let discarded = paths.copy_path(&TransactionId::new());
    tokio::fs::create_dir(&discarded).await.unwrap();
    for index in 0..257 {
        tokio::fs::write(discarded.join(format!("{index}.bin")), b"x")
            .await
            .unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = UploadCleanupWorker::start_with_limits(
        paths.clone(),
        Arc::new(Semaphore::new(1)),
        1,
        Duration::from_millis(20),
        {
            let calls = calls.clone();
            move |path, kind| {
                let calls = calls.clone();
                async move {
                    if matches!(kind, StagingKind::Copy) {
                        calls.fetch_add(1, Ordering::Relaxed);
                    }
                    remove_staging(&path, kind).await
                }
            }
        },
    );
    worker
        .reserve()
        .unwrap()
        .abandon_completed_copy(discarded.clone());
    wait_until(|| worker.status().pending_copies == 0).await;
    assert!(!discarded.exists());
    assert!(calls.load(Ordering::Relaxed) >= 2);
    drop(worker);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[tokio::test]
async fn failure_keeps_its_slot_and_retries_without_scanning_active_uploads() {
    let fixture = TestDirectory::new("upload-cleanup-retry");
    let paths = Arc::new(TransactionPaths::initialize(fixture.path()).await.unwrap());
    let abandoned = paths.upload_path(&TransactionId::new());
    let active = paths.upload_path(&TransactionId::new());
    let copy = paths.copy_path(&TransactionId::new());
    for path in [&abandoned, &active, &copy] {
        tokio::fs::write(path, b"ordinary data").await.unwrap();
    }
    let fail = Arc::new(AtomicBool::new(true));
    let worker = UploadCleanupWorker::start_with_limits(
        paths.clone(),
        Arc::new(Semaphore::new(1)),
        1,
        Duration::from_millis(20),
        {
            let fail = fail.clone();
            move |path, _kind| {
                let fail = fail.clone();
                async move {
                    if fail.load(Ordering::Relaxed) {
                        Err(AppError::internal("injected ordinary removal failure"))
                    } else {
                        remove_upload(&path).await
                    }
                }
            }
        },
    );
    worker
        .reserve()
        .unwrap()
        .abandon(abandoned.clone(), None, None);
    wait_until(|| worker.status().failed_attempts > 0).await;
    assert_eq!(worker.status().pending_uploads, 1);
    assert!(worker.reserve().is_err());
    assert!(abandoned.exists());
    fail.store(false, Ordering::Relaxed);
    wait_until(|| worker.status().pending_uploads == 0).await;
    assert!(!abandoned.exists());
    assert_eq!(tokio::fs::read(&active).await.unwrap(), b"ordinary data");
    assert!(copy.exists());
    drop(worker.reserve().unwrap());
    drop(worker);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[tokio::test]
async fn unused_ticket_releases_admission_and_last_owner_drains_the_queue() {
    let fixture = TestDirectory::new("upload-cleanup-close");
    let paths = Arc::new(TransactionPaths::initialize(fixture.path()).await.unwrap());
    let worker = UploadCleanupWorker::start_with_limits(
        paths.clone(),
        Arc::new(Semaphore::new(1)),
        1,
        Duration::from_millis(10),
        |path, _kind| async move { remove_upload(&path).await },
    );
    let ticket = worker.reserve().unwrap();
    assert!(worker.reserve().is_err());
    drop(ticket);
    let ticket = worker.reserve().unwrap();
    let path = paths.upload_path(&TransactionId::new());
    tokio::fs::write(&path, b"discard").await.unwrap();
    drop(worker);
    // The reserved delivery itself keeps the receiver alive until Drop handoff.
    ticket.abandon(path.clone(), None, None);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
    assert!(!path.exists());
}

#[tokio::test]
async fn failed_final_cleanup_is_retained_for_startup_recovery() {
    let fixture = TestDirectory::new("upload-cleanup-restart");
    let paths = Arc::new(TransactionPaths::initialize(fixture.path()).await.unwrap());
    let path = paths.upload_path(&TransactionId::new());
    tokio::fs::write(&path, b"discard").await.unwrap();
    tokio::fs::write(fixture.path().join("keep.txt"), b"keep")
        .await
        .unwrap();
    let worker = UploadCleanupWorker::start_with_limits(
        paths.clone(),
        Arc::new(Semaphore::new(1)),
        1,
        Duration::from_millis(10),
        |_, _| async { Err(AppError::internal("injected ordinary removal failure")) },
    );
    worker.reserve().unwrap().abandon(path.clone(), None, None);
    drop(worker);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
    assert!(path.exists());
    drop(paths);
    TransactionPaths::initialize(fixture.path()).await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        tokio::fs::read(fixture.path().join("keep.txt"))
            .await
            .unwrap(),
        b"keep"
    );
}

#[tokio::test]
async fn busy_io_budget_does_not_block_handoff_of_another_upload() {
    let fixture = TestDirectory::new("upload-cleanup-busy-io");
    let storage = StorageService::new(fixture.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    let path = storage.transactions.upload_path(&TransactionId::new());
    tokio::fs::write(&path, b"discard").await.unwrap();
    let ticket = storage.upload_cleanup.reserve().unwrap();
    let writer = storage.begin_atomic_write("notes.txt").await.unwrap();
    assert_eq!(storage.io_gate.available_permits(), 0);
    ticket.abandon(path.clone(), None, None);
    // The first job has been received, but the active writer owns the only
    // I/O permit. Its subsequent handoff must still be consumed by the worker.
    wait_until(|| storage.upload_cleanup.sender.capacity() == MAX_OWNERS - 1).await;
    assert_eq!(storage.upload_cleanup_status().pending_uploads, 1);
    assert_eq!(storage.upload_cleanup_status().failed_attempts, 0);
    drop(writer);
    wait_until(|| storage.upload_cleanup_status().pending_uploads == 0).await;
    assert!(!path.exists());
    assert_eq!(storage.io_gate.available_permits(), 1);
    let paths = storage.transactions.clone();
    drop(storage);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[test]
fn pending_file_write_keeps_its_io_and_physical_reservation_until_finished() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = TestDirectory::new("upload-pending-write");
        let storage = StorageService::new(fixture.path().into(), 16, 1, 100, 0)
            .await
            .unwrap();
        let mut writer = storage
            .begin_atomic_write_with_expected("notes.txt", 8)
            .await
            .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();
        // Cancel while the accepted chunk is still queued for blocking I/O.
        assert!(tokio::time::timeout(
            Duration::from_millis(30),
            writer.write_chunk(&bytes::Bytes::from_static(b"note"))
        )
        .await
        .is_err());
        drop(writer);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(storage.upload_cleanup_status().pending_uploads, 1);
        assert_eq!(storage.io_gate.available_permits(), 0);
        assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 8);
        release.send(()).unwrap();
        blocker.await.unwrap();
        wait_until(|| storage.upload_cleanup_status().pending_uploads == 0).await;
        assert_eq!(storage.io_gate.available_permits(), 1);
        assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
        assert!(!fixture.path().join("notes.txt").exists());
        let paths = storage.transactions.clone();
        drop(storage);
        wait_until(|| Arc::strong_count(&paths) == 1).await;
    });
}
