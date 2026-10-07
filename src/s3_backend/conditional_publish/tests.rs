use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    body::{to_bytes, Body},
    extract::Query,
    http::{header, HeaderMap, Method, Response, StatusCode, Uri},
    routing::any,
    Router,
};
use tokio::net::TcpListener;

use super::super::{
    authenticated_journal, protocol_tests::test_backend, S3UploadStage, S3UploadTransaction,
    UploadInput, S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE,
};
use crate::{
    error::{CleanupState, CommitState},
    storage::WriteConditions,
};

const FORMAL: &str = "/bucket/tenant/file.txt";

#[test]
fn unavailable_conditional_modes_are_rejected_before_any_transport() {
    let mut backend = test_backend("http://127.0.0.1:1");
    let mut headers = HeaderMap::new();
    headers.insert(header::IF_NONE_MATCH, "*".parse().unwrap());
    let conditions = WriteConditions::parse(&headers).unwrap();
    assert!(backend
        .validate_write_conditions(&conditions, super::super::S3_SINGLE_COPY_LIMIT + 1)
        .is_err());
    backend.provider = crate::config::S3Provider::AlibabaOss;
    assert!(backend.validate_write_conditions(&conditions, 1).is_err());
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Scenario {
    Success,
    NewFile,
    Conflict,
    DeferredRejection,
    LostResponse,
    NoProof,
    IgnoredConditions,
}

#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    etag: String,
    owner: Option<String>,
}

#[derive(Default)]
struct Store {
    objects: HashMap<String, Object>,
    sequence: usize,
    formal_copies: usize,
    stages: Vec<S3UploadStage>,
    block_cleanup: bool,
}

