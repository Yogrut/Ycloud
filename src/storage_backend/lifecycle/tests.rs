use super::super::StorageRegistry;
use super::*;
use axum::body::Body;

async fn local_lifecycle_fixture(
    label: &str,
) -> (crate::test_support::TestDirectory, StorageBackend) {
    let fixture = crate::test_support::TestDirectory::new(label);
    let storage =
        crate::storage::StorageService::new(fixture.path().join("files"), 1024, 1, 100, 0)
            .await
            .unwrap();
    (fixture, StorageBackend::local(storage))
}

#[tokio::test]
async fn policy_interruption_does_not_wait_or_duplicate_drain_and_reenable_stays_closed() {
    let (_fixture, backend) = local_lifecycle_fixture("policy-interruption-owner").await;
    let old = backend.admitted().unwrap();
    let owner = old.clone();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (finish, finishing) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        owner
            .owned_mutation(move |_| async move {
                started.send(()).unwrap();
                finishing.await.unwrap();
                Ok(())
            })
            .await
    });
    entered.await.unwrap();

    let guard = backend.interrupt_for_policy().unwrap();
    assert!(!waiter.is_finished());
    assert!(old.transfer_token().is_cancelled());
    assert!(backend.admitted().is_err());
    assert!(backend.interrupt_for_policy().is_none());
    drop(guard);
    assert!(backend.reenable_guard().await.is_err());
    assert!(!waiter.is_finished());

    finish.send(()).unwrap();
    waiter.await.unwrap().unwrap();
    crate::test_support::wait_storage_settled(&backend).await;
    let enabled = backend.reenable_guard().await.unwrap();
    assert!(backend.admitted().is_err());
    drop(enabled);
    let fresh = backend.admitted().unwrap();
    assert!(!fresh.transfer_token().is_cancelled());
    assert!(old.transfer_token().is_cancelled());
}

#[tokio::test]
async fn mutation_budget_rejects_without_executing_and_recovers_after_release() {
    let (_fixture, backend) = local_lifecycle_fixture("lifecycle-mutation-budget").await;
    let budget = backend
        .active
        .mutation_owners
        .clone()
        .try_acquire_many_owned(super::MAX_MUTATION_OWNERS as u32)
        .unwrap();
    let executed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = executed.clone();
    let error = backend
        .owned_mutation(move |_| async move {
            signal.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), "too_many_requests");
    assert!(error.operation().is_none());
    assert!(!executed.load(std::sync::atomic::Ordering::Acquire));

    drop(budget);
    backend.owned_mutation(|_| async { Ok(()) }).await.unwrap();
    assert_eq!(
        backend.active.mutation_owners.available_permits(),
        super::MAX_MUTATION_OWNERS
    );
}

#[tokio::test]
async fn panicked_mutation_is_unknown_and_releases_its_budget_and_admission() {
    let (_fixture, backend) = local_lifecycle_fixture("lifecycle-mutation-panic").await;
    let admitted = backend.admitted().unwrap();
    let error = admitted
        .owned_mutation::<(), _, _>(|_| async {
            panic!("simulated mutation owner panic");
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), "operation_result_unknown");
    let outcome = error.operation().unwrap();
    assert_eq!(outcome.commit, crate::error::CommitState::Unknown);
    assert_eq!(outcome.cleanup, crate::error::CleanupState::Unknown);
    assert_eq!(outcome.retry, "verify_first");
    assert!(error.blocks_retry());
    assert_eq!(
        backend.active.mutation_owners.available_permits(),
        super::MAX_MUTATION_OWNERS
    );
    drop(admitted);
    backend.edit_guard().await.unwrap();
    backend.owned_mutation(|_| async { Ok(()) }).await.unwrap();
}

