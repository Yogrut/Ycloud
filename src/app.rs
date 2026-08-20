use std::{sync::Arc, time::Duration};

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderName, Method, StatusCode},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post, put},
    Router,
};
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::{SetSensitiveRequestHeadersLayer, SetSensitiveResponseHeadersLayer},
    trace::TraceLayer,
};

use crate::{
    admin_api, api, archive,
    auth::{
        self, admin_auth_middleware, auth_middleware, me_handler, write_auth_middleware,
        RateLimiter,
    },
    batch_operations,
    security::{csrf_middleware, proxy_boundary_middleware, security_headers_middleware},
    state::AppState,
    webdav,
};

pub fn build_router(state: AppState) -> Router {
    let max_body_bytes = usize::try_from(state.config.max_upload_bytes)
        .unwrap_or(usize::MAX)
        .max(1);
    let unlock_limiter = Arc::new(RateLimiter::new(10, 60));

    let auth_routes = Router::new()
        .route("/login", post(auth::login_handler))
        .route("/gate", post(auth::gate_handler))
        .layer(DefaultBodyLimit::max(32 * 1024));

    let public_api = Router::new()
        .route("/health", get(health_handler))
        .route("/logout", post(auth::logout_handler))
        .route("/me", get(me_handler))
        .merge(auth_routes);

    let unlock_route = Router::new()
        .route("/folder/unlock", post(api::unlock_folder))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn_with_state(
            unlock_limiter,
            auth::rate_limit_middleware,
        ));

    let read_api = Router::new()
        .route("/files", get(api::list_files))
        .route("/download", get(api::download_file))
        .route(
            "/archive/prepare",
            post(archive::prepare_archive).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/archive", get(archive::download_archive))
        .route("/preview", get(api::preview_file))
        .merge(unlock_route);

    let write_api = Router::new()
        .route("/files", axum::routing::delete(api::delete_file))
        .route("/mkdir", post(api::create_directory))
        .route("/upload", put(api::upload_file))
        .route("/rename", put(api::rename_file))
        .route("/batch/delete", post(batch_operations::delete))
        .route("/batch/move", put(batch_operations::move_items))
        .route("/batch/copy", post(batch_operations::copy))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            write_auth_middleware,
        ));

    let protected_api = Router::new()
        .merge(read_api)
        .merge(write_api)
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let api_routes =
        Router::new()
            .merge(public_api)
            .merge(protected_api)
            .layer(middleware::from_fn_with_state(
                state.config.clone(),
                csrf_middleware,
            ));

    let admin_routes = Router::new()
        .route("/info", get(admin_api::admin_info))
        .route("/shares", post(admin_api::create_share))
        .route(
            "/shares/{id}",
            put(admin_api::update_share).delete(admin_api::delete_share),
        )
        .route("/account", put(admin_api::update_admin_account))
        .route("/limits", put(admin_api::update_transfer_limits))
        .route("/security/block", post(admin_api::block_login))
        .route("/security/unblock", post(admin_api::unblock_login))
        .route("/locks", post(admin_api::create_lock))
        .route(
            "/locks/{id}",
            put(admin_api::update_lock).delete(admin_api::delete_lock),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.config.clone(),
            csrf_middleware,
        ));

    let dav_router = Router::new()
        .route("/dav/{*rest}", axum::routing::any(webdav::webdav_handler))
        .layer(DefaultBodyLimit::max(max_body_bytes));

    let request_id_header = HeaderName::from_static("x-request-id");
    let middleware_stack = ServiceBuilder::new()
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::COOKIE,
        ]))
        .layer(SetSensitiveResponseHeadersLayer::new([header::SET_COOKIE]))
        .layer(SetRequestIdLayer::new(
            request_id_header.clone(),
            MakeRequestUuid,
        ))
        .layer(PropagateRequestIdLayer::new(request_id_header))
        .layer(TraceLayer::new_for_http());
    let timeouts = RequestTimeouts {
        regular: Duration::from_secs(state.config.request_timeout_secs),
        upload: Duration::from_secs(state.config.upload_timeout_secs),
    };

    Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
        .route("/login", get(|| async { Redirect::to("/") }))
        .route("/login.html", get(|| async { Redirect::to("/") }))
        .route("/admin", get(serve_admin))
        .route("/admin.html", get(serve_admin))
        .route("/browse", get(serve_browser))
        .route("/browser.html", get(serve_browser))
        .route("/preview.html", get(serve_preview))
        .route("/theme.css", get(serve_theme_css))
        .route("/theme.js", get(serve_theme_js))
        .route("/index.js", get(serve_index_js))
        .route("/admin.js", get(serve_admin_js))
        .route("/browser.js", get(serve_browser_js))
        .route("/browser-api.js", get(serve_browser_api_js))
        .route("/browser-state.js", get(serve_browser_state_js))
        .route("/browser-dialog.js", get(serve_browser_dialog_js))
        .route("/preview.js", get(serve_preview_js))
        .route("/favicon.svg", get(serve_favicon))
        .nest("/api/admin", admin_routes)
        .nest("/api", api_routes)
        .merge(dav_router)
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.config.clone(),
            security_headers_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            timeouts,
            request_timeout_middleware,
        ))
        .layer(middleware_stack)
        .layer(middleware::from_fn_with_state(
            state.config.clone(),
            proxy_boundary_middleware,
        ))
        .with_state(state)
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    if state.storage.ready().await {
        (
            StatusCode::OK,
            axum::Json(JsonStatus {
                status: "ok",
                storage: "ready",
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(JsonStatus {
                status: "degraded",
                storage: "unavailable",
            }),
        )
    }
}

#[derive(serde::Serialize)]
struct JsonStatus {
    status: &'static str,
    storage: &'static str,
}

#[derive(Clone, Copy)]
struct RequestTimeouts {
    regular: Duration,
    upload: Duration,
}

async fn request_timeout_middleware(
    State(timeouts): State<RequestTimeouts>,
    request: Request,
    next: middleware::Next,
) -> Response {
    let is_upload = request.uri().path() == "/api/upload"
        || (request.method() == Method::PUT && request.uri().path().starts_with("/dav/"));
    let duration = if is_upload {
        timeouts.upload
    } else {
        timeouts.regular
    };
    match tokio::time::timeout(duration, next.run(request)).await {
        Ok(response) => response,
        Err(_) => crate::error::AppError::RequestTimeout.into_response(),
    }
}

macro_rules! embedded_handler {
    ($name:ident, $content_type:literal, $path:literal) => {
        async fn $name() -> Response {
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, $content_type),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                &include_bytes!($path)[..],
            )
                .into_response()
        }
    };
}

