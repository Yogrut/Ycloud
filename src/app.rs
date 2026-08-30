use std::{sync::Arc, time::Duration};

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderName, Method, StatusCode},
    middleware,
    response::{IntoResponse, Response},
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
    upload_batch, webdav,
};

const MAX_BATCH_BODY_BYTES: usize = 256 * 1024;

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
        .route("/ready", get(readiness_handler))
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
        .route("/storages", get(api::browser_storages))
        .route("/files", get(api::list_files))
        .route("/download", get(api::download_file))
        .route(
            "/archive/prepare",
            post(archive::prepare_archive).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/archive", get(archive::download_archive))
        .route("/preview", get(api::preview_file))
        .merge(unlock_route);

    // Batch payloads contain only paths and a destination. Keep their JSON
    // envelope independent from the much larger raw upload body allowance so
    // deserialization cannot reserve upload-sized memory.
    let batch_api = Router::new()
        .route("/batch/delete", post(batch_operations::delete))
        .route("/batch/move", put(batch_operations::move_items))
        .route("/batch/copy", post(batch_operations::copy))
        .layer(DefaultBodyLimit::max(MAX_BATCH_BODY_BYTES));

    let write_api = Router::new()
        .route("/files", axum::routing::delete(api::delete_file))
        .route("/mkdir", post(api::create_directory))
        .route(
            "/upload/prepare",
            post(upload_batch::prepare_upload_batch).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/upload/cancel",
            post(upload_batch::cancel_upload_batch).layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .route("/upload", put(api::upload_file))
        .route("/rename", put(api::rename_file))
        .merge(batch_api)
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
        .route("/account/totp/setup", post(admin_api::setup_admin_totp))
        .route("/account/totp/enable", post(admin_api::enable_admin_totp))
        .route(
            "/account/totp",
            axum::routing::delete(admin_api::disable_admin_totp),
        )
        .route("/users", post(admin_api::create_user_account))
        .route(
            "/users/{id}",
            put(admin_api::update_user_account).delete(admin_api::delete_user_account),
        )
        .route("/storage/test", post(admin_api::test_s3_storage))
        .route("/storage/local/test", post(admin_api::test_local_storage))
        .route("/storage/s3/{id}", put(admin_api::update_s3_storage))
        .route(
            "/storage/pending",
            put(admin_api::stage_s3_storage).delete(admin_api::discard_pending_storage),
        )
        .route(
            "/storage/local",
            post(admin_api::add_local_storage).put(admin_api::update_local_storage),
        )
        .route(
            "/storage/activate",
            post(admin_api::activate_pending_storage),
        )
        .route(
            "/storage/{id}",
            put(admin_api::update_storage_access).delete(admin_api::delete_storage),
        )
        .route("/storage/{id}/default", put(admin_api::set_default_storage))
        .route("/limits", put(admin_api::update_transfer_limits))
        .route(
            "/security/settings",
            put(admin_api::update_login_security_settings),
        )
        .route("/security/events", get(admin_api::login_events))
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
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("missing");
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    uri = %request.uri(),
                    request_id = %request_id,
                )
            }),
        );
    let timeouts = RequestTimeouts {
        regular: Duration::from_secs(state.config.request_timeout_secs),
        upload: Duration::from_secs(state.config.upload_timeout_secs),
    };

    Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
        .route("/browse", get(serve_index))
        .route("/admin", get(serve_index))
        .route("/admin/{*rest}", get(serve_index))
        .route("/preview", get(serve_index))
        .route("/assets/app.css", get(serve_app_css))
        .route("/assets/app.js", get(serve_app_js))
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

async fn health_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(JsonStatus {
            status: "ok",
            storage: "unchecked",
        }),
    )
}

async fn readiness_handler(State(state): State<AppState>) -> impl IntoResponse {
    let default_storage_id = state.config_file.read().await.default_storage_id.clone();
    let ready = match state.storage_backend(&default_storage_id).await {
        Ok(backend) => backend.ready().await,
        Err(_) => false,
    };
    if ready {
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
    "../static/app/index.html"
);
embedded_handler!(
    serve_app_css,
    "text/css; charset=utf-8",
    "../static/app/assets/app.css"
);
embedded_handler!(
    serve_app_js,
    "application/javascript; charset=utf-8",
    "../static/app/assets/app.js"
);
embedded_handler!(serve_favicon, "image/svg+xml", "../static/favicon.svg");

async fn not_found(_request: Request) -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "Not Found")
}