#[tokio::test]
async fn cancelled_interrupt_waiter_reopens_only_after_old_owners_exit() {
    use std::{sync::atomic::Ordering, time::Duration};
    let (_fixture, backend) = local_lifecycle_fixture("lifecycle-edit-waiter-cancel").await;
    let old = backend.admitted().unwrap();
    let owner = backend
        .active
        .mutation_owners
        .clone()
        .try_acquire_owned()
        .unwrap();
    let editing_backend = backend.clone();
    let waiter = tokio::spawn(async move { editing_backend.interrupt_for_edit().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !backend.active.editing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(backend.active.editing.load(Ordering::Acquire));
    assert!(backend.admitted().is_err());
    assert!(backend.owned_mutation(|_| async { Ok(()) }).await.is_err());
    assert!(old.transfer_token().is_cancelled());

    drop(owner);
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.active.editing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let fresh = backend.admitted().unwrap();
    assert!(!fresh.transfer_token().is_cancelled());
    assert!(old.transfer_token().is_cancelled());
    fresh.owned_mutation(|_| async { Ok(()) }).await.unwrap();
}

#[tokio::test]
async fn interrupt_revokes_shared_admission_without_reviving_old_handles() {
    let (_fixture, backend) = local_lifecycle_fixture("lifecycle-admission-clones").await;
    let admitted = backend.admitted().unwrap();
    let first_clone = admitted.clone();
    let second_clone = admitted.clone();
    assert_eq!(backend.active.leases.lock().unwrap().len(), 1);
    let edit = backend.interrupt_for_edit().await.unwrap();
    assert!(backend.admitted().is_err());
    assert!(admitted.lease.as_ref().unwrap().lock().unwrap().is_none());
    assert!(first_clone.transfer_token().is_cancelled());
    assert!(second_clone.transfer_token().is_cancelled());
    drop(edit);

    let fresh = backend.admitted().unwrap();
    assert!(!fresh.transfer_token().is_cancelled());
    for old in [admitted, first_clone, second_clone] {
        let error = old.owned_mutation(|_| async { Ok(()) }).await.unwrap_err();
        assert_eq!(error.code(), "conflict");
    }
    fresh.owned_mutation(|_| async { Ok(()) }).await.unwrap();
}

#[tokio::test]
async fn edit_deadline_preserves_the_owner_and_reopens_only_after_completion() {
    use std::{sync::atomic::Ordering, time::Duration};

    let (_fixture, backend) = local_lifecycle_fixture("lifecycle-real-edit-deadline").await;
    let old = backend.admitted().unwrap();
    let owner = old.clone();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (finish, finishing) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        owner
            .owned_mutation(move |_| async move {
                started.send(()).unwrap();
                finishing.await.unwrap();
                Ok(())
            })
            .await
    });
    entered.await.unwrap();

    // Exercise the public deadline, not a manually constructed dropped guard.
    // Leave time for scheduling without turning a broken deadline into a hang.
    let result = tokio::time::timeout(
        super::EDIT_DRAIN_TIMEOUT + Duration::from_secs(2),
        backend.interrupt_for_edit(),
    )
    .await
    .unwrap();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("an unfinished owner must prevent the edit"),
    };
    assert_eq!(error.code(), "conflict");
    assert!(error.operation().is_none());
    assert!(!waiter.is_finished());
    assert!(backend.active.editing.load(Ordering::Acquire));
    assert!(backend.admitted().is_err());
    assert!(old.transfer_token().is_cancelled());
    assert_eq!(
        backend.active.mutation_owners.available_permits(),
        super::MAX_MUTATION_OWNERS - 1
    );

    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.active.editing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let fresh = backend.admitted().unwrap();
    assert!(!fresh.transfer_token().is_cancelled());
    assert!(old.transfer_token().is_cancelled());
    assert!(old.owned_mutation(|_| async { Ok(()) }).await.is_err());
    fresh.owned_mutation(|_| async { Ok(()) }).await.unwrap();
    drop(fresh);
    drop(old);
    backend.interrupt_for_edit().await.unwrap();
}

