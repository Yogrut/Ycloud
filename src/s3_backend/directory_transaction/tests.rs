use super::record::sign_transaction;
use super::record::TRASH_CATEGORY;
use super::*;

type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<(axum::http::Method, String)>>>;

#[derive(Clone, Copy, PartialEq)]
enum CompletionFault {
    None,
    FinalWrite,
    DelayedFinalWrite,
    TrashDelete,
}

struct CompletionFixture {
    backend: S3Backend,
    journal_key: String,
    journal: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    requests: RequestLog,
    final_write_started: std::sync::Arc<tokio::sync::Notify>,
    final_write_release: std::sync::Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for CompletionFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl CompletionFixture {
    async fn new(operation: Operation, completed: bool, fault: CompletionFault) -> Self {
        use axum::{
            body::{Body, Bytes},
            http::{HeaderMap, Method, Response, Uri},
            routing::any,
            Router,
        };
        use std::sync::{Arc, Mutex};

        let id = "fedcba9876543210fedcba9876543210";
        let journal_key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let target_key = if operation == Operation::Delete {
            internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/file.bin"))
        } else {
            "tenant/target/file.bin".into()
        };
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation,
            source_relative: "source".into(),
            destination_relative: (operation != Operation::Delete).then(|| "target".into()),
            stage: if completed {
                if operation == Operation::Copy {
                    Stage::CopyCompleted
                } else {
                    Stage::SourcesDeleted
                }
            } else if operation == Operation::Copy {
                Stage::CopyingTargets
            } else {
                Stage::DeletingSources
            },
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: target_key.clone(),
                size: 42,
                source_etag: "source-etag".into(),
                target_etag: Some("target-etag".into()),
                source_deleted: completed && operation != Operation::Copy,
            }],
            auth_tag: String::new(),
        };
        sign_transaction(&[0x31; 32], &mut transaction).unwrap();
        let journal = Arc::new(Mutex::new(Some(serde_json::to_vec(&transaction).unwrap())));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let final_write_started = Arc::new(tokio::sync::Notify::new());
        let final_write_release = Arc::new(tokio::sync::Notify::new());
        let write_started = final_write_started.clone();
        let write_release = final_write_release.clone();
        let stored = journal.clone();
        let observed = requests.clone();
        let journal_path = format!("/bucket/{journal_key}");
        let target_path = format!("/bucket/{target_key}");
        let trash_present = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let router = Router::new().route(
            "/{*key}",
            any(
                move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                    let journal = stored.clone();
                    let requests = observed.clone();
                    let journal_path = journal_path.clone();
                    let target_path = target_path.clone();
                    let trash_present = trash_present.clone();
                    let write_started = write_started.clone();
                    let write_release = write_release.clone();
                    async move {
                        requests
                            .lock()
                            .unwrap()
                            .push((method.clone(), uri.path().into()));
                        let response = Response::builder();
                        if uri.path() == journal_path {
                            if method == Method::GET {
                                let stored = journal.lock().unwrap();
                                let chunks = stored
                                    .as_ref()
                                    .unwrap()
                                    .chunks(13)
                                    .map(|chunk| {
                                        Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk))
                                    })
                                    .collect::<Vec<_>>();
                                return response
                                    .header("etag", "journal-etag")
                                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                                    .unwrap();
                            }
                            if method == Method::PUT {
                                assert_eq!(headers.get("if-match").unwrap(), "journal-etag");
                                if fault == CompletionFault::DelayedFinalWrite {
                                    write_started.notify_one();
                                    write_release.notified().await;
                                }
                                if fault == CompletionFault::FinalWrite {
                                    return response.status(403).body(Body::empty()).unwrap();
                                }
                                let updated: Transaction = serde_json::from_slice(&body).unwrap();
                                assert_eq!(
                                    updated.stage,
                                    if operation == Operation::Copy {
                                        Stage::CopyCompleted
                                    } else {
                                        Stage::SourcesDeleted
                                    }
                                );
                                validate_transaction(
                                    &[0x31; 32],
                                    "tenant/",
                                    journal_path.strip_prefix("/bucket/").unwrap(),
                                    &updated,
                                )
                                .unwrap();
                                *journal.lock().unwrap() = Some(body.to_vec());
                            } else if method == Method::DELETE {
                                assert_eq!(headers.get("if-match").unwrap(), "journal-etag");
                                *journal.lock().unwrap() = None;
                                return response.status(204).body(Body::empty()).unwrap();
                            } else {
                                assert_eq!(method, Method::HEAD);
                            }
                            return response
                                .status(if journal.lock().unwrap().is_some() {
                                    200
                                } else {
                                    404
                                })
                                .header("etag", "journal-etag")
                                .header("content-length", "0")
                                .body(Body::empty())
                                .unwrap();
                        }
                        if uri.path() == target_path {
                            if method == Method::DELETE {
                                assert_eq!(operation, Operation::Delete);
                                assert_eq!(headers.get("if-match").unwrap(), "target-etag");
                                if fault == CompletionFault::TrashDelete {
                                    return response.status(403).body(Body::empty()).unwrap();
                                }
                                trash_present.store(false, std::sync::atomic::Ordering::SeqCst);
                                return response.status(204).body(Body::empty()).unwrap();
                            }
                            assert_eq!(method, Method::HEAD);
                            return response
                                .status(
                                    if trash_present.load(std::sync::atomic::Ordering::SeqCst) {
                                        200
                                    } else {
                                        404
                                    },
                                )
                                .header("etag", "target-etag")
                                .header("content-length", "42")
                                .body(Body::empty())
                                .unwrap();
                        }
                        assert_eq!(method, Method::HEAD);
                        assert_eq!(uri.path(), "/bucket/tenant/source/file.bin");
                        response.status(404).body(Body::empty()).unwrap()
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        backend.recovery_runtime.journal_write_started(&journal_key);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            backend,
            journal_key,
            journal,
            requests,
            final_write_started,
            final_write_release,
            server,
        }
    }

    async fn finish(&self, completion: CompletionMode) -> AppResult<()> {
        self.finish_on(&self.backend, completion).await
    }

    async fn finish_on(&self, backend: &S3Backend, completion: CompletionMode) -> AppResult<()> {
        let mut transaction: Transaction =
            serde_json::from_slice(self.journal.lock().unwrap().as_ref().unwrap()).unwrap();
        backend
            .execute_directory_transaction(
                &self.journal_key,
                "journal-etag".into(),
                &mut transaction,
                completion,
            )
            .await
    }

    fn writes(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| *method == axum::http::Method::PUT)
            .count()
    }
}

