use super::*;
use axum::{
    body::Body,
    extract::Query,
    http::{Method, Response, Uri},
    routing::any,
    Router,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const CATEGORIES: [&str; 8] = [
    "activation-probe-intents",
    "multipart-sessions",
    "file-move-transactions",
    "transactions",
    "directory-transactions",
    "internal-upload-intents",
    "uploads",
    "backups",
];
const ID: &str = "0123456789abcdef0123456789abcdef";

struct RecoveryFixture {
    backend: S3Backend,
    requests: Arc<Mutex<Vec<(Method, String)>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for RecoveryFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl RecoveryFixture {
    async fn new(
        failed_category: Option<&'static str>,
        invalid_record: bool,
        unlisted_upload: bool,
    ) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let router = Router::new().route("/{*key}", any(
            move |method: Method, uri: Uri, Query(query): Query<HashMap<String, String>>| {
                let observed = observed.clone();
                async move {
                    let response = Response::builder();
                    if let Some(prefix) = query.get("prefix") {
                        assert_eq!(method, Method::GET);
                        observed.lock().unwrap().push((method, prefix.clone()));
                        let selected = failed_category.is_some_and(|category| {
                            prefix == &format!("tenant/.ycloud-system/{category}/")
                        });
                        if selected && !invalid_record {
                            return response.status(403).body(Body::empty()).unwrap();
                        }
                        let content = if selected {
                            format!("<Contents><Key>{prefix}{ID}</Key></Contents>")
                        } else {
                            String::new()
                        };
                        return response.header("content-type", "application/xml")
                            .body(Body::from(format!(
                                "<ListBucketResult>{content}<IsTruncated>false</IsTruncated></ListBucketResult>"
                            ))).unwrap();
                    }
                    observed.lock().unwrap().push((method.clone(), uri.path().to_owned()));
                    if method == Method::HEAD {
                        let selected_record = invalid_record && failed_category.is_some_and(|category| {
                            uri.path() == format!("/bucket/tenant/.ycloud-system/{category}/{ID}")
                        });
                        let dependent_upload = unlisted_upload
                            && uri.path() == format!("/bucket/tenant/.ycloud-system/transactions/{ID}");
                        if selected_record || dependent_upload {
                            return response.header("etag", "journal-etag")
                                .header("content-length", 2).body(Body::empty()).unwrap();
                        }
                        return response.status(404).body(Body::empty()).unwrap();
                    }
                    assert_eq!(method, Method::GET, "untrusted record must not issue writes");
                    response.header("etag", "journal-etag")
                        .body(Body::from("{}")).unwrap()
                }
            }
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            backend,
            requests,
            server,
        }
    }
}

#[tokio::test]
async fn startup_recovery_keeps_dependency_order_and_read_only_orphan_inventory() {
    let fixture = RecoveryFixture::new(None, false, false).await;
    assert_eq!(fixture.backend.recover_transactions().await.unwrap(), 0);
    let expected =
        CATEGORIES.map(|category| (Method::GET, format!("tenant/.ycloud-system/{category}/")));
    assert_eq!(
        fixture.requests.lock().unwrap().as_slice(),
        expected.as_slice()
    );
    assert!(fixture.backend.recovery_gate.try_write().is_ok());
    assert!(fixture.backend.mutation_gate.try_lock().is_ok());
}

#[tokio::test]
async fn startup_listing_failure_stops_before_later_dependency_categories() {
    for (index, category) in CATEGORIES.into_iter().enumerate() {
        let fixture = RecoveryFixture::new(Some(category), false, false).await;
        assert!(
            fixture.backend.recover_transactions().await.is_err(),
            "{category}"
        );
        let expected = CATEGORIES[..=index]
            .iter()
            .map(|category| (Method::GET, format!("tenant/.ycloud-system/{category}/")))
            .collect::<Vec<_>>();
        assert_eq!(*fixture.requests.lock().unwrap(), expected, "{category}");
        assert!(fixture.backend.recovery_gate.try_write().is_ok());
        assert!(fixture.backend.mutation_gate.try_lock().is_ok());
    }
}

#[tokio::test]
async fn startup_recovery_authenticates_each_kind_before_any_object_mutation() {
    for (index, category) in CATEGORIES[..6].iter().copied().enumerate() {
        let fixture = RecoveryFixture::new(Some(category), true, false).await;
        assert!(
            fixture.backend.recover_transactions().await.is_err(),
            "{category}"
        );
        let requests = fixture.requests.lock().unwrap();
        assert!(requests
            .iter()
            .all(|(method, _)| *method == Method::GET || *method == Method::HEAD));
        let listings = requests
            .iter()
            .filter(|(_, path)| !path.starts_with('/'))
            .count();
        assert_eq!(listings, index + 1, "{category}");
        assert_eq!(
            requests
                .iter()
                .filter(|(method, path)| {
                    *method == Method::GET
                        && path == &format!("/bucket/tenant/.ycloud-system/{category}/{ID}")
                })
                .count(),
            1,
            "{category}"
        );
    }
}

#[tokio::test]
async fn online_recovery_dispatches_each_kind_without_a_category_scan() {
    for category in &CATEGORIES[..6] {
        let fixture = RecoveryFixture::new(Some(category), true, false).await;
        let key = internal_key("tenant/", category, ID);
        fixture.backend.recovery_runtime.journal_write_started(&key);
        assert!(
            fixture
                .backend
                .recover_runtime_transactions(&crate::capacity::CapacityTracker::new(None, 0))
                .await
                .is_err(),
            "{category}"
        );
        assert_eq!(
            fixture.backend.recovery_runtime.pending_keys(),
            vec![key.clone()]
        );
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests.iter().all(|(method, path)| {
                path.starts_with('/') && (*method == Method::GET || *method == Method::HEAD)
            }),
            "{category}"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(method, path)| {
                    *method == Method::GET && path == &format!("/bucket/{key}")
                })
                .count(),
            1,
            "{category}"
        );
    }
}