embedded_handler!(
    serve_index,
    "text/html; charset=utf-8",
    "../static/index.html"
);
async fn serve_admin(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    let is_admin = match auth::extract_session_token(&headers) {
        Some(token) => state.sessions.validate(&token).await,
        None => false,
    };
    if !is_admin {
        return Redirect::to("/browse").into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        &include_bytes!("../static/admin.html")[..],
    )
        .into_response()
}
embedded_handler!(
    serve_browser,
    "text/html; charset=utf-8",
    "../static/browser.html"
);
embedded_handler!(
    serve_preview,
    "text/html; charset=utf-8",
    "../static/preview.html"
);
embedded_handler!(
    serve_theme_css,
    "text/css; charset=utf-8",
    "../static/theme.css"
);
embedded_handler!(
    serve_theme_js,
    "application/javascript; charset=utf-8",
    "../static/theme.js"
);
embedded_handler!(
    serve_index_js,
    "application/javascript; charset=utf-8",
    "../static/index.js"
);
embedded_handler!(
    serve_admin_js,
    "application/javascript; charset=utf-8",
    "../static/admin.js"
);
embedded_handler!(
    serve_browser_js,
    "application/javascript; charset=utf-8",
    "../static/browser.js"
);
embedded_handler!(
    serve_browser_api_js,
    "application/javascript; charset=utf-8",
    "../static/browser-api.js"
);
embedded_handler!(
    serve_browser_state_js,
    "application/javascript; charset=utf-8",
    "../static/browser-state.js"
);
embedded_handler!(
    serve_browser_dialog_js,
    "application/javascript; charset=utf-8",
    "../static/browser-dialog.js"
);
embedded_handler!(
    serve_preview_js,
    "application/javascript; charset=utf-8",
    "../static/preview.js"
);
embedded_handler!(serve_favicon, "image/svg+xml", "../static/favicon.svg");