#[tokio::test]
async fn final_directory_journal_write_is_not_dropped_by_cleanup_timeout_or_pause() {
    use std::{sync::Arc, time::Duration};
    let fixture = Arc::new(
        CompletionFixture::new(Operation::Move, false, CompletionFault::DelayedFinalWrite).await,
    );
    let pass = fixture.backend.scoped_work(None, None);
    let executing = fixture.clone();
    let waiter =
        tokio::spawn(async move { executing.finish_on(&pass, CompletionMode::Foreground).await });
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.final_write_started.notified(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !waiter.is_finished(),
        "cleanup timeout dropped an issued journal PUT"
    );
    fixture.backend.pause_maintenance();
    assert!(!waiter.is_finished());
    fixture.final_write_release.notify_one();
    waiter.await.unwrap().unwrap();
    let transaction: Transaction =
        serde_json::from_slice(fixture.journal.lock().unwrap().as_ref().unwrap()).unwrap();
    assert_eq!(transaction.stage, Stage::SourcesDeleted);
    assert!(fixture.backend.recovery_has_pending());
    assert_eq!(fixture.writes(), 1);

    fixture.backend.resume_maintenance();
    fixture.finish(CompletionMode::Recovery).await.unwrap();
    assert_eq!(fixture.writes(), 1);
    assert!(!fixture.backend.recovery_has_pending());
    assert!(fixture.journal.lock().unwrap().is_none());
}

#[tokio::test]
async fn directory_journal_read_preserves_chunked_manifests_and_maintenance_pause() {
    for operation in [Operation::Copy, Operation::Move, Operation::Delete] {
        let fixture = CompletionFixture::new(operation, true, CompletionFault::None).await;
        let expected: Transaction =
            serde_json::from_slice(fixture.journal.lock().unwrap().as_ref().unwrap()).unwrap();
        let (restored, etag) = fixture
            .backend
            .read_directory_transaction(&fixture.journal_key)
            .await
            .unwrap();
        assert_eq!(restored, expected);
        assert_eq!(etag, "journal-etag");
        fixture.backend.pause_maintenance();
        assert!(fixture
            .backend
            .read_directory_transaction(&fixture.journal_key)
            .await
            .is_err());
        assert_eq!(
            *fixture.requests.lock().unwrap(),
            vec![(
                axum::http::Method::GET,
                format!("/bucket/{}", fixture.journal_key)
            )]
        );
        assert!(fixture.journal.lock().unwrap().is_some());
        assert!(fixture.backend.recovery_has_pending());
    }
}