async fn serve(
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: Body,
    store: Arc<Mutex<Store>>,
    scenario: Scenario,
) -> Response<Body> {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    let mut store = store.lock().unwrap();
    let path = uri.path().to_owned();
    if query.contains_key("list-type") {
        let prefix = query.get("prefix").map_or("", String::as_str);
        let entries: Vec<_> = store
            .objects
            .iter()
            .filter(|(key, _)| key.trim_start_matches("/bucket/").starts_with(prefix))
            .collect();
        let contents = entries
            .iter()
            .map(|(key, object)| {
                format!(
                    "<Contents><Key>{}</Key><Size>{}</Size></Contents>",
                    key.trim_start_matches("/bucket/"),
                    object.bytes.len()
                )
            })
            .collect::<String>();
        return Response::new(Body::from(format!("<ListBucketResult>{contents}<KeyCount>{}</KeyCount><IsTruncated>false</IsTruncated></ListBucketResult>", entries.len())));
    }
    if method == Method::HEAD || method == Method::GET {
        let Some(object) = store.objects.get(&path) else {
            return Response::builder().status(404).body(Body::empty()).unwrap();
        };
        let mut response = Response::builder()
            .header(header::ETAG, &object.etag)
            .header(header::CONTENT_LENGTH, object.bytes.len());
        if let Some(owner) = &object.owner {
            response = response.header("x-amz-meta-ycloud-operation", owner);
        }
        return response
            .body(if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(object.bytes.clone())
            })
            .unwrap();
    }
    if method == Method::DELETE {
        assert_ne!(
            path, FORMAL,
            "guarded upload cleanup must never delete the formal file"
        );
        if store.block_cleanup && path.contains("/.ycloud-system/uploads/") {
            return Response::builder().status(403).body(Body::empty()).unwrap();
        }
        store.objects.remove(&path);
        return Response::builder().status(204).body(Body::empty()).unwrap();
    }
    assert_eq!(method, Method::PUT);
    let source = headers
        .get("x-amz-copy-source")
        .map(|value| format!("/{}", value.to_str().unwrap().trim_start_matches('/')));
    if source.is_some() && path == FORMAL {
        store.formal_copies += 1;
        if matches!(scenario, Scenario::Conflict | Scenario::DeferredRejection) {
            store.objects.insert(
                path.clone(),
                Object {
                    bytes: b"winner".to_vec(),
                    etag: "\"winner\"".into(),
                    owner: None,
                },
            );
        }
        if scenario == Scenario::NoProof {
            return Response::builder().status(500).body(Body::empty()).unwrap();
        }
    }
    let ignore =
        source.as_deref() == Some(path.as_str()) && scenario == Scenario::IgnoredConditions;
    let create_conflict = headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value == "*")
        && store.objects.contains_key(&path);
    let version_conflict = headers.get(header::IF_MATCH).is_some_and(|value| {
        store
            .objects
            .get(&path)
            .is_none_or(|object| value != object.etag.as_str())
    });
    if !ignore && (create_conflict || version_conflict) {
        return Response::builder().status(412).body(Body::empty()).unwrap();
    }
    store.sequence += 1;
    let etag = format!("\"object-{}\"", store.sequence);
    let object = if let Some(source) = source.as_ref() {
        let mut object = store.objects.get(source).unwrap().clone();
        object.etag = etag.clone();
        object
    } else {
        if path.contains("/.ycloud-system/transactions/") {
            let transaction: S3UploadTransaction = authenticated_journal::decode(
                &[0x31; 32],
                S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE,
                &bytes,
            )
            .unwrap();
            store.stages.push(transaction.stage);
        }
        Object {
            bytes: bytes.to_vec(),
            etag: etag.clone(),
            owner: headers
                .get("x-amz-meta-ycloud-operation")
                .map(|value| value.to_str().unwrap().to_owned()),
        }
    };
    store.objects.insert(path.clone(), object);
    if source.is_some() {
        if path == FORMAL && scenario == Scenario::LostResponse {
            return Response::builder().status(500).body(Body::empty()).unwrap();
        }
        return Response::new(Body::from(format!(
            "<CopyObjectResult><ETag>{etag}</ETag></CopyObjectResult>"
        )));
    }
    Response::builder()
        .header(header::ETAG, etag)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn guarded_publication_keeps_conditional_conflicts_and_lost_responses_distinct() {
    for scenario in [
        Scenario::Success,
        Scenario::NewFile,
        Scenario::Conflict,
        Scenario::DeferredRejection,
        Scenario::LostResponse,
        Scenario::NoProof,
        Scenario::IgnoredConditions,
    ] {
        let store = Arc::new(Mutex::new(Store::default()));
        if scenario != Scenario::NewFile {
            store.lock().unwrap().objects.insert(
                FORMAL.into(),
                Object {
                    bytes: b"old".to_vec(),
                    etag: "\"old\"".into(),
                    owner: None,
                },
            );
        }
        store.lock().unwrap().block_cleanup = scenario == Scenario::DeferredRejection;
        let observed = store.clone();
        let router = Router::new().route(
            "/{*key}",
            any(
                move |method: Method,
                      uri: Uri,
                      headers: HeaderMap,
                      Query(query): Query<HashMap<String, String>>,
                      body: Body| {
                    serve(
                        method,
                        uri,
                        headers,
                        query,
                        body,
                        observed.clone(),
                        scenario,
                    )
                },
            ),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let backend = test_backend(&endpoint);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_MATCH, "\"old\"".parse().unwrap());
        if scenario == Scenario::NewFile {
            headers.remove(header::IF_MATCH);
            headers.insert(header::IF_NONE_MATCH, "*".parse().unwrap());
        }
        let mut input = UploadInput::relay(Body::from("payload"));
        input.conditions = WriteConditions::parse(&headers).unwrap();
        let result = backend
            .upload_file_mode("file.txt", input, 7, 1024, None, false)
            .await;
        match scenario {
            Scenario::Success | Scenario::NewFile | Scenario::LostResponse => {
                assert_eq!(result.unwrap().size, 7);
            }
            Scenario::Conflict | Scenario::DeferredRejection => {
                let error = result.unwrap_err();
                assert_eq!(error.status(), StatusCode::PRECONDITION_FAILED);
                assert_eq!(error.operation().unwrap().commit, CommitState::NotCommitted);
                assert_eq!(
                    error.operation().unwrap().cleanup,
                    if scenario == Scenario::DeferredRejection {
                        CleanupState::Pending
                    } else {
                        CleanupState::Complete
                    }
                );
                if scenario == Scenario::DeferredRejection {
                    assert!(store
                        .lock()
                        .unwrap()
                        .objects
                        .keys()
                        .any(|key| key.contains("/.ycloud-system/transactions/")));
                    store.lock().unwrap().block_cleanup = false;
                    let restarted = test_backend(&endpoint);
                    restarted.recover_transactions().await.unwrap();
                }
            }
            Scenario::NoProof => {
                assert_eq!(
                    result.unwrap_err().operation().unwrap().commit,
                    CommitState::Unknown
                );
                assert!(backend
                    .recover_runtime_transactions(&crate::capacity::CapacityTracker::new(None, 0))
                    .await
                    .is_err());
                let restarted = test_backend(&endpoint);
                assert!(restarted.recover_transactions().await.is_err());
            }
            Scenario::IgnoredConditions => {
                let error = result.unwrap_err();
                assert_eq!(error.capability(), Some("conditional_file_publish"));
                assert_eq!(error.operation().unwrap().commit, CommitState::NotCommitted);
            }
        }
        let state = store.lock().unwrap();
        let expected = match scenario {
            Scenario::Success | Scenario::NewFile | Scenario::LostResponse => &b"payload"[..],
            Scenario::Conflict | Scenario::DeferredRejection => &b"winner"[..],
            _ => &b"old"[..],
        };
        assert_eq!(state.objects[FORMAL].bytes, expected);
        assert_eq!(
            state.formal_copies,
            usize::from(scenario != Scenario::IgnoredConditions)
        );
        assert!(state.stages.contains(&S3UploadStage::CheckingPublication));
        if matches!(scenario, Scenario::Conflict | Scenario::DeferredRejection) {
            assert!(state.stages.contains(&S3UploadStage::PublicationRejected));
        }
        if scenario == Scenario::NoProof {
            assert!(state
                .objects
                .keys()
                .any(|key| key.contains("/.ycloud-system/transactions/")));
            assert!(state
                .objects
                .keys()
                .any(|key| key.contains("/.ycloud-system/uploads/")));
        } else {
            assert_eq!(state.objects.len(), 1);
        }
        drop(state);
        server.abort();
    }
}
