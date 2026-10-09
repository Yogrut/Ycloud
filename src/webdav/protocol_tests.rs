use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    response::IntoResponse,
    Router,
};
use base64::Engine;
use tower::ServiceExt;

use crate::{
    config::{ConfigFile, Share},
    test_support::{app_state, TestDirectory},
};

async fn fixture() -> (TestDirectory, Router) {
    fixture_with_access(true).await
}

async fn fixture_with_access(readonly: bool) -> (TestDirectory, Router) {
    let (directory, app, _) = fixture_with_state(readonly).await;
    (directory, app)
}

async fn fixture_with_state(readonly: bool) -> (TestDirectory, Router, crate::state::AppState) {
    let directory = TestDirectory::new("dav-protocol-query");
    let state = app_state(
        &directory,
        ConfigFile {
            shares: vec![Share {
                id: "protocol-share".into(),
                storage_id: "primary".into(),
                name: "documents".into(),
                path: String::new(),
                username: Some("reader".into()),
                webdav_enabled: true,
                password_hash: Some(crate::config::hash_password("protocol-password")),
                readonly,
            }],
            ..ConfigFile::with_test_storage()
        },
    )
    .await;
    tokio::fs::write(state.config.storage_path.join("note.txt"), b"file bytes")
        .await
        .unwrap();
    tokio::fs::create_dir(state.config.storage_path.join("folder"))
        .await
        .unwrap();
    (directory, crate::app::build_router(state.clone()), state)
}

#[tokio::test]
async fn directory_result_logs_after_processing_without_counting_protocol_errors_as_bad_passwords()
{
    let (_directory, app, state) = fixture_with_state(true).await;
    let (status, _) = propfind(&app, "/dav/documents/", Some("0"), "invalid XML").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let events = state
        .login_security
        .query_events(crate::login_security::EventQuery::default())
        .await;
    assert_eq!(events.total, 1);
    assert!(!events.events[0].event.success);
    assert_eq!(events.events[0].event.failed_attempts, 0);
    assert!(events.events[0].event.result.contains("400"));
    let (status, _) = propfind(&app, "/dav/documents/", Some("1"), "").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let events = state
        .login_security
        .query_events(crate::login_security::EventQuery::default())
        .await;
    assert_eq!(events.total, 2);
    assert!(events.events[0].event.success);
    assert_eq!(events.events[0].event.failed_attempts, 0);
}

