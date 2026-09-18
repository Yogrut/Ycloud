use super::*;
use crate::test_support::TestDirectory;
use std::time::Duration;
use tokio::sync::{oneshot, Semaphore};

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned work finishes within the test deadline");
}

async fn close_storage(storage: StorageService) {
    let paths = storage.transactions.clone();
    drop(storage);
    wait_until(|| Arc::strong_count(&paths) == 1).await;
}

#[tokio::test]
async fn cancellation_waiting_for_admission_creates_no_upload_or_reservation() {
    let directory = TestDirectory::new("upload-admission");
    let io = Arc::new(Semaphore::new(1));
    let storage = StorageService::new_with_io_gate(directory.path().into(), 16, io.clone(), 100, 0)
        .await
        .unwrap();
    let held = io.clone().acquire_owned().await.unwrap();
    let waiting_storage = storage.clone();
    let owners = Arc::strong_count(&io);
    let waiter = tokio::spawn(async move {
        waiting_storage
            .begin_atomic_write_with_expected("notes.txt", 8)
            .await
    });
    // The admission future holds an extra Arc only after path validation.
    wait_until(|| Arc::strong_count(&io) > owners).await;
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
    assert_eq!(
        std::fs::read_dir(&storage.transactions.uploads)
            .unwrap()
            .count(),
        0
    );
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    drop(held);
    assert_eq!(io.available_permits(), 1);
    assert!(!directory.path().join("notes.txt").exists());
    close_storage(storage).await;
}

#[tokio::test]
async fn cancelled_open_waiter_keeps_budgets_until_the_file_has_an_owner() {
    for created_before_wait in [false, true] {
        let directory = TestDirectory::new("upload-owned-open");
        let io = Arc::new(Semaphore::new(1));
        let storage =
            StorageService::new_with_io_gate(directory.path().into(), 16, io.clone(), 100, 0)
                .await
                .unwrap();
        let (started, received) = oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let waiting_storage = storage.clone();
        let waiter = tokio::spawn(async move {
            waiting_storage
                .begin_atomic_write_internal("notes.txt", Some(8), move |path| {
                    let file = if created_before_wait {
                        Some(create_temporary(path)?)
                    } else {
                        None
                    };
                    let _ = started.send(path.to_path_buf());
                    blocked
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(std::io::Error::other)?;
                    match file {
                        Some(file) => Ok(file),
                        None => create_temporary(path),
                    }
                })
                .await
        });
        let temporary = tokio::time::timeout(Duration::from_secs(3), received)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        assert_eq!(temporary.exists(), created_before_wait);
        assert_eq!(io.available_permits(), 0);
        assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 8);
        release.send(()).unwrap();
        wait_until(|| {
            io.available_permits() == 1
                && !temporary.exists()
                && *storage.reserved_upload_bytes.lock().unwrap() == 0
        })
        .await;
        assert!(!directory.path().join("notes.txt").exists());
        close_storage(storage).await;
    }
}

#[tokio::test]
async fn ordinary_open_failure_releases_both_budgets() {
    let directory = TestDirectory::new("upload-open-error");
    let storage = StorageService::new(directory.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    let result = storage
        .begin_atomic_write_internal("notes.txt", Some(8), |_| {
            Err(std::io::Error::other("injected ordinary open failure"))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(storage.io_gate.available_permits(), 1);
    assert_eq!(*storage.reserved_upload_bytes.lock().unwrap(), 0);
    assert_eq!(
        std::fs::read_dir(&storage.transactions.uploads)
            .unwrap()
            .count(),
        0
    );
    close_storage(storage).await;
}

#[tokio::test]
async fn cancelling_commit_waiter_keeps_upload_until_publication_finishes() {
    let directory = TestDirectory::new("upload-before-handoff");
    let storage = StorageService::new(directory.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    let mut writer = storage
        .begin_atomic_write_with_expected("notes.txt", 4)
        .await
        .unwrap();
    writer
        .write_chunk(&Bytes::from_static(b"note"))
        .await
        .unwrap();
    writer.file.as_ref().unwrap().sync_all().await.unwrap();
    let temporary = writer.temporary.clone();
    let mutation = storage.mutation_gate.lock().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), writer.commit())
            .await
            .is_err()
    );
    assert!(temporary.exists());
    assert!(!directory.path().join("notes.txt").exists());
    assert_eq!(
        std::fs::read_dir(&storage.transactions.journals)
            .unwrap()
            .count(),
        0
    );
    drop(mutation);
    wait_until(|| storage.io_gate.available_permits() == 1).await;
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(directory.path().join("notes.txt")).unwrap(),
        b"note"
    );
    close_storage(storage).await;
}

#[tokio::test]
async fn validation_failure_does_not_transfer_upload_ownership() {
    let directory = TestDirectory::new("upload-validation-owner");
    let storage = StorageService::new(directory.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    let mut writer = storage
        .begin_atomic_write_with_expected("notes.txt", 4)
        .await
        .unwrap();
    writer
        .write_chunk(&Bytes::from_static(b"note"))
        .await
        .unwrap();
    let temporary = writer.temporary.clone();
    // An ordinary competing operation has created a directory at the target.
    std::fs::create_dir(directory.path().join("notes.txt")).unwrap();
    assert_eq!(writer.commit().await.unwrap_err().code(), "conflict");
    wait_until(|| !temporary.exists()).await;
    assert!(directory.path().join("notes.txt").is_dir());
    close_storage(storage).await;
}

#[tokio::test]
async fn journal_io_failure_retains_upload_for_restart_recovery() {
    let directory = TestDirectory::new("upload-recovery-owner");
    let storage = StorageService::new(directory.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    std::fs::write(directory.path().join("notes.txt"), b"original").unwrap();
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
    // Remove only this fixture's empty journal directory to inject ordinary I/O failure.
    std::fs::remove_dir(&journals).unwrap();
    assert!(writer.commit().await.is_err());
    assert_eq!(storage.upload_cleanup_status().pending_uploads, 0);
    assert_eq!(std::fs::read(&temporary).unwrap(), b"note");
    assert_eq!(
        std::fs::read(directory.path().join("notes.txt")).unwrap(),
        b"original"
    );
    std::fs::create_dir(&journals).unwrap();
    close_storage(storage).await;
    let recovered = StorageService::new(directory.path().into(), 16, 1, 100, 0)
        .await
        .unwrap();
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(directory.path().join("notes.txt")).unwrap(),
        b"original"
    );
    close_storage(recovered).await;
}