#[tokio::test]
async fn verified_directory_trash_deletion_uses_one_identity_read_and_one_confirmation() {
    let fixture = CompletionFixture::new(Operation::Delete, true, CompletionFault::None).await;
    fixture.finish(CompletionMode::Recovery).await.unwrap();
    let requests = fixture.requests.lock().unwrap();
    let trash = requests
        .iter()
        .filter(|(_, path)| path.contains("/directory-trash/"))
        .map(|(method, _)| method.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        trash,
        vec![
            axum::http::Method::HEAD,
            axum::http::Method::DELETE,
            axum::http::Method::HEAD
        ]
    );
    assert!(fixture.journal.lock().unwrap().is_none());
    assert!(!fixture.backend.recovery_has_pending());
}

#[tokio::test]
async fn directory_source_deletion_reuses_the_verified_snapshot_and_checkpoints_only_confirmation()
{
    use axum::{
        body::Body,
        http::{HeaderMap, Method, Response, Uri},
        routing::any,
        Router,
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    for size in [42, 43] {
        let requests = RequestLog::default();
        let observed = requests.clone();
        let present = Arc::new(AtomicBool::new(true));
        let stored = present.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri, headers: HeaderMap| {
                let requests = observed.clone();
                let present = stored.clone();
                async move {
                    assert_eq!(uri.path(), "/bucket/tenant/source/file.bin");
                    requests
                        .lock()
                        .unwrap()
                        .push((method.clone(), uri.path().into()));
                    let response = Response::builder();
                    if method == Method::DELETE {
                        assert_eq!(headers.get("if-match").unwrap(), "source-etag");
                        present.store(false, Ordering::SeqCst);
                        return response.status(204).body(Body::empty()).unwrap();
                    }
                    assert_eq!(method, Method::HEAD);
                    response
                        .status(if present.load(Ordering::SeqCst) {
                            200
                        } else {
                            404
                        })
                        .header("content-length", size.to_string())
                        .header("etag", "source-etag")
                        .body(Body::empty())
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: "fedcba9876543210fedcba9876543210".into(),
            operation: Operation::Move,
            source_relative: "source".into(),
            destination_relative: Some("target".into()),
            stage: Stage::DeletingSources,
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: "tenant/target/file.bin".into(),
                size: 42,
                source_etag: "source-etag".into(),
                target_etag: Some("target-etag".into()),
                source_deleted: false,
            }],
            auth_tag: String::new(),
        };
        let key = internal_key("tenant/", JOURNAL_CATEGORY, &transaction.id);
        let result = backend
            .delete_sources(&key, "journal-etag".into(), &mut transaction)
            .await;
        assert_eq!(result.is_ok(), size == 42);
        assert_eq!(transaction.objects[0].source_deleted, size == 42);
        assert_eq!(present.load(Ordering::SeqCst), size != 42);
        let methods = requests
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| method.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            methods,
            if size == 42 {
                vec![Method::HEAD, Method::DELETE, Method::HEAD]
            } else {
                vec![Method::HEAD]
            }
        );
        server.abort();
    }
}

#[tokio::test]
async fn inconsistent_directory_progress_stops_before_remote_work() {
    for operation in [Operation::Move, Operation::Delete] {
        for stage in [Stage::CopyingTargets, Stage::DeletingSources] {
            let fixture = CompletionFixture::new(operation, true, CompletionFault::None).await;
            let mut transaction: Transaction =
                serde_json::from_slice(fixture.journal.lock().unwrap().as_ref().unwrap()).unwrap();
            transaction.stage = stage;
            if stage == Stage::DeletingSources {
                transaction.objects[0].target_etag = None;
                transaction.objects[0].source_deleted = false;
            }
            sign_transaction(&[0x31; 32], &mut transaction).unwrap();
            let result = fixture
                .backend
                .execute_directory_transaction(
                    &fixture.journal_key,
                    "journal-etag".into(),
                    &mut transaction,
                    CompletionMode::Recovery,
                )
                .await;
            assert!(result.is_err(), "{operation:?}/{stage:?}");
            assert!(fixture.requests.lock().unwrap().is_empty());
            assert!(fixture.journal.lock().unwrap().is_some());
            assert!(fixture.backend.recovery_has_pending());
        }
    }
}

