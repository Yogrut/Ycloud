use crate::{
    error::{CleanupState, CommitState},
    s3_backend::{internal_key, S3Backend},
};
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scenario {
    Success,
    ResponseLost,
    Retained,
    Rejected,
    Replaced,
    ConfirmationDenied,
    InitiallyAbsent,
    InitiallyReplaced,
    OssSuccess,
    OssChangedBeforeDelete,
}

struct DeleteFixture {
    backend: S3Backend,
    key: String,
    heads: Arc<AtomicUsize>,
    deletes: Arc<AtomicUsize>,
    present: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for DeleteFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl DeleteFixture {
    async fn new(scenario: Scenario) -> Self {
        let key = internal_key(
            "tenant/",
            "transactions",
            "0123456789abcdef0123456789abcdef",
        );
        let path = format!("/bucket/{key}");
        let heads = Arc::new(AtomicUsize::new(0));
        let deletes = Arc::new(AtomicUsize::new(0));
        let present = Arc::new(AtomicBool::new(scenario != Scenario::InitiallyAbsent));
        let observed_heads = heads.clone();
        let observed_deletes = deletes.clone();
        let stored = present.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri, headers: HeaderMap| {
                let heads = observed_heads.clone();
                let deletes = observed_deletes.clone();
                let present = stored.clone();
                let path = path.clone();
                async move {
                    assert_eq!(uri.path(), path);
                    let response = Response::builder();
                    if method == Method::DELETE {
                        if scenario == Scenario::OssSuccess {
                            assert!(headers.get("if-match").is_none());
                        } else {
                            assert_eq!(headers.get("if-match").unwrap(), "owned-etag");
                        }
                        deletes.fetch_add(1, Ordering::SeqCst);
                        if matches!(
                            scenario,
                            Scenario::Success
                                | Scenario::ResponseLost
                                | Scenario::ConfirmationDenied
                                | Scenario::OssSuccess
                        ) {
                            present.store(false, Ordering::SeqCst);
                        }
                        return response
                            .status(
                                if matches!(
                                    scenario,
                                    Scenario::ResponseLost
                                        | Scenario::Rejected
                                        | Scenario::Replaced
                                ) {
                                    403
                                } else {
                                    204
                                },
                            )
                            .body(Body::empty())
                            .unwrap();
                    }
                    assert_eq!(method, Method::HEAD);
                    let pass = heads.fetch_add(1, Ordering::SeqCst);
                    let deleted = deletes.load(Ordering::SeqCst) > 0;
                    if deleted && scenario == Scenario::ConfirmationDenied {
                        return response.status(403).body(Body::empty()).unwrap();
                    }
                    response
                        .status(if present.load(Ordering::SeqCst) {
                            200
                        } else {
                            404
                        })
                        .header("content-length", "42")
                        .header(
                            "etag",
                            if scenario == Scenario::InitiallyReplaced
                                || (deleted && scenario == Scenario::Replaced)
                                || (scenario == Scenario::OssChangedBeforeDelete && pass > 0)
                            {
                                "foreign-etag"
                            } else {
                                "owned-etag"
                            },
                        )
                        .body(Body::empty())
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        if matches!(
            scenario,
            Scenario::OssSuccess | Scenario::OssChangedBeforeDelete
        ) {
            backend.provider = crate::config::S3Provider::AlibabaOss;
        }
        backend.recovery_runtime.journal_write_started(&key);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            backend,
            key,
            heads,
            deletes,
            present,
            server,
        }
    }
}

#[tokio::test]
async fn confirmed_deletion_settles_only_proven_absence_and_preserves_changed_objects() {
    for scenario in [
        Scenario::Success,
        Scenario::ResponseLost,
        Scenario::Retained,
        Scenario::Rejected,
        Scenario::Replaced,
        Scenario::ConfirmationDenied,
        Scenario::InitiallyAbsent,
        Scenario::InitiallyReplaced,
    ] {
        let fixture = DeleteFixture::new(scenario).await;
        let result = fixture
            .backend
            .delete_key_confirmed(&fixture.key, Some("owned-etag"))
            .await;
        let settled = matches!(
            scenario,
            Scenario::Success | Scenario::ResponseLost | Scenario::InitiallyAbsent
        );
        assert_eq!(result.is_ok(), settled, "{scenario:?}");
        assert_eq!(
            fixture.backend.recovery_has_pending(),
            !settled,
            "{scenario:?}"
        );
        let initially_unavailable = matches!(
            scenario,
            Scenario::InitiallyAbsent | Scenario::InitiallyReplaced
        );
        assert_eq!(
            fixture.deletes.load(Ordering::SeqCst),
            usize::from(!initially_unavailable),
            "{scenario:?}"
        );
        assert_eq!(
            fixture.heads.load(Ordering::SeqCst),
            if initially_unavailable { 1 } else { 2 },
            "{scenario:?}"
        );
        if !settled && scenario != Scenario::InitiallyReplaced {
            let outcome = result.unwrap_err().operation().unwrap();
            assert_eq!(
                outcome.commit,
                if scenario == Scenario::Rejected {
                    CommitState::NotCommitted
                } else {
                    CommitState::Unknown
                },
                "{scenario:?}"
            );
            assert_eq!(outcome.cleanup, CleanupState::Pending);
        }
        if matches!(
            scenario,
            Scenario::Retained
                | Scenario::Rejected
                | Scenario::Replaced
                | Scenario::InitiallyReplaced
        ) {
            assert!(fixture.present.load(Ordering::SeqCst), "{scenario:?}");
        }
    }
}

#[tokio::test]
async fn oss_deletion_keeps_its_native_identity_recheck_and_absence_confirmation() {
    for scenario in [Scenario::OssSuccess, Scenario::OssChangedBeforeDelete] {
        let fixture = DeleteFixture::new(scenario).await;
        let result = fixture
            .backend
            .delete_key_confirmed(&fixture.key, Some("owned-etag"))
            .await;
        assert_eq!(result.is_ok(), scenario == Scenario::OssSuccess);
        assert_eq!(fixture.heads.load(Ordering::SeqCst), 3);
        assert_eq!(
            fixture.deletes.load(Ordering::SeqCst),
            usize::from(scenario == Scenario::OssSuccess)
        );
        assert_eq!(
            fixture.present.load(Ordering::SeqCst),
            scenario != Scenario::OssSuccess
        );
        assert_eq!(
            fixture.backend.recovery_has_pending(),
            scenario != Scenario::OssSuccess
        );
        if let Err(error) = result {
            assert_eq!(error.operation().unwrap().commit, CommitState::Unknown);
        }
    }
}

#[tokio::test]
async fn maintenance_pause_prevents_confirmed_deletion_before_any_request() {
    let fixture = DeleteFixture::new(Scenario::Success).await;
    fixture.backend.pause_maintenance();
    assert!(fixture
        .backend
        .delete_key_confirmed(&fixture.key, Some("owned-etag"))
        .await
        .is_err());
    assert_eq!(fixture.heads.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.deletes.load(Ordering::SeqCst), 0);
    assert!(fixture.backend.recovery_has_pending());
    assert!(fixture.present.load(Ordering::SeqCst));
}
