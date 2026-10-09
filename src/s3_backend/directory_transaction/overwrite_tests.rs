use super::*;
use axum::{
    body::{Body, Bytes},
    extract::Query,
    http::{HeaderMap, Method, Response, Uri},
    routing::any,
    Router,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    etag: String,
    operation: Option<String>,
}

struct Fixture {
    backend: S3Backend,
    objects: Arc<Mutex<HashMap<String, Object>>>,
    fail_copy: Arc<AtomicBool>,
    fail_cleanup: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(source_dir: bool, target_dir: bool) -> Self {
        let objects = Arc::new(Mutex::new(HashMap::<String, Object>::new()));
        for (key, bytes) in [
            (
                if source_dir {
                    "tenant/source/new.txt"
                } else {
                    "tenant/source"
                },
                b"new".to_vec(),
            ),
            (
                if target_dir {
                    "tenant/target/old-only.txt"
                } else {
                    "tenant/target"
                },
                b"previous".to_vec(),
            ),
        ] {
            objects.lock().unwrap().insert(
                key.into(),
                Object {
                    bytes,
                    etag: format!("\"{key}\""),
                    operation: None,
                },
            );
        }
        let fail_copy = Arc::new(AtomicBool::new(false));
        let fail_cleanup = Arc::new(AtomicBool::new(false));
        let serial = Arc::new(AtomicUsize::new(1));
        let shared = objects.clone();
        let denied_copy = fail_copy.clone();
        let denied_cleanup = fail_cleanup.clone();
        let router = Router::new().route("/bucket/", any({
            let shared = shared.clone();
            move |Query(query): Query<HashMap<String, String>>| {
                let objects = shared.clone();
                async move {
                    let prefix = query.get("prefix").cloned().unwrap_or_default();
                    let objects = objects.lock().unwrap();
                    let mut listed = objects.iter().filter(|(key, _)| key.starts_with(&prefix)).collect::<Vec<_>>();
                    listed.sort_by_key(|(key, _)| *key);
                    let body = listed.iter().map(|(key, object)| format!("<Contents><Key>{key}</Key><Size>{}</Size><ETag>{}</ETag></Contents>", object.bytes.len(), object.etag)).collect::<String>();
                    Response::builder().header("content-type", "application/xml").body(Body::from(format!("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><KeyCount>{}</KeyCount><IsTruncated>false</IsTruncated>{body}</ListBucketResult>", listed.len()))).unwrap()
                }
            }
        })).route("/bucket/{*key}", any(move |method: Method, uri: Uri, headers: HeaderMap, bytes: Bytes| {
            let objects = shared.clone();
            let serial = serial.clone();
            let fail_copy = denied_copy.clone();
            let fail_cleanup = denied_cleanup.clone();
            async move {
                let key = uri.path().strip_prefix("/bucket/").unwrap().to_owned();
                let mut objects = objects.lock().unwrap();
                let response = Response::builder();
                if method == Method::HEAD || method == Method::GET {
                    let Some(object) = objects.get(&key) else { return response.status(404).body(Body::empty()).unwrap(); };
                    let mut response = response.header("etag", &object.etag).header("content-length", object.bytes.len());
                    if let Some(operation) = &object.operation { response = response.header(format!("x-amz-meta-{}", crate::s3_backend::S3_OPERATION_METADATA_KEY), operation); }
                    return response.body(if method == Method::HEAD { Body::empty() } else { Body::from(object.bytes.clone()) }).unwrap();
                }
                if let Some(expected) = headers.get("if-match") {
                    if objects.get(&key).is_none_or(|object| expected.to_str().unwrap() != object.etag) {
                        return response.status(412).body(Body::empty()).unwrap();
                    }
                }
                if method == Method::DELETE {
                    if key.contains("/directory-trash/") && fail_cleanup.load(Ordering::Relaxed) { return response.status(403).body(Body::empty()).unwrap(); }
                    objects.remove(&key);
                    return response.status(204).body(Body::empty()).unwrap();
                }
                assert_eq!(method, Method::PUT);
                if headers.get("if-none-match").is_some() && objects.contains_key(&key) {
                    return response.status(412).body(Body::empty()).unwrap();
                }
                let copied = headers.get("x-amz-copy-source").map(|value| value.to_str().unwrap().trim_start_matches('/').strip_prefix("bucket/").unwrap().to_owned());
                if key.starts_with("tenant/target") && copied.is_some() && fail_copy.swap(false, Ordering::Relaxed) {
                    return response.status(403).body(Body::empty()).unwrap();
                }
                let content = if let Some(source) = copied.as_ref() {
                    let Some(object) = objects.get(source) else { return response.status(404).body(Body::empty()).unwrap(); };
                    if let Some(expected) = headers.get("x-amz-copy-source-if-match") { assert_eq!(expected.to_str().unwrap(), object.etag); }
                    object.bytes.clone()
                } else { bytes.to_vec() };
                let etag = format!("\"etag-{}\"", serial.fetch_add(1, Ordering::Relaxed));
                let operation = headers.get(format!("x-amz-meta-{}", crate::s3_backend::S3_OPERATION_METADATA_KEY)).map(|value| value.to_str().unwrap().to_owned());
                objects.insert(key, Object { bytes: content, etag: etag.clone(), operation });
                if copied.is_some() {
                    response.header("content-type", "application/xml").body(Body::from(format!("<CopyObjectResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><ETag>{etag}</ETag></CopyObjectResult>"))).unwrap()
                } else { response.header("etag", etag).body(Body::empty()).unwrap() }
            }
        }));
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
            objects,
            fail_copy,
            fail_cleanup,
            server,
        }
    }

    fn journal(&self) -> String {
        self.objects
            .lock()
            .unwrap()
            .keys()
            .find(|key| key.contains("/directory-transactions/"))
            .unwrap()
            .clone()
    }
    fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .map(|object| object.bytes.clone())
    }
}