async fn not_found(_request: Request) -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "Not Found")
}

#[cfg(test)]
mod tests {
    use super::build_router;
    use crate::{
        config::{hash_password, Config, ConfigFile, Share},
        state::AppState,
    };
    use axum::{
        body::{to_bytes, Body},
        extract::ConnectInfo,
        http::{header, Request, StatusCode},
    };
    use std::{net::SocketAddr, sync::Arc};
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    #[tokio::test]
    async fn router_exposes_health_and_protects_storage_api() {
        let root = std::env::temp_dir().join(format!("ycloud-app-{}", uuid::Uuid::new_v4()));
        let state = AppState::new(
            Config {
                bind_address: std::net::IpAddr::from([127, 0, 0, 1]),
                port: 18_473,
                storage_path: root.clone(),
                config_path: root.join("config.json"),
                max_upload_bytes: 2 * 1024 * 1024,
                io_concurrency: 2,
                max_list_entries: 100,
                request_timeout_secs: 30,
                upload_timeout_secs: 300,
                disk_reserve_bytes: 0,
                secure_cookies: false,
                allow_lan_http: false,
                public_base_url: None,
                public_host: None,
                trusted_proxy_ips: Default::default(),
            },
            Arc::new(RwLock::new(ConfigFile {
                schema_version: 2,
                admin_username: "admin".into(),
                admin_password_hash: hash_password("test-password"),
                global_web_password_hash: None,
                folder_locks: Vec::new(),
                shares: vec![Share {
                    id: "test-share".into(),
                    name: "open".into(),
                    path: String::new(),
                    username: Some("yogrut".into()),
                    webdav_enabled: true,
                    password_hash: Some(hash_password("webdav-password")),
                    readonly: false,
                }],
                max_upload_bytes: 1024 * 1024,
                max_archive_bytes: 2 * 1024 * 1024,
                max_archive_entries: 100,
            })),
        )
        .await
        .unwrap();
        let admin_token = state.sessions.create().await;
        let gate_token = state.gate_access.create("__gate__".into()).await;
        let app = build_router(state);

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(
            health
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .unwrap(),
            "nosniff"
        );

        let webdav_challenge = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PROPFIND")
                    .uri("/dav/open")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_challenge.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            webdav_challenge
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap(),
            "Basic realm=\"Ycloud WebDAV\", charset=\"UTF-8\""
        );