#[tokio::test]
async fn mkcol_does_not_silently_discard_an_unsupported_body() {
    let (_directory, app, state) = fixture_with_state(false).await;
    let response = app
        .clone()
        .oneshot(
            authorized_request("MKCOL", "/dav/documents/new-folder")
                .body(Body::from("unsupported collection payload"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(!state.config.storage_path.join("new-folder").exists());
}

#[tokio::test]
async fn copy_and_move_honor_overwrite_for_files_directories_and_cross_type_replacement() {
    for method in ["COPY", "MOVE"] {
        for (source_directory, target_directory) in
            [(false, false), (true, true), (false, true), (true, false)]
        {
            for overwrite in [None, Some("T"), Some("F"), Some("t"), Some("f")] {
                let (_directory, app, state) = fixture_with_state(false).await;
                let root = &state.config.storage_path;
                if source_directory {
                    tokio::fs::create_dir_all(root.join("source/nested"))
                        .await
                        .unwrap();
                    tokio::fs::write(root.join("source/nested/新文件.txt"), b"new data")
                        .await
                        .unwrap();
                } else {
                    tokio::fs::write(root.join("source"), b"new data")
                        .await
                        .unwrap();
                }
                if target_directory {
                    tokio::fs::create_dir(root.join("target")).await.unwrap();
                    tokio::fs::write(root.join("target/old-only.txt"), b"old data")
                        .await
                        .unwrap();
                } else {
                    tokio::fs::write(root.join("target"), b"old data")
                        .await
                        .unwrap();
                }
                let mut request = authorized_request(method, "/dav/documents/source").header(
                    "Destination",
                    "https://127.0.0.1:18473/dav/documents/target",
                );
                if let Some(value) = overwrite {
                    request = request.header("Overwrite", value);
                }
                let response = app
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                if overwrite.is_some_and(|value| value.eq_ignore_ascii_case("F")) {
                    assert_eq!(
                        response.status(),
                        StatusCode::PRECONDITION_FAILED,
                        "{method}, source dir={source_directory}, target dir={target_directory}"
                    );
                    let old = if target_directory {
                        root.join("target/old-only.txt")
                    } else {
                        root.join("target")
                    };
                    assert_eq!(tokio::fs::read(old).await.unwrap(), b"old data");
                    assert!(root.join("source").exists());
                } else {
                    assert_eq!(
                        response.status(),
                        StatusCode::NO_CONTENT,
                        "{method}, source dir={source_directory}, target dir={target_directory}"
                    );
                    let new = if source_directory {
                        root.join("target/nested/新文件.txt")
                    } else {
                        root.join("target")
                    };
                    assert_eq!(tokio::fs::read(new).await.unwrap(), b"new data");
                    assert!(!root.join("target/old-only.txt").exists());
                    assert_eq!(root.join("source").exists(), method == "COPY");
                }
            }
        }
    }
}

#[tokio::test]
async fn copy_move_creation_and_invalid_headers_preserve_source_and_target_contracts() {
    for method in ["COPY", "MOVE"] {
        let (_directory, app, state) = fixture_with_state(false).await;
        let response = app
            .clone()
            .oneshot(
                authorized_request(method, "/dav/documents/note.txt")
                    .header("Destination", "/dav/documents/new.txt")
                    .header("Overwrite", "F")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("new.txt"))
                .await
                .unwrap(),
            b"file bytes"
        );
        for invalid in ["true", "false", "TF", ""] {
            let response = app
                .clone()
                .oneshot(
                    authorized_request(method, "/dav/documents/new.txt")
                        .header("Destination", "/dav/documents/rejected.txt")
                        .header("Overwrite", invalid)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(!state.config.storage_path.join("rejected.txt").exists());
        }
        let response = app
            .oneshot(
                authorized_request(method, "/dav/documents/new.txt")
                    .header("Destination", "/dav/documents/rejected.txt")
                    .header("Overwrite", "T")
                    .header("Overwrite", "F")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn failed_or_readonly_transfers_do_not_remove_the_destination() {
    for readonly in [false, true] {
        for method in ["COPY", "MOVE"] {
            let (_directory, app, state) = fixture_with_state(readonly).await;
            let response = app
                .oneshot(
                    authorized_request(method, "/dav/documents/missing")
                        .header("Destination", "/dav/documents/note.txt")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if readonly {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::NOT_FOUND
                }
            );
            assert_eq!(
                tokio::fs::read(state.config.storage_path.join("note.txt"))
                    .await
                    .unwrap(),
                b"file bytes"
            );
        }
    }
}

#[tokio::test]
async fn overwriting_transfers_settle_capacity_once_and_do_not_change_browser_copy_rules() {
    for copy in [false, true] {
        let (_directory, _app, state) = fixture_with_state(false).await;
        let backend = state.storage_backend("primary").await.unwrap();
        backend
            .upload_file("source.bin", Body::from("new"), Some(3), 1024, None)
            .await
            .unwrap();
        backend
            .upload_file("target.bin", Body::from("previous"), Some(8), 1024, None)
            .await
            .unwrap();
        let before = backend.capacity_status().used;
        assert!(backend.copy_path("source.bin", "target.bin").await.is_err());
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("target.bin"))
                .await
                .unwrap(),
            b"previous"
        );
        assert!(!backend
            .dav_transfer(
                "source.bin",
                "target.bin",
                if copy {
                    crate::storage_backend::TransferKind::Copy
                } else {
                    crate::storage_backend::TransferKind::Move
                },
                true
            )
            .await
            .unwrap());
        assert_eq!(
            backend.capacity_status().used,
            before - 8 + if copy { 3 } else { 0 }
        );
        assert_eq!(backend.capacity_status().reserved, 0);
    }
}

#[tokio::test]
async fn copy_depth_zero_replaces_with_an_empty_collection_and_preserves_source() {
    for target_kind in ["missing", "file", "directory"] {
        let (_directory, app, state) = fixture_with_state(false).await;
        let backend = state.storage_backend("primary").await.unwrap();
        backend.create_directory("source").await.unwrap();
        backend
            .upload_file("source/child.txt", Body::from("new"), Some(3), 1024, None)
            .await
            .unwrap();
        if target_kind == "directory" {
            backend.create_directory("target").await.unwrap();
            backend
                .upload_file(
                    "target/old.txt",
                    Body::from("previous"),
                    Some(8),
                    1024,
                    None,
                )
                .await
                .unwrap();
        } else if target_kind == "file" {
            backend
                .upload_file("target", Body::from("previous"), Some(8), 1024, None)
                .await
                .unwrap();
        }
        let before = backend.capacity_status().used;
        let response = app
            .oneshot(
                authorized_request("COPY", "/dav/documents/source")
                    .header("Destination", "/dav/documents/target")
                    .header("Depth", "0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if target_kind == "missing" {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            }
        );
        let root = &state.config.storage_path;
        assert!(root.join("target").is_dir());
        assert!(tokio::fs::read_dir(root.join("target"))
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            tokio::fs::read(root.join("source/child.txt"))
                .await
                .unwrap(),
            b"new"
        );
        assert_eq!(
            backend.capacity_status().used,
            before - if target_kind == "missing" { 0 } else { 8 }
        );
        assert_eq!(backend.capacity_status().reserved, 0);
    }
}

#[tokio::test]
async fn depth_validation_is_applied_before_mutation_and_file_copy_zero_keeps_bytes() {
    let (_directory, app, state) = fixture_with_state(false).await;
    for (method, depth) in [
        ("COPY", "1"),
        ("COPY", "invalid"),
        ("MOVE", "0"),
        ("MOVE", "1"),
    ] {
        let response = app
            .clone()
            .oneshot(
                authorized_request(method, "/dav/documents/note.txt")
                    .header("Destination", "/dav/documents/rejected.txt")
                    .header("Depth", depth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!state.config.storage_path.join("rejected.txt").exists());
        assert!(state.config.storage_path.join("note.txt").exists());
    }
    let response = app
        .clone()
        .oneshot(
            authorized_request("COPY", "/dav/documents/note.txt")
                .header("Destination", "/dav/documents/copied.txt")
                .header("Depth", "0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        tokio::fs::read(state.config.storage_path.join("copied.txt"))
            .await
            .unwrap(),
        b"file bytes"
    );
    let response = app
        .oneshot(
            authorized_request("COPY", "/dav/documents/note.txt")
                .header("Destination", "/dav/documents/rejected.txt")
                .header("Depth", "0")
                .header("Depth", "infinity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mkcol_reports_existing_target_and_missing_parent_by_protocol_reason() {
    let (_directory, app, state) = fixture_with_state(false).await;
    for (path, expected) in [
        ("folder", StatusCode::METHOD_NOT_ALLOWED),
        ("note.txt", StatusCode::METHOD_NOT_ALLOWED),
        ("missing/child", StatusCode::CONFLICT),
        ("created", StatusCode::CREATED),
    ] {
        let response = app
            .clone()
            .oneshot(
                authorized_request("MKCOL", &format!("/dav/documents/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
    }
    assert!(state.config.storage_path.join("created").is_dir());
    assert!(!state.config.storage_path.join("missing").exists());
}

#[tokio::test]
async fn dav_get_and_head_evaluate_preconditions_before_ranges() {
    let (_directory, app) = fixture().await;
    let initial = app
        .clone()
        .oneshot(
            authorized_request("HEAD", "/dav/documents/note.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let modified = initial
        .headers()
        .get(header::LAST_MODIFIED)
        .unwrap()
        .clone();
    assert!(initial.headers().get(header::ETAG).is_none());
    for method in ["GET", "HEAD"] {
        for (name, value, expected) in [
            (
                header::IF_MODIFIED_SINCE,
                modified.clone(),
                StatusCode::NOT_MODIFIED,
            ),
            (
                header::IF_NONE_MATCH,
                axum::http::HeaderValue::from_static("*"),
                StatusCode::NOT_MODIFIED,
            ),
            (
                header::IF_MATCH,
                axum::http::HeaderValue::from_static("\"unavailable\""),
                StatusCode::PRECONDITION_FAILED,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    authorized_request(method, "/dav/documents/note.txt")
                        .header(name, value)
                        .header(header::RANGE, "bytes=999-1000")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(
                response.headers().get(header::LAST_MODIFIED).unwrap(),
                &modified
            );
            assert!(to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty());
        }
    }
}

async fn propfind(
    app: &Router,
    path: &str,
    depth: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut request = authorized_request("PROPFIND", path);
    if let Some(depth) = depth {
        request = request.header("Depth", depth);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn finite_depth_lists_exactly_the_requested_level_and_reports_infinity() {
    let (_directory, app) = fixture().await;
    for (depth, count) in [("0", 1), ("1", 3)] {
        let (status, xml) = propfind(&app, "/dav/documents/", Some(depth), "").await;
        assert_eq!(status, StatusCode::MULTI_STATUS);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(
            doc.descendants()
                .filter(|node| node.has_tag_name(("DAV:", "response")))
                .count(),
            count
        );
        if depth == "1" {
            assert!(xml.contains("/dav/documents/folder/</D:href>"));
        }
    }
    for depth in [None, Some("infinity")] {
        let (status, xml) = propfind(&app, "/dav/documents/", depth, "").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert!(doc
            .descendants()
            .any(|node| node.has_tag_name(("DAV:", "propfind-finite-depth"))));
    }
    let (status, body) = propfind(&app, "/dav/documents/", Some("2"), "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Depth"));
    assert_eq!(
        propfind(&app, "/dav/documents/note.txt", Some("infinity"), "")
            .await
            .0,
        StatusCode::MULTI_STATUS
    );
}

fn authorized_request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:18473")
        .header(header::CONTENT_TYPE, "application/xml")
        .header(
            header::AUTHORIZATION,
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("reader:protocol-password")
            ),
        )
        .extension(axum::extract::ConnectInfo(
            "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
        ))
}

#[tokio::test]
async fn utf16_propfinds_work_through_authenticated_http_and_keep_utf8_responses() {
    let (_directory, app) = fixture().await;
    for little_endian in [true, false] {
        for bom in [true, false] {
            let charset = if bom {
                "utf-16"
            } else if little_endian {
                "utf-16le"
            } else {
                "utf-16be"
            };
            let text = format!(
                r#"<?xml version="1.0" encoding="{charset}"?><propfind xmlns="DAV:" xmlns:x="urn:照片📷"><prop><displayname/><x:标题/></prop></propfind>"#
            );
            let mut bytes = Vec::new();
            if bom {
                bytes.extend_from_slice(if little_endian {
                    &[0xff, 0xfe]
                } else {
                    &[0xfe, 0xff]
                });
            }
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            let mut request = authorized_request("PROPFIND", "/dav/documents/")
                .header("Depth", "1")
                .body(Body::from(bytes))
                .unwrap();
            request.headers_mut().insert(
                header::CONTENT_TYPE,
                format!("application/xml; charset={charset}")
                    .parse()
                    .unwrap(),
            );
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::MULTI_STATUS);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/xml; charset=utf-8"
            );
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let xml = std::str::from_utf8(&body).unwrap();
            let document = roxmltree::Document::parse(xml).unwrap();
            assert_eq!(
                document
                    .descendants()
                    .filter(|node| node.has_tag_name(("DAV:", "response")))
                    .count(),
                3
            );
            assert!(xml.contains("urn:照片📷"));
            assert!(xml.contains("HTTP/1.1 404 Not Found"));
        }
    }
}

#[tokio::test]
async fn propfind_encoding_errors_keep_the_shared_safe_error_contract() {
    let (_directory, app) = fixture().await;
    let response = app
        .oneshot(
            authorized_request("PROPFIND", "/dav/documents/")
                .header("Depth", "0")
                .body(Body::from(vec![0xff, 0xfe, 0x3c]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "bad_request");
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("utf16_invalid"));
    assert!(!String::from_utf8_lossy(&body).contains("protocol-password"));
}

#[tokio::test]
async fn conditional_puts_keep_existing_bytes_and_other_conditional_methods_do_not_execute() {
    let (_directory, app) = fixture_with_access(false).await;
    for (path, name, value, expected) in [
        (
            "/dav/documents/note.txt",
            "If-None-Match",
            "*",
            StatusCode::PRECONDITION_FAILED,
        ),
        (
            "/dav/documents/missing.txt",
            "If-Match",
            "*",
            StatusCode::PRECONDITION_FAILED,
        ),
        (
            "/dav/documents/note.txt",
            "If-Unmodified-Since",
            "Tue, 14 Nov 2023 22:13:20 GMT",
            StatusCode::PRECONDITION_FAILED,
        ),
        (
            "/dav/documents/new.txt",
            "If-None-Match",
            "*",
            StatusCode::CREATED,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                authorized_request("PUT", path)
                    .header(name, value)
                    .header(header::CONTENT_LENGTH, 7)
                    .body(Body::from("payload"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let response = app
        .clone()
        .oneshot(
            authorized_request("DELETE", "/dav/documents/note.txt")
                .header("If-Match", "*")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let response = app
        .clone()
        .oneshot(
            authorized_request("GET", "/dav/documents/note.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        b"file bytes"
    );
}

#[tokio::test]
async fn selected_properties_preserve_namespaces_and_separate_missing_values() {
    let (_directory, app) = fixture().await;
    let request = r#"<q:propfind xmlns:q="DAV:" xmlns:x="urn:client"><q:prop><q:displayname/><q:displayname/><x:label/></q:prop></q:propfind>"#;
    let (status, xml) = propfind(&app, "/dav/documents/note.txt", Some("0"), request).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let doc = roxmltree::Document::parse(&xml).unwrap();
    assert_eq!(
        doc.descendants()
            .filter(|node| node.has_tag_name(("DAV:", "displayname")))
            .count(),
        1
    );
    assert!(!doc
        .descendants()
        .any(|node| node.has_tag_name(("DAV:", "getcontentlength"))));
    let groups: Vec<_> = doc
        .descendants()
        .filter(|node| node.has_tag_name(("DAV:", "propstat")))
        .collect();
    assert_eq!(groups.len(), 2);
    assert!(groups[0]
        .descendants()
        .any(|node| node.text() == Some("HTTP/1.1 200 OK")));
    assert!(groups[1]
        .descendants()
        .any(|node| node.has_tag_name(("urn:client", "label"))));
    assert!(groups[1]
        .descendants()
        .any(|node| node.text() == Some("HTTP/1.1 404 Not Found")));
}

#[tokio::test]
async fn property_names_have_no_values_and_allprop_honors_include() {
    let (_directory, app) = fixture().await;
    let request =
        r#"<propfind xmlns="DAV:"><propname><!-- client comment --></propname></propfind>"#;
    let (status, xml) = propfind(&app, "/dav/documents/folder/", Some("0"), request).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let properties = doc
        .descendants()
        .find(|node| node.has_tag_name(("DAV:", "prop")))
        .unwrap();
    assert_eq!(
        properties
            .children()
            .filter(|node| node.is_element())
            .count(),
        5
    );
    assert!(properties
        .children()
        .all(|node| node.children().next().is_none()));
    let request =
        r#"<propfind xmlns="DAV:"><allprop/><include><creationdate/></include></propfind>"#;
    let (status, xml) = propfind(&app, "/dav/documents/note.txt", Some("0"), request).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(xml.contains("<D:displayname>note.txt</D:displayname>"));
    assert!(xml.contains("<D:creationdate/>"));
    assert!(xml.contains("HTTP/1.1 404 Not Found"));
}

#[tokio::test]
async fn ordinary_dav_errors_keep_safe_details_without_private_sources() {
    for error in [
        crate::error::AppError::BadRequest("public validation reason".into()),
        crate::error::AppError::internal("private backend context"),
    ] {
        let status = error.status();
        let response = super::DavError::from(error).into_response();
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"]["code"].is_string());
        assert!(!String::from_utf8_lossy(&body).contains("private backend context"));
        if status == StatusCode::BAD_REQUEST {
            assert_eq!(json["error"]["message"], "public validation reason");
        }
    }
}