#[tokio::test]
async fn completed_directory_transactions_only_cleanup_without_rewriting_stage() {
    for operation in [Operation::Copy, Operation::Move, Operation::Delete] {
        for completion in [CompletionMode::Foreground, CompletionMode::Recovery] {
            let fixture =
                CompletionFixture::new(operation, true, CompletionFault::FinalWrite).await;
            fixture.finish(completion).await.unwrap();
            assert_eq!(fixture.writes(), 0);
            assert!(fixture.journal.lock().unwrap().is_none());
            assert!(!fixture.backend.recovery_has_pending());
            assert!(fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(_, path)| !path.contains("/source/") && !path.contains("/target/")));
        }
    }
}

#[tokio::test]
async fn directory_final_stage_is_persisted_once_and_failed_cleanup_remains_recoverable() {
    for operation in [Operation::Copy, Operation::Move, Operation::Delete] {
        let fixture = CompletionFixture::new(operation, false, CompletionFault::None).await;
        fixture.finish(CompletionMode::Recovery).await.unwrap();
        assert_eq!(fixture.writes(), 1);
        assert!(fixture.journal.lock().unwrap().is_none());
        assert!(!fixture.backend.recovery_has_pending());

        let fixture = CompletionFixture::new(operation, false, CompletionFault::FinalWrite).await;
        fixture.finish(CompletionMode::Foreground).await.unwrap();
        assert!(fixture.journal.lock().unwrap().is_some());
        assert!(fixture.backend.recovery_has_pending());
        let error = fixture.finish(CompletionMode::Recovery).await.unwrap_err();
        assert_eq!(error.operation().unwrap().commit, CommitState::Committed);
        assert_eq!(error.operation().unwrap().cleanup, CleanupState::Pending);
        assert!(fixture.backend.recovery_has_pending());
        assert_eq!(fixture.writes(), 2);
        assert!(fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| *method != axum::http::Method::DELETE));
    }
}

#[tokio::test]
async fn failed_directory_trash_delete_preserves_committed_result_and_journal() {
    let fixture =
        CompletionFixture::new(Operation::Delete, true, CompletionFault::TrashDelete).await;
    fixture.finish(CompletionMode::Foreground).await.unwrap();
    let error = fixture.finish(CompletionMode::Recovery).await.unwrap_err();
    assert_eq!(error.operation().unwrap().commit, CommitState::Committed);
    assert_eq!(error.operation().unwrap().cleanup, CleanupState::Pending);
    assert!(error.blocks_retry());
    assert_eq!(fixture.writes(), 0);
    assert!(fixture.journal.lock().unwrap().is_some());
    assert!(fixture.backend.recovery_has_pending());
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _)| *method == axum::http::Method::DELETE)
            .count(),
        2
    );
    assert!(requests
        .iter()
        .all(|(_, path)| path.contains("/directory-trash/")));
}

