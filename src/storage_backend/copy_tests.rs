use super::*;
use axum::{
    extract::Query,
    http::{HeaderMap, Method, Response, Uri},
    routing::any,
    Router,
};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicUsize, Mutex},
    time::Duration,
};

type Objects = Arc<Mutex<BTreeMap<String, (u64, String)>>>;

struct CopyFixture {
    backend: StorageBackend,
    settings: crate::config::S3StorageConfig,
    objects: Objects,
    started: Arc<Notify>,
    release: Arc<Notify>,
    copies: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    scans: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
    directory: crate::test_support::TestDirectory,
}

impl Drop for CopyFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl CopyFixture {
    async fn new(directory_copy: bool, reject_copy: bool) -> Self {
        let source = if directory_copy {
            "tenant/source/file.txt"
        } else {
            "tenant/source.txt"
        };
        let objects: Objects = Arc::new(Mutex::new(BTreeMap::from([(
            source.into(),
            (4, "\"source-etag\"".into()),
        )])));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let copies = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let scans = Arc::new(AtomicUsize::new(0));
        let stored = objects.clone();
        let signal = started.clone();
        let unblock = release.clone();
        let copy_count = copies.clone();
        let request_count = requests.clone();
        let scan_count = scans.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri, headers: HeaderMap,
                      Query(query): Query<HashMap<String, String>>| {
                let objects = stored.clone();
                let started = signal.clone();
                let release = unblock.clone();
                let copies = copy_count.clone();
                let requests = request_count.clone();
                let scans = scan_count.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    let key = uri.path().strip_prefix("/bucket/").unwrap_or("");
                    let response = Response::builder();
                    if method == Method::HEAD {
                        return match objects.lock().unwrap().get(key) {
                            Some((size, etag)) => response.header("content-length", *size)
                                .header("etag", etag).body(Body::empty()).unwrap(),
                            None => response.status(404).body(Body::empty()).unwrap(),
                        };
                    }
                    if method == Method::GET && query.get("list-type").map(String::as_str) == Some("2") {
                        let prefix = query.get("prefix").map(String::as_str).unwrap_or("");
                        if prefix == "tenant/" {
                            scans.fetch_add(1, Ordering::SeqCst);
                        }
                        let max = query.get("max-keys").unwrap().parse::<usize>().unwrap();
                        let objects = objects.lock().unwrap();
                        let contents = objects.iter().filter(|(key, _)| key.starts_with(prefix))
                            .take(max).map(|(key, (size, etag))| format!(
                                "<Contents><Key>{key}</Key><Size>{size}</Size><ETag>{etag}</ETag></Contents>"
                            )).collect::<Vec<_>>();
                        return response.header("content-type", "application/xml")
                            .body(Body::from(format!(
                                "<ListBucketResult><KeyCount>{}</KeyCount><IsTruncated>false</IsTruncated>{}</ListBucketResult>",
                                contents.len(), contents.concat()
                            ))).unwrap();
                    }
                    if method == Method::PUT {
                        if let Some(source) = headers.get("x-amz-copy-source") {
                            assert_eq!(headers.get("x-amz-copy-source-if-match").unwrap(), "\"source-etag\"");
                            assert_eq!(headers.get("if-none-match").unwrap(), "*");
                            copies.fetch_add(1, Ordering::SeqCst);
                            started.notify_one();
                            release.notified().await;
                            if reject_copy {
                                return response.status(403).body(Body::empty()).unwrap();
                            }
                            let source = source.to_str().unwrap().trim_start_matches('/');
                            let source = source.strip_prefix("bucket/").unwrap();
                            let mut objects = objects.lock().unwrap();
                            let (size, _) = objects.get(source).unwrap();
                            let size = *size;
                            objects.insert(key.into(), (size, "\"copy-etag\"".into()));
                            return response.header("content-type", "application/xml")
                                .body(Body::from("<CopyObjectResult><ETag>\"copy-etag\"</ETag></CopyObjectResult>"))
                                .unwrap();
                        }
                        let size = headers.get("content-length").unwrap().to_str().unwrap().parse().unwrap();
                        objects.lock().unwrap().insert(key.into(), (size, "\"journal-etag\"".into()));
                        return response.header("etag", "\"journal-etag\"").body(Body::empty()).unwrap();
                    }
                    if method == Method::DELETE {
                        assert!(key.starts_with("tenant/.ycloud-system/"));
                        objects.lock().unwrap().remove(key);
                        return response.status(204).body(Body::empty()).unwrap();
                    }
                    response.status(400).body(Body::empty()).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let settings = crate::s3_backend::protocol_tests::test_settings(&endpoint);
        let remote = crate::s3_backend::protocol_tests::test_backend(&endpoint);
        let directory = crate::test_support::TestDirectory::new("s3-copy-owner");
        let capacity = CapacityTracker::new_with_ledger(
            Some(8),
            4,
            true,
            Some(directory.path().join("ledger.json")),
        );
        let backend = StorageBackend {
            active: Arc::new(ActiveStorage::new(
                StorageBackendKind::S3(remote),
                capacity,
                None,
            )),
            lease: None,
            transfer: None,
        };
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            backend,
            settings,
            objects,
            started,
            release,
            copies,
            requests,
            scans,
            server,
            directory,
        }
    }

