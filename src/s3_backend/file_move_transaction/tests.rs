use super::*;
use crate::s3_backend::RawS3Metadata;

struct MoveFixture {
    backend: S3Backend,
    journal_key: String,
    source_heads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    target_heads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    source_deletes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    journal_present: std::sync::Arc<std::sync::atomic::AtomicBool>,
    source_present: std::sync::Arc<std::sync::atomic::AtomicBool>,
    delete_started: std::sync::Arc<tokio::sync::Notify>,
    delete_release: std::sync::Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for MoveFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl MoveFixture {
    async fn new(scenario: &'static str) -> Self {
        use axum::{
            body::Body,
            http::{HeaderMap, Method, Response, Uri},
            routing::any,
            Router,
        };
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        let value = transaction(Stage::DestinationCopied);
        let journal_key = internal_key("tenant/", JOURNAL_CATEGORY, &value.id);
        let source_heads = Arc::new(AtomicUsize::new(0));
        let target_heads = Arc::new(AtomicUsize::new(0));
        let source_deletes = Arc::new(AtomicUsize::new(0));
        let journal_present = Arc::new(AtomicBool::new(true));
        let source_present = Arc::new(AtomicBool::new(scenario != "already_absent"));
        let delete_started = Arc::new(tokio::sync::Notify::new());
        let delete_release = Arc::new(tokio::sync::Notify::new());
        let started = delete_started.clone();
        let release = delete_release.clone();
        let journal_path = format!("/bucket/{journal_key}");
        let observed_source = source_heads.clone();
        let observed_target = target_heads.clone();
        let observed_deletes = source_deletes.clone();
        let stored_journal = journal_present.clone();
        let stored_source = source_present.clone();
        let confirmation_heads = Arc::new(AtomicUsize::new(0));
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri, headers: HeaderMap| {
                let source_heads = observed_source.clone();
                let target_heads = observed_target.clone();
                let source_deletes = observed_deletes.clone();
                let journal_present = stored_journal.clone();
                let source_present = stored_source.clone();
                let journal_path = journal_path.clone();
                let confirmation_heads = confirmation_heads.clone();
                let started = started.clone();
                let release = release.clone();
                async move {
                    let response = Response::builder();
                    if uri.path() == journal_path {
                        if method == Method::DELETE {
                            assert_eq!(headers.get("if-match").unwrap(), "journal-etag");
                            journal_present.store(false, Ordering::SeqCst);
                            return response.status(204).body(Body::empty()).unwrap();
                        }
                        assert_eq!(method, Method::HEAD, "copied stage needs no journal update");
                        return response
                            .status(if journal_present.load(Ordering::SeqCst) {
                                200
                            } else {
                                404
                            })
                            .header("etag", "journal-etag")
                            .header("content-length", "0")
                            .body(Body::empty())
                            .unwrap();
                    }
                    if uri.path() == "/bucket/tenant/folder/destination.bin" {
                        assert_eq!(
                            method,
                            Method::HEAD,
                            "copied target must not be copied again"
                        );
                        target_heads.fetch_add(1, Ordering::SeqCst);
                        return response
                            .header("content-length", "42")
                            .header(
                                "etag",
                                if scenario == "target_changed" {
                                    "foreign-etag"
                                } else {
                                    "destination-etag"
                                },
                            )
                            .body(Body::empty())
                            .unwrap();
                    }
                    assert_eq!(uri.path(), "/bucket/tenant/source.bin");
                    if method == Method::DELETE {
                        assert_eq!(headers.get("if-match").unwrap(), "source-etag");
                        source_deletes.fetch_add(1, Ordering::SeqCst);
                        if scenario == "delayed_source_delete" {
                            started.notify_one();
                            release.notified().await;
                        }
                        if !matches!(scenario, "retained" | "rejected" | "replaced") {
                            source_present.store(false, Ordering::SeqCst);
                        }
                        return response
                            .status(
                                if matches!(scenario, "lost_response" | "rejected" | "replaced") {
                                    403
                                } else {
                                    204
                                },
                            )
                            .body(Body::empty())
                            .unwrap();
                    }
                    assert_eq!(method, Method::HEAD);
                    source_heads.fetch_add(1, Ordering::SeqCst);
                    let deleted = source_deletes.load(Ordering::SeqCst) > 0;
                    if deleted {
                        let pass = confirmation_heads.fetch_add(1, Ordering::SeqCst);
                        if scenario == "verification_unavailable"
                            || (scenario == "lost_response" && pass > 0)
                        {
                            return response.status(403).body(Body::empty()).unwrap();
                        }
                    }
                    response
                        .status(if source_present.load(Ordering::SeqCst) {
                            200
                        } else {
                            404
                        })
                        .header("content-length", "42")
                        .header(
                            "etag",
                            if scenario == "source_changed" || (scenario == "replaced" && deleted) {
                                "foreign-etag"
                            } else {
                                "source-etag"
                            },
                        )
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
        backend.recovery_runtime.journal_write_started(&journal_key);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            backend,
            journal_key,
            source_heads,
            target_heads,
            source_deletes,
            journal_present,
            source_present,
            delete_started,
            delete_release,
            server,
        }
    }

    async fn run(&self) -> AppResult<()> {
        self.run_on(&self.backend).await
    }

    async fn run_on(&self, backend: &S3Backend) -> AppResult<()> {
        let mut value = transaction(Stage::DestinationCopied);
        value.destination = Some(ObjectSnapshot {
            size: 42,
            etag: "destination-etag".into(),
        });
        backend
            .execute_file_move_transaction(
                &self.journal_key,
                Some("journal-etag".into()),
                &mut value,
                CompletionMode::Recovery,
            )
            .await
    }
}

#[tokio::test]
async fn cancelled_move_recovery_waits_for_sent_source_delete_and_preserves_its_journal() {
    use std::{
        sync::{atomic::Ordering, Arc},
        time::Duration,
    };
    let fixture = Arc::new(MoveFixture::new("delayed_source_delete").await);
    let stop = tokio_util::sync::CancellationToken::new();
    let pass = fixture.backend.scoped_work(None, Some(stop.clone()));
    let executing = fixture.clone();
    let waiter = tokio::spawn(async move { executing.run_on(&pass).await });
    tokio::time::timeout(Duration::from_secs(3), fixture.delete_started.notified())
        .await
        .unwrap();
    stop.cancel();
    assert!(!waiter.is_finished());
    fixture.delete_release.notify_one();
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(error.operation().unwrap().commit, CommitState::Unknown);
    assert!(!fixture.source_present.load(Ordering::SeqCst));
    assert!(fixture.journal_present.load(Ordering::SeqCst));
    assert!(fixture.backend.recovery_has_pending());
    assert_eq!(fixture.source_deletes.load(Ordering::SeqCst), 1);

    // The next pass confirms the missing source and existing destination; it
    // settles only the journal and must not issue the source DELETE again.
    fixture.run().await.unwrap();
    assert_eq!(fixture.source_deletes.load(Ordering::SeqCst), 1);
    assert!(!fixture.journal_present.load(Ordering::SeqCst));
    assert!(!fixture.backend.recovery_has_pending());
}

#[tokio::test]
async fn copied_move_stage_reads_only_the_metadata_needed_for_completion() {
    use std::sync::atomic::Ordering;
    let fixture = MoveFixture::new("already_absent").await;
    fixture.run().await.unwrap();
    assert_eq!(fixture.source_heads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.target_heads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.source_deletes.load(Ordering::SeqCst), 0);
    assert!(!fixture.journal_present.load(Ordering::SeqCst));
    assert!(!fixture.backend.recovery_has_pending());
}

#[tokio::test]
async fn move_source_deletion_is_decided_by_one_confirmation_not_its_response() {
    use std::sync::atomic::Ordering;
    for scenario in [
        "success",
        "lost_response",
        "retained",
        "rejected",
        "replaced",
        "verification_unavailable",
        "source_changed",
        "target_changed",
    ] {
        let fixture = MoveFixture::new(scenario).await;
        let result = fixture.run().await;
        let committed = matches!(scenario, "success" | "lost_response");
        if committed {
            result.unwrap();
            assert!(!fixture.source_present.load(Ordering::SeqCst));
            assert!(!fixture.backend.recovery_has_pending());
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.operation().unwrap().commit,
                CommitState::Unknown,
                "{scenario}"
            );
            assert!(error.blocks_retry());
            assert!(fixture.backend.recovery_has_pending());
            if scenario != "verification_unavailable" {
                assert!(fixture.source_present.load(Ordering::SeqCst), "{scenario}");
            }
        }
        assert_eq!(
            fixture.journal_present.load(Ordering::SeqCst),
            !committed,
            "{scenario}"
        );
        assert_eq!(
            fixture.source_deletes.load(Ordering::SeqCst),
            usize::from(!matches!(scenario, "source_changed" | "target_changed")),
            "{scenario}"
        );
        assert_eq!(
            fixture.source_heads.load(Ordering::SeqCst),
            match scenario {
                "target_changed" => 0,
                "source_changed" => 1,
                _ => 2,
            },
            "{scenario}"
        );
    }
}

fn transaction(stage: Stage) -> Transaction {
    Transaction {
        schema_version: SCHEMA_VERSION,
        id: "0123456789abcdef0123456789abcdef".into(),
        source_relative: "source.bin".into(),
        destination_relative: "folder/destination.bin".into(),
        stage,
        source: ObjectSnapshot {
            size: 42,
            etag: "source-etag".into(),
        },
        destination: None,
    }
}

#[test]
fn move_records_require_exact_stage_snapshots_and_namespace() {
    let mut value = transaction(Stage::Prepared);
    let key = internal_key("tenant/", JOURNAL_CATEGORY, &value.id);
    assert!(validate_transaction("tenant/", &key, &value).is_ok());

    value.stage = Stage::DestinationCopied;
    assert!(validate_transaction("tenant/", &key, &value).is_err());
    value.destination = Some(ObjectSnapshot {
        size: 42,
        etag: "destination-etag".into(),
    });
    assert!(validate_transaction("tenant/", &key, &value).is_ok());

    value.source_relative = "../outside".into();
    assert!(validate_transaction("tenant/", &key, &value).is_err());
    value.source_relative = "source.bin".into();
    assert!(validate_transaction("tenant/", "other/key", &value).is_err());
}

#[test]
fn prepared_move_accepts_only_source_etag_or_operation_marker() {
    let value = transaction(Stage::Prepared);
    let mut metadata = RawS3Metadata {
        size: 42,
        etag: Some("source-etag".into()),
        content_type: None,
        operation_id: None,
    };
    assert!(prepared_destination_matches(&metadata, &value));

    metadata.etag = Some("multipart-etag".into());
    assert!(!prepared_destination_matches(&metadata, &value));
    metadata.operation_id = Some(value.id.clone());
    assert!(prepared_destination_matches(&metadata, &value));
    metadata.size = 41;
    assert!(!prepared_destination_matches(&metadata, &value));
}

#[test]
fn copied_move_record_requires_matching_size_and_preserves_its_wire_format() {
    let mut value = transaction(Stage::DestinationCopied);
    value.destination = Some(ObjectSnapshot {
        size: 42,
        etag: "destination-etag".into(),
    });
    let key = internal_key("tenant/", JOURNAL_CATEGORY, &value.id);
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({
            "schema_version": 1,
            "id": "0123456789abcdef0123456789abcdef",
            "source_relative": "source.bin",
            "destination_relative": "folder/destination.bin",
            "stage": "destination_copied",
            "source": { "size": 42, "etag": "source-etag" },
            "destination": { "size": 42, "etag": "destination-etag" }
        })
    );
    let mut decoded: Transaction = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, value);
    assert!(validate_transaction("tenant/", &key, &decoded).is_ok());
    decoded.destination.as_mut().unwrap().size = 41;
    assert!(validate_transaction("tenant/", &key, &decoded).is_err());
}