#[tokio::test]
async fn s3_copy_and_move_replace_files_and_directories_without_leaving_old_children() {
    for copy in [false, true] {
        for (source_dir, target_dir) in [(false, false), (true, true), (false, true), (true, false)]
        {
            let fixture = Fixture::new(source_dir, target_dir).await;
            let result = fixture
                .backend
                .transfer_path("source", "target", copy, true, 3)
                .await
                .unwrap();
            assert!(!result.created);
            assert_eq!(result.previous_size, 8);
            let target = if source_dir {
                "tenant/target/new.txt"
            } else {
                "tenant/target"
            };
            assert_eq!(fixture.bytes(target).unwrap(), b"new");
            assert!(fixture.bytes("tenant/target/old-only.txt").is_none());
            let source = if source_dir {
                "tenant/source/new.txt"
            } else {
                "tenant/source"
            };
            assert_eq!(fixture.bytes(source).is_some(), copy);
            assert!(!fixture
                .objects
                .lock()
                .unwrap()
                .keys()
                .any(|key| key.contains("/directory-trash/")
                    || key.contains("/directory-transactions/")));
        }
    }
}

#[tokio::test]
async fn s3_no_overwrite_is_a_precondition_failure_without_writes() {
    for copy in [false, true] {
        let fixture = Fixture::new(false, false).await;
        let error = fixture
            .backend
            .transfer_path("source", "target", copy, false, 3)
            .await
            .unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::PRECONDITION_FAILED);
        assert_eq!(fixture.bytes("tenant/target").unwrap(), b"previous");
        assert_eq!(fixture.objects.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn interrupted_s3_replacement_keeps_old_bytes_and_recovers_the_same_operation() {
    for copy in [false, true] {
        let fixture = Fixture::new(false, false).await;
        fixture.fail_copy.store(true, Ordering::Relaxed);
        let error = fixture
            .backend
            .transfer_path("source", "target", copy, true, 3)
            .await
            .unwrap_err();
        assert_eq!(error.operation().unwrap().commit, CommitState::Unknown);
        assert_eq!(fixture.bytes("tenant/source").unwrap(), b"new");
        assert!(fixture
            .objects
            .lock()
            .unwrap()
            .iter()
            .any(|(key, object)| key.contains("/directory-trash/") && object.bytes == b"previous"));
        let journal = fixture.journal();
        fixture
            .backend
            .recover_directory_transaction(&journal)
            .await
            .unwrap();
        assert_eq!(fixture.bytes("tenant/target").unwrap(), b"new");
        assert_eq!(fixture.bytes("tenant/source").is_some(), copy);
        assert!(fixture.bytes(&journal).is_none());
    }
}

#[tokio::test]
async fn s3_verified_copy_does_not_fail_due_to_old_target_cleanup() {
    let fixture = Fixture::new(false, false).await;
    fixture.fail_cleanup.store(true, Ordering::Relaxed);
    fixture
        .backend
        .transfer_path("source", "target", true, true, 3)
        .await
        .unwrap();
    assert_eq!(fixture.bytes("tenant/target").unwrap(), b"new");
    let journal = fixture.journal();
    fixture.fail_cleanup.store(false, Ordering::Relaxed);
    fixture
        .backend
        .recover_directory_transaction(&journal)
        .await
        .unwrap();
    assert_eq!(fixture.bytes("tenant/target").unwrap(), b"new");
    assert!(fixture.bytes(&journal).is_none());
}