#[cfg(test)]
mod tests {
    use super::{build_router, MAX_BATCH_BODY_BYTES};
    use crate::{
        config::{
            hash_password, Config, ConfigFile, S3AddressingStyle, S3Provider, S3StorageConfig,
            Share, StorageBackendConfig, StoragePermission, UserAccount, CONFIG_SCHEMA_VERSION,
        },
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
                local_mounts: crate::storage_catalog::LocalMountCatalog::new(
                    root.clone(),
                    Vec::new(),
                )
                .unwrap(),
                config_path: root.join("config.json"),
                max_upload_bytes: 2 * 1024 * 1024,
                max_upload_batch_bytes: 100 * 1024 * 1024 * 1024,
                max_upload_batch_entries: 10_000,
                max_archive_bytes: 100 * 1024 * 1024 * 1024,
                max_archive_entries: 100_000,
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
                s3_allowed_endpoints: Default::default(),
            },
            Arc::new(RwLock::new(ConfigFile {
                schema_version: CONFIG_SCHEMA_VERSION,
                admin_username: "admin".into(),
                admin_password_hash: hash_password("test-password"),
                global_web_password_hash: None,
                folder_locks: Vec::new(),
                shares: vec![Share {
                    id: "test-share".into(),
                    storage_id: crate::config::DEFAULT_STORAGE_ID.into(),
                    name: "open".into(),
                    path: String::new(),
                    username: Some("yogrut".into()),
                    webdav_enabled: true,
                    password_hash: Some(hash_password("webdav-password")),
                    readonly: false,
                }],
                user_accounts: vec![UserAccount {
                    id: "reader".into(),
                    username: "reader".into(),
                    password_hash: hash_password("reader-password"),
                    enabled: true,
                    permissions: vec![StoragePermission {
                        storage_id: crate::config::DEFAULT_STORAGE_ID.into(),
                        browse: true,
                        download: true,
                        upload: false,
                        create_directory: false,
                        rename: false,
                        move_items: false,
                        copy: false,
                        delete: false,
                    }],
                }],
                max_upload_bytes: 1024 * 1024,
                max_archive_bytes: 2 * 1024 * 1024,
                max_archive_entries: 100,
                pending_storage_instance: Some(crate::config::StorageInstanceConfig {
                    id: "pending-test".into(),
                    name: "Pending test".into(),
                    enabled: true,
                    allow_guest_access: false,
                    backend: StorageBackendConfig::S3(S3StorageConfig {
                        provider: S3Provider::AlibabaOss,
                        endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
                        bucket: "ycloud-test".into(),
                        region: "cn-hangzhou".into(),
                        prefix: "files/".into(),
                        addressing_style: S3AddressingStyle::VirtualHosted,
                        access_key_id: "pending-access-key".into(),
                        secret_access_key: "pending-secret-key".into(),
                        capacity_limit_bytes: None,
                    }),
                }),
                ..ConfigFile::default()
            })),
        )
        .await
        .unwrap();
        let admin_token = state.sessions.create().await;
        let reader_token = state.sessions.create_user("reader".into()).await;
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
        assert!(health.headers().get("x-request-id").is_some());
        assert_eq!(
            health
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .unwrap(),
            "nosniff"
        );

        let readiness = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(readiness.status(), StatusCode::OK);

        let admin_info = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/info")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admin_info.status(), StatusCode::OK);
        let admin_info_body = to_bytes(admin_info.into_body(), 16 * 1024).await.unwrap();
        let admin_info_json: serde_json::Value = serde_json::from_slice(&admin_info_body).unwrap();
        assert_eq!(
            admin_info_json["storage_instances"][0]["backend"]["type"],
            "local"
        );
        assert!(admin_info_json["storage_instances"][0]["backend"]["path"].is_string());
        assert_eq!(
            admin_info_json["pending_storage_instance"]["backend"]["type"],
            "s3"
        );
        assert_eq!(
            admin_info_json["pending_storage_instance"]["backend"]["has_access_key_id"],
            true
        );
        assert_eq!(
            admin_info_json["pending_storage_instance"]["backend"]["has_secret_access_key"],
            true
        );
        assert!(admin_info_json["local_storage_path"].is_string());
        assert_eq!(admin_info_json["local_mounts"][0]["mount_id"], "primary");
        assert_eq!(admin_info_json["local_mounts"][0]["storage_id"], "primary");
        assert!(admin_info_json["local_mounts"][0]["path"].is_string());
        assert_eq!(admin_info_json["user_accounts"][0]["username"], "reader");
        assert!(admin_info_json["user_accounts"][0]
            .get("password_hash")
            .is_none());
        let admin_info_text = String::from_utf8_lossy(&admin_info_body);
        assert!(!admin_info_text.contains("pending-access-key"));
        assert!(!admin_info_text.contains("pending-secret-key"));

        let reader_admin_info = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/info")
                    .header(header::COOKIE, format!("session={reader_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reader_admin_info.status(), StatusCode::UNAUTHORIZED);

        let reader_files = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files")
                    .header(header::COOKIE, format!("session={reader_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reader_files.status(), StatusCode::OK);
        let reader_files_body = to_bytes(reader_files.into_body(), 4096).await.unwrap();
        let reader_files_json: serde_json::Value =
            serde_json::from_slice(&reader_files_body).unwrap();
        assert_eq!(reader_files_json["is_admin"], false);
        assert_eq!(reader_files_json["page_size"], 20);
        assert!(reader_files_json.get("page_start").is_some());
        assert!(reader_files_json.get("next_cursor").is_some());
        assert_eq!(reader_files_json["capabilities"]["download"], true);
        assert_eq!(reader_files_json["capabilities"]["upload"], false);
        assert_eq!(reader_files_json["capabilities"]["delete"], false);

        let reader_write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mkdir")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={reader_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"reader-forbidden"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reader_write.status(), StatusCode::FORBIDDEN);

        let unapproved_s3_test = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/storage/test")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"provider":"minio","endpoint":"http://127.0.0.1:9000","bucket":"ycloud","region":"us-east-1","prefix":"data/","addressing_style":"path","access_key_id":"test-access","secret_access_key":"test-secret"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unapproved_s3_test.status(), StatusCode::FORBIDDEN);
        let rejected_body = to_bytes(unapproved_s3_test.into_body(), 16 * 1024)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&rejected_body).contains("test-secret"));

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

        let invalid_page_size = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/files?limit=25")
                    .header(header::COOKIE, format!("gate_access={gate_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_page_size.status(), StatusCode::BAD_REQUEST);

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

        const WEBDAV_AUTH: &str = "Basic eW9ncnV0OndlYmRhdi1wYXNzd29yZA==";
        let webdav_mkcol = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("MKCOL")
                    .uri("/dav/open/dav-test")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_mkcol.status(), StatusCode::CREATED);

        let webdav_put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/dav/open/dav-test/file.txt")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .header(header::CONTENT_LENGTH, "4")
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("data"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_put.status(), StatusCode::CREATED);

        let webdav_propfind = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PROPFIND")
                    .uri("/dav/open/dav-test")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .header("depth", "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_propfind.status(), StatusCode::MULTI_STATUS);
        let propfind_body = to_bytes(webdav_propfind.into_body(), 16 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&propfind_body).contains("file.txt"));

        let webdav_get = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dav/open/dav-test/file.txt")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_get.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(webdav_get.into_body(), 16).await.unwrap().as_ref(),
            b"data"
        );

        let webdav_copy = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("COPY")
                    .uri("/dav/open/dav-test/file.txt")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .header(header::HOST, "ycloud.test")
                    .header(
                        "destination",
                        "http://ycloud.test/dav/open/dav-test/copy.txt",
                    )
                    .header("overwrite", "F")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_copy.status(), StatusCode::CREATED);

        let webdav_move = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("MOVE")
                    .uri("/dav/open/dav-test/copy.txt")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .header(header::HOST, "ycloud.test")
                    .header(
                        "destination",
                        "http://ycloud.test/dav/open/dav-test/moved.txt",
                    )
                    .header("overwrite", "F")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_move.status(), StatusCode::CREATED);

        let webdav_delete = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/dav/open/dav-test/moved.txt")
                    .header(header::AUTHORIZATION, WEBDAV_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(webdav_delete.status(), StatusCode::NO_CONTENT);

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
                        r#"{"max_upload_bytes":2097152,"max_upload_batch_bytes":4194304,"max_upload_batch_entries":4,"max_archive_bytes":3145728,"max_archive_entries":2}"#,
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
        assert_eq!(limits_json["max_upload_batch_bytes"], 4_194_304);
        assert_eq!(limits_json["max_upload_batch_entries"], 4);
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
        assert_eq!(anonymous_admin.status(), StatusCode::OK);

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

        let app_asset = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(app_asset.status(), StatusCode::OK);
        assert_eq!(
            app_asset.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/javascript; charset=utf-8"
        );

        let removed_legacy_vue = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v2/admin/security")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(removed_legacy_vue.status(), StatusCode::NOT_FOUND);

        tokio::fs::write(root.join("duplicate-delete.txt"), b"data")
            .await
            .unwrap();
        let oversized_batch = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/batch/delete")
                    .header(header::HOST, "ycloud.test")
                    .header(header::ORIGIN, "http://ycloud.test")
                    .header(header::COOKIE, format!("session={admin_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"paths":["{}"]}}"#,
                        "x".repeat(MAX_BATCH_BODY_BYTES)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(oversized_batch.status(), StatusCode::PAYLOAD_TOO_LARGE);

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