    async fn wait_started(&self) {
        tokio::time::timeout(Duration::from_secs(3), self.started.notified())
            .await
            .unwrap();
    }

    async fn app_state(&self) -> crate::state::AppState {
        let mut persisted = crate::config::ConfigFile::with_test_storage();
        persisted.storage_instances[0].backend =
            crate::config::StorageBackendConfig::S3(self.settings.clone());
        persisted.storage_instances[0].enabled = false;
        let state = crate::test_support::app_state(&self.directory, persisted).await;
        // The protocol fixture is already activated; publish that exact backend
        // instead of asking the copy-only simulator to emulate activation probes.
        state
            .update_config(|config| {
                config.storage_instances[0].enabled = true;
                Ok(())
            })
            .await
            .unwrap();
        state
            .backends
            .insert_ready("primary", self.backend.clone())
            .await;
        state
    }

    async fn wait_settled(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.backend.active.mutation_owners.available_permits() != MAX_MUTATION_OWNERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    async fn wait_reconciled(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !self.backend.capacity_status().accurate {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn cancelled_edit_waiting_on_remote_gate_keeps_admission_closed_until_drain_finishes() {
    let fixture = CopyFixture::new(false, false).await;
    let StorageBackendKind::S3(remote) = &fixture.backend.active.kind else {
        unreachable!();
    };
    let capacity_gate = remote.acquire_capacity_mutation().await;
    let backend = fixture.backend.clone();
    let waiter = tokio::spawn(async move { backend.interrupt_for_edit().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.backend.active.mutation_owners.available_permits() != 0
            || fixture.backend.active.lifecycle.try_read().is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Owner and lifecycle gates have drained, but the remote gate has not.
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    assert!(fixture.backend.interruption_pending());
    assert!(fixture.backend.admitted().is_err());
    assert!(fixture.backend.interrupt_for_policy().is_none());
    drop(capacity_gate);
    crate::test_support::wait_storage_settled(&fixture.backend).await;
    assert!(fixture.backend.admitted().is_ok());
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disabling_s3_publishes_while_sent_copy_settles_and_reenable_rejects_old_owner() {
    let fixture = CopyFixture::new(false, false).await;
    let state = fixture.app_state().await;
    let backend = state.storage_backend("primary").await.unwrap();
    let waiter = tokio::spawn(async move { backend.copy_path("source.txt", "target.txt").await });
    fixture.wait_started().await;

    tokio::time::timeout(
        Duration::from_secs(2),
        state.update_storage_access("primary", false, false),
    )
    .await
    .expect("disabling waited for the unfinished COPY")
    .unwrap();
    assert!(!waiter.is_finished());
    assert!(
        !crate::config::load_config(&state.config.config_path)
            .await
            .unwrap()
            .storage_instances[0]
            .enabled
    );
    assert!(state.storage_backend("primary").await.is_err());
    assert!(state
        .update_storage_access("primary", true, false)
        .await
        .is_err());
    assert!(!state.config_file.read().await.storage_instances[0].enabled);

    // Repeated disabling must not create a second drain or forget the owner.
    state
        .update_storage_access("primary", false, false)
        .await
        .unwrap();
    assert!(!waiter.is_finished());
    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Unknown
    );
    crate::test_support::wait_storage_settled(&fixture.backend).await;
    fixture.wait_reconciled().await;
    assert!(fixture
        .objects
        .lock()
        .unwrap()
        .contains_key("tenant/target.txt"));
    assert_eq!(fixture.copies.load(Ordering::SeqCst), 1);
    state
        .update_storage_access("primary", true, false)
        .await
        .unwrap();
    assert!(state.storage_backend("primary").await.is_ok());
}

#[tokio::test]
async fn rejected_policy_publication_preserves_configuration_and_the_sent_copy_owner() {
    let fixture = CopyFixture::new(false, false).await;
    let state = fixture.app_state().await;
    let backend = state.storage_backend("primary").await.unwrap();
    let waiter = tokio::spawn(async move { backend.copy_path("source.txt", "target.txt").await });
    fixture.wait_started().await;
    // Exercise the public config-publication deadline, without altering real
    // permissions or deleting files to manufacture a filesystem failure.
    let publication = state.config_updates.lock().await;
    let error = tokio::time::timeout(
        Duration::from_secs(7),
        state.update_storage_access("primary", false, false),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert!(state.config_file.read().await.storage_instances[0].enabled);
    assert!(
        crate::config::load_config(&state.config.config_path)
            .await
            .unwrap()
            .storage_instances[0]
            .enabled
    );
    assert!(fixture.backend.interruption_pending());
    assert!(!waiter.is_finished());
    assert!(state.storage_backend("primary").await.is_err());
    drop(publication);

    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Unknown
    );
    crate::test_support::wait_storage_settled(&fixture.backend).await;
    assert!(state.storage_backend("primary").await.is_ok());
    assert!(fixture
        .objects
        .lock()
        .unwrap()
        .contains_key("tenant/target.txt"));
    assert_eq!(fixture.copies.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn copy_deadline_exits_after_issued_write_and_does_not_delete_its_result() {
    let fixture = CopyFixture::new(false, false).await;
    let StorageBackendKind::S3(remote) = &fixture.backend.active.kind else {
        unreachable!();
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let worker = remote.scoped_work(Some(deadline), None);
    let waiter = tokio::spawn(async move { worker.copy_file("source.txt", "target.txt").await });
    fixture.wait_started().await;
    tokio::time::sleep_until(deadline).await;
    assert!(!waiter.is_finished());
    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Unknown
    );
    assert!(fixture
        .objects
        .lock()
        .unwrap()
        .contains_key("tenant/target.txt"));
    assert_eq!(remote.metadata("target.txt").await.unwrap().size, 4);
}

#[tokio::test]
async fn resumed_maintenance_cannot_revive_a_copy_already_bound_to_old_generation() {
    let fixture = CopyFixture::new(false, false).await;
    let StorageBackendKind::S3(remote) = &fixture.backend.active.kind else {
        unreachable!();
    };
    let worker = remote.clone();
    let waiter = tokio::spawn(async move { worker.copy_file("source.txt", "target.txt").await });
    fixture.wait_started().await;
    remote.pause_maintenance();
    remote.resume_maintenance();
    assert!(!waiter.is_finished());
    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Unknown
    );
    assert!(fixture
        .objects
        .lock()
        .unwrap()
        .contains_key("tenant/target.txt"));
    assert_eq!(fixture.copies.load(Ordering::SeqCst), 1);
    assert_eq!(remote.metadata("target.txt").await.unwrap().size, 4);
}

#[tokio::test]
async fn copy_shutdown_keeps_issued_write_alive_until_it_returns() {
    let fixture = CopyFixture::new(false, false).await;
    let StorageBackendKind::S3(remote) = &fixture.backend.active.kind else {
        unreachable!();
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let worker = remote.scoped_work(None, Some(stop.clone()));
    let waiter = tokio::spawn(async move { worker.copy_file("source.txt", "target.txt").await });
    fixture.wait_started().await;
    stop.cancel();
    assert!(!waiter.is_finished());
    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Unknown
    );
    assert!(fixture
        .objects
        .lock()
        .unwrap()
        .contains_key("tenant/target.txt"));
    assert_eq!(remote.metadata("target.txt").await.unwrap().size, 4);
}

#[tokio::test]
async fn s3_copy_owner_finishes_after_waiter_disconnect_and_charges_once() {
    for directory_copy in [false, true] {
        let fixture = CopyFixture::new(directory_copy, false).await;
        let backend = fixture.backend.admitted().unwrap();
        let waiter = tokio::spawn(async move {
            if directory_copy {
                backend.copy_path("source", "target").await
            } else {
                backend.copy_path("source.txt", "target.txt").await
            }
        });
        fixture.wait_started().await;
        assert_eq!(fixture.backend.capacity_status().reserved, 4);
        assert_eq!(fixture.backend.capacity_status().used, 4);
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(
            fixture.backend.active.mutation_owners.available_permits(),
            MAX_MUTATION_OWNERS - 1
        );
        assert!(fixture.backend.edit_guard().await.is_err());
        fixture.release.notify_one();
        fixture.wait_settled().await;
        let status = fixture.backend.capacity_status();
        assert_eq!(status.used, 8);
        assert_eq!(status.reserved, 0);
        assert!(status.accurate);
        assert_eq!(fixture.copies.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::capacity::load_capacity_ledger(&fixture.directory.path().join("ledger.json"))
                .await
                .unwrap(),
            Some(8)
        );
        assert_eq!(fixture.scans.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.objects.lock().unwrap().len(), 2);
        fixture.backend.edit_guard().await.unwrap();
    }
}

#[tokio::test]
async fn s3_copy_owner_budget_rejects_before_remote_requests() {
    let fixture = CopyFixture::new(false, false).await;
    let permits = fixture
        .backend
        .active
        .mutation_owners
        .clone()
        .acquire_many_owned(MAX_MUTATION_OWNERS as u32)
        .await
        .unwrap();
    let error = fixture
        .backend
        .copy_path("source.txt", "target.txt")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "too_many_requests");
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.backend.capacity_status().reserved, 0);
    drop(permits);
}

#[tokio::test]
async fn failed_s3_copy_owner_releases_reservation_and_reconciles_capacity() {
    let fixture = CopyFixture::new(false, true).await;
    let backend = fixture.backend.clone();
    let waiter = tokio::spawn(async move { backend.copy_path("source.txt", "target.txt").await });
    fixture.wait_started().await;
    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Unknown
    );
    assert!(error.blocks_retry());
    fixture.wait_settled().await;
    assert_eq!(fixture.backend.capacity_status().reserved, 0);
    fixture.wait_reconciled().await;
    assert_eq!(fixture.backend.capacity_status().used, 4);
    assert_eq!(fixture.objects.lock().unwrap().len(), 1);
    assert_eq!(fixture.scans.load(Ordering::SeqCst), 1);
    fixture.backend.edit_guard().await.unwrap();
}

#[tokio::test]
async fn s3_copy_owner_rechecks_source_size_after_waiting_for_capacity_gate() {
    let fixture = CopyFixture::new(false, false).await;
    let StorageBackendKind::S3(remote) = &fixture.backend.active.kind else {
        unreachable!();
    };
    let gate = remote.acquire_capacity_mutation().await;
    let backend = fixture.backend.clone();
    let waiter = tokio::spawn(async move { backend.copy_path("source.txt", "target.txt").await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.backend.capacity_status().reserved != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture
        .objects
        .lock()
        .unwrap()
        .insert("tenant/source.txt".into(), (5, "\"changed-etag\"".into()));
    drop(gate);
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert!(error.operation().is_none());
    fixture.wait_settled().await;
    fixture.wait_reconciled().await;
    assert_eq!(fixture.copies.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.objects.lock().unwrap().len(), 1);
    assert_eq!(fixture.backend.capacity_status().reserved, 0);
    assert_eq!(fixture.backend.capacity_status().used, 5);
    assert_eq!(fixture.scans.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn s3_copy_owner_invalidates_snapshots_built_during_copy() {
    use crate::directory_listing::{DirectoryEntryFilter, DirectorySort, SortDirection};
    let fixture = CopyFixture::new(false, false).await;
    let backend = fixture.backend.clone();
    let waiter = tokio::spawn(async move { backend.copy_path("source.txt", "target.txt").await });
    fixture.wait_started().await;
    let key = DirectorySnapshotKey::new(
        "",
        &DirectoryListRequest {
            limit: 20,
            search: None,
            sort: DirectorySort::Name,
            direction: SortDirection::Asc,
            filter: DirectoryEntryFilter::All,
            after: None,
        },
    );
    let SnapshotClaim::Build(build) = fixture
        .backend
        .active
        .directory_snapshots
        .claim(key.clone())
    else {
        panic!("expected a fresh snapshot");
    };
    build.publish(Arc::new(Vec::new()));
    assert!(matches!(
        fixture
            .backend
            .active
            .directory_snapshots
            .claim(key.clone()),
        SnapshotClaim::Ready(_)
    ));
    fixture.release.notify_one();
    waiter.await.unwrap().unwrap();
    assert!(matches!(
        fixture.backend.active.directory_snapshots.claim(key),
        SnapshotClaim::Build(_)
    ));
}

#[tokio::test]
async fn s3_copy_owner_admin_interrupt_waits_for_sent_copy_and_reports_unknown() {
    let fixture = CopyFixture::new(false, false).await;
    let backend = fixture.backend.admitted().unwrap();
    let waiter = tokio::spawn(async move { backend.copy_path("source.txt", "target.txt").await });
    fixture.wait_started().await;
    let backend = fixture.backend.clone();
    let editor = tokio::spawn(async move { backend.interrupt_for_edit().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture.backend.active.editing.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!editor.is_finished());
    assert!(fixture.backend.admitted().is_err());
    assert_eq!(fixture.copies.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.backend.capacity_status().reserved, 4);
    fixture.release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(
        error.operation().map(|outcome| outcome.commit),
        Some(crate::error::CommitState::Unknown)
    );
    assert!(error.blocks_retry());
    let edit = editor.await.unwrap().unwrap();
    assert!(fixture
        .objects
        .lock()
        .unwrap()
        .contains_key("tenant/target.txt"));
    assert_eq!(fixture.backend.capacity_status().reserved, 0);
    assert!(!fixture.backend.capacity_status().accurate);
    drop(edit);
    fixture.wait_reconciled().await;
    assert_eq!(fixture.backend.capacity_status().used, 8);
    assert_eq!(fixture.copies.load(Ordering::SeqCst), 1);
}