        let mut remote_gate_request = Request::builder()
            .method("POST")
            .uri("/api/gate")
            .header(header::HOST, "ycloud.test")
            .header(header::ORIGIN, "http://ycloud.test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"","password":""}"#))
            .unwrap();
        remote_gate_request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 168, 2, 10], 40_000))));
        let remote_gate = app.clone().oneshot(remote_gate_request).await.unwrap();
        let remote_gate_body = to_bytes(remote_gate.into_body(), 4096).await.unwrap();
        let remote_gate_json: serde_json::Value =
            serde_json::from_slice(&remote_gate_body).unwrap();
        assert_eq!(remote_gate_json["success"], false);

        let mut local_gate_request = Request::builder()
            .method("POST")
            .uri("/api/gate")
            .header(header::HOST, "ycloud.test")
            .header(header::ORIGIN, "http://ycloud.test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"","password":""}"#))
            .unwrap();
        local_gate_request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_001))));
        let local_gate = app.clone().oneshot(local_gate_request).await.unwrap();
        let local_gate_body = to_bytes(local_gate.into_body(), 4096).await.unwrap();
        let local_gate_json: serde_json::Value = serde_json::from_slice(&local_gate_body).unwrap();
        assert_eq!(local_gate_json["success"], true);

        let files = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(files.status(), StatusCode::UNAUTHORIZED);

        let gate_files = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files")
                    .header(header::COOKIE, format!("gate_access={gate_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(gate_files.status(), StatusCode::OK);
        let gate_files_body = to_bytes(gate_files.into_body(), 4096).await.unwrap();
        let gate_files_json: serde_json::Value = serde_json::from_slice(&gate_files_body).unwrap();
        assert_eq!(gate_files_json["can_write"], false);

        let gate_write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mkdir")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("gate_access={gate_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"forbidden"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(gate_write.status(), StatusCode::FORBIDDEN);

        let basic_write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mkdir")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::AUTHORIZATION, "Basic YWRtaW46dGVzdC1wYXNzd29yZA==")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"basic-forbidden"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(basic_write.status(), StatusCode::UNAUTHORIZED);

        let admin_write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mkdir")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"allowed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admin_write.status(), StatusCode::OK);

        let nested_admin_write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mkdir?path=%2Fallowed")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"nested"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(nested_admin_write.status(), StatusCode::OK);
        assert!(tokio::fs::metadata(root.join("allowed").join("nested"))
            .await
            .unwrap()
            .is_dir());

        let limits_update = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/admin/limits")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"max_upload_bytes":2097152,"max_archive_bytes":3145728,"max_archive_entries":2}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(limits_update.status(), StatusCode::OK);

        let limits_list = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(limits_list.status(), StatusCode::OK);
        let limits_body = to_bytes(limits_list.into_body(), 16 * 1024).await.unwrap();
        let limits_json: serde_json::Value = serde_json::from_slice(&limits_body).unwrap();
        assert_eq!(limits_json["max_upload_bytes"], 2_097_152);
        assert_eq!(limits_json["max_archive_bytes"], 3_145_728);
        assert_eq!(limits_json["max_archive_entries"], 2);

        let manual_block = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/security/block")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"entry":"admin","ip":"192.0.2.10"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(manual_block.status(), StatusCode::OK);

        let manual_unblock = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/security/unblock")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"entry":"admin","ip":"192.0.2.10"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(manual_unblock.status(), StatusCode::OK);

        tokio::fs::write(
            root.join("allowed").join("nested").join("file.txt"),
            b"data",
        )
        .await
        .unwrap();
        let archive_entry_limit = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archive/prepare")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"paths":["/allowed"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(archive_entry_limit.status(), StatusCode::BAD_REQUEST);

        let favicon = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/favicon.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(favicon.status(), StatusCode::OK);
        assert_eq!(
            favicon.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/svg+xml"
        );

        let anonymous_admin = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous_admin.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            anonymous_admin.headers().get(header::LOCATION).unwrap(),
            "/browse"
        );

        let authenticated_admin = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authenticated_admin.status(), StatusCode::OK);

        tokio::fs::write(root.join("duplicate-delete.txt"), b"data")
            .await
            .unwrap();
        let partial_batch = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/batch/delete")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"paths":["duplicate-delete.txt","duplicate-delete.txt"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(partial_batch.status(), StatusCode::MULTI_STATUS);
        let partial_body = to_bytes(partial_batch.into_body(), 8192).await.unwrap();
        let partial_json: serde_json::Value = serde_json::from_slice(&partial_body).unwrap();
        assert_eq!(partial_json["success"], 1);
        assert_eq!(partial_json["failed"], 1);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