#[tokio::test]
async fn directory_recovery_uses_bounded_namespace_checked_listing() {
    use axum::{
        body::Body,
        extract::Query,
        http::{Method, Response},
        routing::any,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    for scenario in ["empty", "escaped", "invalid_id", "stalled", "missing_token"] {
        let listings = Arc::new(AtomicUsize::new(0));
        let other_requests = Arc::new(AtomicUsize::new(0));
        let observed_listings = listings.clone();
        let observed_other = other_requests.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, Query(query): Query<std::collections::HashMap<String, String>>| {
                let observed_listings = observed_listings.clone();
                let observed_other = observed_other.clone();
                async move {
                    if method != Method::GET
                        || query.get("list-type").map(String::as_str) != Some("2")
                    {
                        observed_other.fetch_add(1, Ordering::SeqCst);
                        return Response::builder()
                            .status(400)
                            .body(Body::empty())
                            .unwrap();
                    }
                    assert_eq!(
                        query.get("prefix").map(String::as_str),
                        Some("tenant/.ycloud-system/directory-transactions/")
                    );
                    let page = observed_listings.fetch_add(1, Ordering::SeqCst);
                    if page > 0 {
                        assert_eq!(
                            query.get("continuation-token").map(String::as_str),
                            Some("same-token")
                        );
                    }
                    let content = match scenario {
                        "escaped" => "<Contents><Key>other/.ycloud-system/directory-transactions/0123456789abcdef0123456789abcdef</Key></Contents>",
                        "invalid_id" => "<Contents><Key>tenant/.ycloud-system/directory-transactions/not-an-id</Key></Contents>",
                        _ => "",
                    };
                    let pagination = match scenario {
                        "stalled" => "<IsTruncated>true</IsTruncated><NextContinuationToken>same-token</NextContinuationToken>",
                        "missing_token" => "<IsTruncated>true</IsTruncated>",
                        _ => "<IsTruncated>false</IsTruncated>",
                    };
                    Response::builder()
                        .header("content-type", "application/xml")
                        .body(Body::from(format!(
                            "<ListBucketResult>{content}{pagination}</ListBucketResult>"
                        )))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let result = backend
            .recover_journal_category(crate::s3_backend::recovery::JournalKind::Directory)
            .await;
        if scenario == "empty" {
            assert_eq!(result.unwrap(), 0);
        } else {
            assert!(result.is_err(), "{scenario} must not allow recovery");
        }
        assert_eq!(other_requests.load(Ordering::SeqCst), 0, "{scenario}");
        assert_eq!(
            listings.load(Ordering::SeqCst),
            if scenario == "stalled" { 2 } else { 1 },
            "{scenario}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn verified_directory_delete_does_not_fail_or_settle_when_trash_changed() {
    use axum::{
        body::Body,
        http::{Method, Response, Uri},
        routing::any,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let deletions = Arc::new(AtomicUsize::new(0));
    let observed = deletions.clone();
    let router = Router::new().route(
        "/{*key}",
        any(move |method: Method, uri: Uri| {
            let observed = observed.clone();
            async move {
                let response = Response::builder();
                if method == Method::PUT {
                    response
                        .header("etag", "\"journal-etag\"")
                        .body(Body::empty())
                } else if method == Method::HEAD && uri.path().contains("/directory-trash/") {
                    // An external change to owned trash must not be deleted.
                    response
                        .header("content-length", "43")
                        .header("etag", "changed-etag")
                        .body(Body::empty())
                } else {
                    if method == Method::DELETE {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }
                    response.status(400).body(Body::empty())
                }
                .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
        "http://{}",
        listener.local_addr().unwrap()
    ));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let id = "fedcba9876543210fedcba9876543210";
    let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
    backend.recovery_runtime.journal_write_started(&key);
    let mut transaction = Transaction {
        schema_version: SCHEMA_VERSION,
        id: id.into(),
        operation: Operation::Delete,
        source_relative: "source".into(),
        destination_relative: None,
        stage: Stage::SourcesDeleted,
        objects: vec![ObjectRecord {
            source_key: "tenant/source/file.bin".into(),
            target_key: internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/file.bin")),
            size: 42,
            source_etag: "source-etag".into(),
            target_etag: Some("trash-etag".into()),
            source_deleted: true,
        }],
        auth_tag: String::new(),
    };
    sign_transaction(&[0x31; 32], &mut transaction).unwrap();
    backend
        .execute_directory_transaction(
            &key,
            "\"journal-etag\"".into(),
            &mut transaction,
            super::CompletionMode::Foreground,
        )
        .await
        .unwrap();
    assert!(backend.recovery_has_pending());
    let error = backend
        .execute_directory_transaction(
            &key,
            "\"journal-etag\"".into(),
            &mut transaction,
            super::CompletionMode::Recovery,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.operation().unwrap().commit,
        crate::error::CommitState::Committed
    );
    assert!(backend.recovery_has_pending());
    assert_eq!(deletions.load(Ordering::SeqCst), 0);
    server.abort();
}

#[test]
fn progress_is_checkpointed_in_bounded_batches() {
    assert!(!should_checkpoint_progress(0, 40));
    assert!(should_checkpoint_progress(15, 40));
    assert!(should_checkpoint_progress(31, 40));
    assert!(!should_checkpoint_progress(39, 40));
    assert!(!should_checkpoint_progress(15, 16));
    assert!(!should_checkpoint_progress(0, 1));
    assert!(!should_checkpoint_progress(0, 0));
    assert!(!should_checkpoint_progress(usize::MAX, usize::MAX));
}
