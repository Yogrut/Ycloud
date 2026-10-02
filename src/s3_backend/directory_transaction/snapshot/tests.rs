use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use axum::{
    body::Body,
    extract::Query,
    http::{Method, Response},
    routing::any,
    Router,
};

use super::*;

struct SnapshotFixture {
    backend: S3Backend,
    requests: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for SnapshotFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl SnapshotFixture {
    async fn new(scenario: &'static str) -> Self {
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let router = Router::new().route("/{*key}", any(
            move |method: Method, Query(query): Query<HashMap<String, String>>| {
                let observed = observed.clone();
                async move {
                    assert_eq!(method, Method::GET, "snapshot must be read-only");
                    assert_eq!(query.get("list-type").map(String::as_str), Some("2"));
                    assert_eq!(query.get("prefix").map(String::as_str), Some("tenant/source/"));
                        assert_eq!(query.get("max-keys").unwrap(), &S3_PAGE_SIZE.to_string());
                    let page = observed.fetch_add(1, Ordering::SeqCst);
                    let content = match scenario {
                        "empty_marker" => "<Contents><Key>tenant/source/</Key><Size>0</Size><ETag>marker-etag</ETag></Contents>".into(),
                        "valid" if page == 0 => "<Contents><Key>tenant/source/</Key><Size>0</Size><ETag>marker-etag</ETag></Contents>".into(),
                        "valid" => "<Contents><Key>tenant/source/nested/file.bin</Key><Size>42</Size><ETag>file-etag</ETag></Contents>".into(),
                        "escaped" => "<Contents><Key>tenant/other/file.bin</Key><Size>42</Size><ETag>file-etag</ETag></Contents>".into(),
                        "missing_key" => "<Contents><Size>42</Size><ETag>file-etag</ETag></Contents>".into(),
                        "missing_etag" => "<Contents><Key>tenant/source/file.bin</Key><Size>42</Size></Contents>".into(),
                        "negative_size" => "<Contents><Key>tenant/source/file.bin</Key><Size>-1</Size><ETag>file-etag</ETag></Contents>".into(),
                        "large_object" => format!("<Contents><Key>tenant/source/file.bin</Key><Size>{}</Size><ETag>file-etag</ETag></Contents>", MAX_SINGLE_COPY_BYTES + 1),
                        "too_many_objects" => (0..=MAX_OBJECTS).map(|index| format!("<Contents><Key>tenant/source/{index}</Key><Size>0</Size><ETag>file-etag</ETag></Contents>")).collect::<String>(),
                        _ => String::new(),
                    };
                    let pagination = match scenario {
                        "valid" if page == 0 => "<IsTruncated>true</IsTruncated><NextContinuationToken>page-1</NextContinuationToken>".into(),
                        "stalled" => "<IsTruncated>true</IsTruncated><NextContinuationToken>same-token</NextContinuationToken>".into(),
                        "missing_token" => "<IsTruncated>true</IsTruncated>".into(),
                        "page_limit" => format!("<IsTruncated>true</IsTruncated><NextContinuationToken>page-{}</NextContinuationToken>", page + 1),
                        _ => "<IsTruncated>false</IsTruncated>".into(),
                    };
                    if page > 0 {
                        let expected = if scenario == "stalled" { "same-token".into() } else { format!("page-{page}") };
                        assert_eq!(query.get("continuation-token"), Some(&expected));
                    }
                    Response::builder().header("content-type", "application/xml")
                        .body(Body::from(format!("<ListBucketResult>{content}{pagination}</ListBucketResult>"))).unwrap()
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
async fn directory_snapshot_handles_empty_markers_through_the_shared_transaction_path() {
    let id = "fedcba9876543210fedcba9876543210";
    for operation in [Operation::Copy, Operation::Move, Operation::Delete] {
        let fixture = SnapshotFixture::new("empty_marker").await;
        let destination = (operation != Operation::Delete).then_some("target");
        let objects = fixture
            .backend
            .snapshot_objects(id, operation, "source", destination)
            .await
            .unwrap();
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        let target_key = if operation == Operation::Delete {
            internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/"))
        } else {
            "tenant/target/".into()
        };
        assert_eq!(
            objects,
            vec![ObjectRecord {
                source_key: "tenant/source/".into(),
                target_key,
                size: 0,
                source_etag: "marker-etag".into(),
                target_etag: None,
                source_deleted: false,
            }]
        );
    }
}

#[tokio::test]
async fn directory_snapshot_preserves_paginated_mapping_and_can_be_paused() {
    let id = "fedcba9876543210fedcba9876543210";
    for operation in [Operation::Copy, Operation::Move, Operation::Delete] {
        let fixture = SnapshotFixture::new("valid").await;
        fixture.backend.pause_maintenance();
        assert!(fixture
            .backend
            .snapshot_objects(id, operation, "source", Some("target"))
            .await
            .is_err());
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
        fixture.backend.resume_maintenance();
        let objects = fixture
            .backend
            .snapshot_objects(id, operation, "source", Some("target"))
            .await
            .unwrap();
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
        assert_eq!(objects.len(), 2);
        let target_prefix = if operation == Operation::Delete {
            internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/"))
        } else {
            "tenant/target/".into()
        };
        assert_eq!(objects[0].source_key, "tenant/source/");
        assert_eq!(objects[0].target_key, target_prefix);
        assert_eq!(objects[0].size, 0);
        assert_eq!(objects[0].source_etag, "marker-etag");
        assert_eq!(objects[1].source_key, "tenant/source/nested/file.bin");
        assert_eq!(
            objects[1].target_key,
            format!("{target_prefix}nested/file.bin")
        );
        assert_eq!(objects[1].size, 42);
        assert_eq!(objects[1].source_etag, "file-etag");
        assert!(objects
            .iter()
            .all(|object| object.target_etag.is_none() && !object.source_deleted));
    }
}

#[tokio::test]
async fn directory_snapshot_rejects_invalid_or_unbounded_lists_without_writes() {
    for scenario in [
        "empty",
        "escaped",
        "missing_key",
        "missing_etag",
        "negative_size",
        "large_object",
        "too_many_objects",
        "stalled",
        "missing_token",
        "page_limit",
    ] {
        let fixture = SnapshotFixture::new(scenario).await;
        assert!(
            fixture
                .backend
                .snapshot_objects(
                    "fedcba9876543210fedcba9876543210",
                    Operation::Copy,
                    "source",
                    Some("target")
                )
                .await
                .is_err(),
            "{scenario}"
        );
        let expected = match scenario {
            "stalled" => 2,
            "page_limit" => S3_MAX_LIST_PAGES,
            _ => 1,
        };
        assert_eq!(
            fixture.requests.load(Ordering::SeqCst),
            expected,
            "{scenario}"
        );
        assert!(!fixture.backend.recovery_has_pending());
    }
}
