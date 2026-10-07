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
    (directory, crate::app::build_router(state))
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
