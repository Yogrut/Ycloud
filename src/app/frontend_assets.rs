//! Only the Vite-generated, compiled resource table is served. A request never
//! opens a filesystem path or falls back to the page HTML for a missing chunk.

use axum::{
    extract::Path,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::OnceLock;

use super::{content_etag, embedded_response};

pub(super) struct EmbeddedAsset {
    pub(super) name: &'static str,
    pub(super) content_type: &'static str,
    pub(super) bytes: &'static [u8],
    etag: OnceLock<HeaderValue>,
}

impl EmbeddedAsset {
    const fn new(name: &'static str, content_type: &'static str, bytes: &'static [u8]) -> Self {
        Self {
            name,
            content_type,
            bytes,
            etag: OnceLock::new(),
        }
    }
}

// The frontend build owns names, MIME types and include_bytes references.
// Missing listed files therefore fail compilation instead of failing at runtime.
include!("../../static/app/embedded-assets.rs");

pub(super) async fn serve_asset(Path(name): Path<String>, headers: HeaderMap) -> Response {
    let Ok(index) = ASSETS.binary_search_by(|asset| asset.name.cmp(name.as_str())) else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let asset = &ASSETS[index];
    embedded_response(
        asset.bytes,
        asset.content_type,
        &headers,
        asset.etag.get_or_init(|| content_etag(asset.bytes)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Method, Request},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn every_compiled_chunk_is_served_with_its_bytes_mime_and_validator() {
        let directory = crate::test_support::TestDirectory::new("frontend-chunk-delivery");
        let state =
            crate::test_support::app_state(&directory, crate::config::ConfigFile::default()).await;
        let router = crate::app::build_router(state);
        assert!(ASSETS.windows(2).all(|pair| pair[0].name < pair[1].name));
        assert!(ASSETS
            .iter()
            .any(|asset| asset.name.starts_with("AdminView-")));
        assert!(ASSETS
            .iter()
            .any(|asset| asset.name.starts_with("BrowserView-")));
        for asset in &ASSETS {
            let request = |method, etag: Option<&HeaderValue>| {
                let mut request = Request::builder()
                    .method(method)
                    .uri(format!("/assets/{}", asset.name))
                    .header(header::HOST, "localhost:18473");
                if let Some(etag) = etag {
                    request = request.header(header::IF_NONE_MATCH, etag);
                }
                request.body(Body::empty()).unwrap()
            };
            let response = router
                .clone()
                .oneshot(request(Method::GET, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{}", asset.name);
            assert_eq!(response.headers()[header::CONTENT_TYPE], asset.content_type);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            let etag = response.headers()[header::ETAG].clone();
            assert_eq!(
                to_bytes(response.into_body(), asset.bytes.len())
                    .await
                    .unwrap()
                    .as_ref(),
                asset.bytes
            );
            let head = router
                .clone()
                .oneshot(request(Method::HEAD, None))
                .await
                .unwrap();
            assert_eq!(head.status(), StatusCode::OK);
            assert_eq!(head.headers()[header::ETAG], etag);
            assert!(to_bytes(head.into_body(), 0).await.unwrap().is_empty());
            let unchanged = router
                .clone()
                .oneshot(request(Method::GET, Some(&etag)))
                .await
                .unwrap();
            assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
            assert!(to_bytes(unchanged.into_body(), 0).await.unwrap().is_empty());
        }
        let unknown = router
            .oneshot(
                Request::builder()
                    .uri("/assets/missing-page.js")
                    .header(header::HOST, "localhost:18473")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            to_bytes(unknown.into_body(), 1024).await.unwrap().as_ref(),
            b"Not Found"
        );
    }
}