#[tokio::test]
async fn startup_intent_cleanup_checks_upload_dependency_even_when_listing_omits_it() {
    let fixture = RecoveryFixture::new(Some("internal-upload-intents"), true, true).await;
    let key = internal_key("tenant/", "internal-upload-intents", ID);
    fixture.backend.recovery_runtime.journal_write_started(&key);
    let error = fixture.backend.recover_transactions().await.unwrap_err();
    assert!(error.to_string().contains("关联事务待恢复"));
    assert_eq!(fixture.backend.recovery_runtime.pending_keys(), vec![key]);
    let requests = fixture.requests.lock().unwrap();
    let expected = CATEGORIES[..6]
        .iter()
        .map(|category| (Method::GET, format!("tenant/.ycloud-system/{category}/")))
        .chain(std::iter::once((
            Method::HEAD,
            format!("/bucket/tenant/.ycloud-system/transactions/{ID}"),
        )))
        .collect::<Vec<_>>();
    assert_eq!(*requests, expected);
}

#[test]
fn runtime_journal_classification_preserves_dependencies_and_exact_namespace() {
    let ordered = [
        ("activation-probe-intents", JournalKind::ActivationProbe),
        ("multipart-sessions", JournalKind::Multipart),
        ("file-move-transactions", JournalKind::FileMove),
        ("transactions", JournalKind::Upload),
        ("directory-transactions", JournalKind::Directory),
        ("internal-upload-intents", JournalKind::InternalUpload),
    ];
    assert_eq!(
        JournalKind::ORDERED.map(JournalKind::category),
        CATEGORIES[..6]
    );
    assert!(JournalKind::ORDERED
        .windows(2)
        .all(|pair| pair[0] < pair[1]));
    for (category, expected) in ordered {
        assert_eq!(
            runtime_journal_kind("tenant/", &internal_key("tenant/", category, ID)).unwrap(),
            expected
        );
    }
    assert!(ordered.windows(2).all(|pair| pair[0].1 < pair[1].1));
    for key in [
        internal_key("other/", "transactions", ID),
        internal_key("tenant/", "uploads", ID),
        internal_key("tenant/", "transactions", "missing-id"),
        internal_key("tenant/", "transactions", &format!("nested/{ID}")),
        format!("tenant/.ycloud-system-extra/transactions/{ID}"),
    ] {
        assert!(runtime_journal_kind("tenant/", &key).is_err(), "{key}");
    }
}