#[tokio::test]
async fn interrupted_edit_reopens_only_after_old_mutation_owners_exit() {
    use std::{sync::atomic::Ordering, time::Duration};
    let fixture = crate::test_support::TestDirectory::new("edit-reopen-after-owner");
    let storage =
        crate::storage::StorageService::new(fixture.path().join("files"), 1024, 1, 100, 0)
            .await
            .unwrap();
    let backend = StorageBackend::local(storage);
    let owner = backend
        .active
        .mutation_owners
        .clone()
        .try_acquire_owned()
        .unwrap();
    backend.active.editing.store(true, Ordering::Release);
    backend.transfer_token().cancel();
    // Model the guard dropped by the edit timeout before owner draining.
    drop(super::StorageEditGuard {
        backend: backend.clone(),
        _gate: None,
        _owners: None,
        interrupted: true,
        _remote: None,
    });
    tokio::task::yield_now().await;
    assert!(backend.active.editing.load(Ordering::Acquire));
    assert!(backend.owned_mutation(|_| async { Ok(()) }).await.is_err());
    drop(owner);
    tokio::time::timeout(Duration::from_secs(1), async {
        while backend.active.editing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!backend.transfer_token().is_cancelled());
    backend.owned_mutation(|_| async { Ok(()) }).await.unwrap();
}

#[tokio::test]
async fn administrator_edit_interrupts_slow_s3_capacity_scan() {
    use axum::{
        http::{Method, Response},
        routing::any,
        Router,
    };
    use std::{sync::Arc, time::Duration};
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = started.clone();
    let router = Router::new().route(
        "/{*key}",
        any(move |method: Method| {
            let signal = signal.clone();
            async move {
                if method == Method::GET {
                    signal.notify_one();
                    return std::future::pending::<Response<Body>>().await;
                }
                Response::builder().status(404).body(Body::empty()).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = crate::s3_backend::protocol_tests::test_backend(&format!(
        "http://{}",
        listener.local_addr().unwrap()
    ));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = crate::test_support::TestDirectory::new("admin-s3-scan-interrupt");
    let backend =
        StorageBackend::s3_configured(remote.clone(), None, fixture.path().join("ledger.json"))
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    let edit = tokio::time::timeout(Duration::from_secs(1), backend.interrupt_for_edit())
        .await
        .unwrap()
        .unwrap();
    assert!(remote.probe().await.is_err());
    drop(edit);
    // The cancelled read does not permanently disable request admission.
    assert!(!remote
        .upload_committed("missing.txt", 1, "test")
        .await
        .unwrap());
    let edit = backend.interrupt_for_edit().await.unwrap();
    edit.retire();
    drop(edit);
    // The old connection remains available for owned deferred cleanup,
    // while retirement keeps user mutation admission closed.
    assert!(!remote
        .upload_committed("missing.txt", 1, "test")
        .await
        .unwrap());
    assert!(backend.owned_mutation(|_| async { Ok(()) }).await.is_err());
    server.abort();
}

#[tokio::test]
async fn administrator_interrupt_cancels_download_and_revokes_old_admission() {
    use futures_util::StreamExt;
    let fixture = crate::test_support::TestDirectory::new("admin-download-interrupt");
    let path = fixture.path().join("files");
    let storage = crate::storage::StorageService::new(path.clone(), 1024, 1, 100, 0)
        .await
        .unwrap();
    tokio::fs::write(path.join("saved.bin"), b"saved")
        .await
        .unwrap();
    let registry = StorageRegistry::single("primary", StorageBackend::local(storage)).await;
    let admitted = registry.get("primary").await.unwrap();
    let response = admitted
        .stream_file(
            "saved.bin",
            &axum::http::HeaderMap::new(),
            crate::storage::FileResponseMode::Attachment,
        )
        .await
        .unwrap();
    let cached = registry.cached("primary").await.unwrap();
    let edit = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        cached.interrupt_for_edit(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(registry.get("primary").await.is_err());
    assert!(response
        .into_body()
        .into_data_stream()
        .next()
        .await
        .unwrap()
        .is_err());
    assert_eq!(
        tokio::fs::read(path.join("saved.bin")).await.unwrap(),
        b"saved"
    );
    drop(edit);
    let fresh = registry.get("primary").await.unwrap();
    assert!(!fresh.transfer_token().is_cancelled());
    assert!(admitted.transfer_token().is_cancelled());
}

#[tokio::test]
async fn lifecycle_admission_survives_disable_and_an_abandoned_mutation_waiter() {
    let fixture = crate::test_support::TestDirectory::new("backend-lifecycle");
    let storage =
        crate::storage::StorageService::new(fixture.path().join("files"), 1024, 1, 100, 0)
            .await
            .unwrap();
    let registry = StorageRegistry::single("primary", StorageBackend::local(storage)).await;
    let cached = registry.cached("primary").await.unwrap();
    let admitted = registry.get("primary").await.unwrap();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (finish, finishing) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        admitted
            .owned_mutation(move |_backend| async move {
                started.send(()).unwrap();
                finishing.await.unwrap();
                Ok(())
            })
            .await
    });
    entered.await.unwrap();
    waiter.abort();
    waiter.await.unwrap_err();
    assert!(cached.edit_guard().await.is_err());
    registry.set_enabled("primary", false).await;
    assert!(registry.get("primary").await.is_err());
    registry.set_enabled("primary", true).await;
    assert!(cached.edit_guard().await.is_err());
    finish.send(()).unwrap();
    let edit = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(edit) = cached.edit_guard().await {
                break edit;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(registry.get("primary").await.is_err());
    edit.retire();
    drop(edit);
    assert!(registry.get("primary").await.is_err());
}
