use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use std::time::Duration;

use crate::{
    auth,
    config::Share,
    file_access::share_storage_path,
    login_security::LoginEntry,
    security::ClientIp,
    state::AppState,
    storage::FileResponseMode,
    storage_backend::{BackendMetadata, StorageBackend},
    webdav_path::{
        display_relative_path, is_write_method, parse_destination, parse_share_path, percent_encode,
    },
    webdav_xml,
};

pub async fn webdav_handler(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    dav_path: Option<Path<String>>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, StatusCode> {
    let _request_permit = state
        .webdav_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    if method == Method::OPTIONS {
        return options_response();
    }
    let body = if method == Method::PUT {
        body
    } else {
        axum::body::to_bytes(body, 64 * 1024)
            .await
            .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
        Body::empty()
    };

    let dav_path = dav_path.map(|path| path.0).unwrap_or_default();
    let client_ip = client_ip.map(|Extension(client)| client.0);
    let (share, sub_path) =
        match verify_share_access(&state, &dav_path, &headers, &method, client_ip).await {
            Ok(access) => access,
            Err(StatusCode::UNAUTHORIZED) => return Ok(basic_auth_challenge()),
            Err(status) => return Err(status),
        };
    let backend = state
        .storage_backend(&share.storage_id)
        .await
        .map_err(|error| error.status())?;

    let destination = headers
        .get("destination")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if method == Method::DELETE && sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    match method.as_str() {
        "PROPFIND" => handle_propfind(&state, &backend, &share, &sub_path, &headers).await,
        "GET" => handle_get(&state, &backend, &share, &sub_path, &headers).await,
        "HEAD" => handle_head(&state, &backend, &share, &sub_path, &headers).await,
        "PUT" => handle_put(&state, &backend, &share, &sub_path, &headers, body).await,
        "DELETE" => handle_delete(&backend, &share, &sub_path).await,
        "MKCOL" => handle_mkcol(&backend, &share, &sub_path).await,
        "MOVE" => {
            handle_move_or_copy(&backend, &share, &sub_path, destination, &headers, false).await
        }
        "COPY" => {
            handle_move_or_copy(&backend, &share, &sub_path, destination, &headers, true).await
        }
        // [稳定 + 安全] DAV class-2 locks were removed because the previous
        // implementation returned tokens without storing or enforcing them.
        "LOCK" | "UNLOCK" | "PROPPATCH" => Err(StatusCode::NOT_IMPLEMENTED),
        _ => Err(StatusCode::METHOD_NOT_ALLOWED),
    }
}

async fn verify_share_access(
    state: &AppState,
    dav_path: &str,
    headers: &HeaderMap,
    method: &Method,
    client_ip: Option<std::net::IpAddr>,
) -> Result<(Share, String), StatusCode> {
    let (share_name, sub_path) = parse_share_path(dav_path);
    let share = {
        let config = state.config_file.read().await;
        config
            .shares
            .iter()
            .find(|share| share.name == share_name)
            .cloned()
    }
    .ok_or(StatusCode::NOT_FOUND)?;
    if !share.webdav_enabled {
        return Err(StatusCode::FORBIDDEN);
    }
    let failure_key = client_ip.unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let _attempt_guard = if headers.contains_key(header::AUTHORIZATION) {
        Some(state.login_attempts.lock().await)
    } else {
        None
    };
    if state
        .login_security
        .is_blocked(LoginEntry::WebDav, failure_key)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if let Err(status) = verify_share_basic_auth(state, &share, headers).await {
        // A challenge without credentials is the normal first step of HTTP Basic
        // authentication. Counting it as a failed password caused WebDAV clients
        // with parallel discovery requests to lock themselves out before retrying.
        if status == StatusCode::UNAUTHORIZED && headers.contains_key(header::AUTHORIZATION) {
            state
                .login_security
                .record_failure(
                    LoginEntry::WebDav,
                    failure_key,
                    headers
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok()),
                    LoginEntry::WebDav.fixed_policy(),
                )
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        }
        return Err(status);
    }
    state
        .login_security
        .record_success(
            LoginEntry::WebDav,
            failure_key,
            headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if share.readonly && is_write_method(method) {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok((share, sub_path.to_string()))
}

async fn verify_share_basic_auth(
    state: &AppState,
    share: &Share,
    headers: &HeaderMap,
) -> Result<(), StatusCode> {
    let Some((username, password)) = auth::extract_basic_auth(headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let password_characters = password.chars().count();
    if password_characters == 0 || password_characters > 1_024 || password.len() > 4_096 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (Some(expected_username), Some(password_hash)) =
        (share.username.as_ref(), share.password_hash.as_ref())
    else {
        // Public, passwordless WebDAV is intentionally not supported.
        return Err(StatusCode::UNAUTHORIZED);
    };
    if username != *expected_username {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match state
        .passwords
        .verify_with_timeout(password_hash.clone(), password, Duration::from_secs(3))
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(StatusCode::UNAUTHORIZED),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

async fn handle_propfind(
    state: &AppState,
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    let target = share_storage_path(share, sub_path);
    let metadata = backend
        .metadata(&target)
        .await
        .map_err(|error| error.status())?;
    let base_url = format!("/dav/{}", percent_encode(&share.name));
    let display_relative = display_relative_path(share, &target);
    let mut responses = vec![propfind_entry(&target, &metadata, display_relative)];

    let depth = headers
        .get("depth")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("1");
    if metadata.is_dir && depth != "0" {
        let (entries, truncated) = backend
            .list_directory(&target, state.config.max_list_entries)
            .await
            .map_err(|error| error.status())?;
        if truncated {
            return Err(StatusCode::INSUFFICIENT_STORAGE);
        }
        for entry in entries {
            let child = BackendMetadata {
                is_dir: entry.is_dir,
                size: entry.size,
                modified_unix: entry.modified_unix,
                content_type: None,
                version_tag: None,
            };
            responses.push(propfind_entry(
                &entry.relative,
                &child,
                display_relative_path(share, &entry.relative),
            ));
        }
    }

    let xml = webdav_xml::build_multistatus(&responses, &base_url);
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(xml))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn handle_get(
    state: &AppState,
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    let response = backend
        .stream_file(
            &share_storage_path(share, sub_path),
            headers,
            FileResponseMode::WebDav,
        )
        .await
        .map_err(|error| error.status())?;
    let response = state
        .traffic
        .download(response, "webdav".into())
        .await
        .map_err(|error| error.status())?;
    Ok(state.download_limiter.wrap_response(response))
}

async fn handle_head(
    _state: &AppState,
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    let mut response = backend
        .stream_file(
            &share_storage_path(share, sub_path),
            headers,
            FileResponseMode::WebDav,
        )
        .await
        .map_err(|error| error.status())?;
    *response.body_mut() = Body::empty();
    Ok(response)
}

async fn handle_put(
    state: &AppState,
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, StatusCode> {
    if sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let storage_path = share_storage_path(share, sub_path);
    let expected_bytes = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let max_upload_bytes = state.config_file.read().await.max_upload_bytes;
    state
        .traffic
        .preflight(
            "webdav",
            crate::traffic::Direction::Upload,
            expected_bytes.unwrap_or(1),
        )
        .await
        .map_err(|error| error.status())?;
    let (body, meter) =
        state
            .traffic
            .meter(body, "webdav".into(), crate::traffic::Direction::Upload);
    let result = backend
        .upload_file(
            &storage_path,
            state.upload_limiter.wrap_body(body),
            expected_bytes,
            max_upload_bytes,
            content_type,
        )
        .await;
    mutation_response(meter.finish(result), StatusCode::CREATED)
}

async fn handle_delete(
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
) -> Result<Response, StatusCode> {
    let result = backend.remove(&share_storage_path(share, sub_path)).await;
    mutation_response(result, StatusCode::NO_CONTENT)
}

async fn handle_mkcol(
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
) -> Result<Response, StatusCode> {
    if sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    backend
        .create_directory(&share_storage_path(share, sub_path))
        .await
        .map_err(|error| error.status())?;
    empty_response(StatusCode::CREATED)
}

async fn handle_move_or_copy(
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    destination: Option<String>,
    headers: &HeaderMap,
    copy: bool,
) -> Result<Response, StatusCode> {
    if sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let destination = destination.ok_or(StatusCode::BAD_REQUEST)?;
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let destination_path =
        parse_destination(&destination, &share.name, request_host).ok_or(StatusCode::FORBIDDEN)?;
    let source = share_storage_path(share, sub_path);
    let target = share_storage_path(share, &destination_path);

    if copy {
        return mutation_response(
            backend.copy_path(&source, &target).await,
            StatusCode::CREATED,
        );
    } else {
        backend
            .move_path(&source, &target)
            .await
            .map_err(|error| error.status())?;
    }
    empty_response(StatusCode::CREATED)
}

fn mutation_response<T>(
    result: crate::error::AppResult<T>,
    success: StatusCode,
) -> Result<Response, StatusCode> {
    match result {
        Ok(_) => empty_response(success),
        Err(error) if error.operation().is_some() => Ok(error.into_response()),
        Err(error) => Err(error.status()),
    }
}

#[cfg(test)]
#[tokio::test]
async fn mutation_response_keeps_unknown_operation_details() {
    let error = crate::error::AppError::internal("private").with_operation(
        crate::error::CommitState::Unknown,
        crate::error::CleanupState::Unknown,
    );
    let response = mutation_response::<()>(Err(error), StatusCode::CREATED).unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["operation"]["commit"], "unknown");
    assert_eq!(body["error"]["operation"]["retry"], "verify_first");
}

fn propfind_entry(
    path: &str,
    metadata: &BackendMetadata,
    display_relative: String,
) -> webdav_xml::PropfindResponseEntry {
    let is_dir = metadata.is_dir;
    webdav_xml::PropfindResponseEntry {
        href: if display_relative.is_empty() {
            "/".into()
        } else {
            format!("/{}", percent_encode(&display_relative))
        },
        displayname: path.rsplit('/').next().unwrap_or("").to_string(),
        is_dir,
        content_length: if is_dir { 0 } else { metadata.size },
        last_modified: metadata.modified_unix.map_or_else(
            || webdav_xml::to_rfc1123(&std::time::SystemTime::UNIX_EPOCH),
            |seconds| {
                chrono::DateTime::from_timestamp_secs(seconds).map_or_else(
                    || webdav_xml::to_rfc1123(&std::time::SystemTime::UNIX_EPOCH),
                    |value| value.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                )
            },
        ),
        content_type: if is_dir {
            "httpd/unix-directory".into()
        } else {
            metadata.content_type.clone().unwrap_or_else(|| {
                mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string()
            })
        },
    }
}

fn options_response() -> Result<Response, StatusCode> {
    Response::builder()
        .status(StatusCode::OK)
        .header("DAV", "1")
        .header(
            "Allow",
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, MOVE, COPY",
        )
        .header("MS-Author-Via", "DAV")
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn basic_auth_challenge() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Basic realm=\"Ycloud WebDAV\", charset=\"UTF-8\""),
    );
    response
}

fn empty_response(status: StatusCode) -> Result<Response, StatusCode> {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod response_contract_tests {
    use super::*;
    use base64::Engine;
    use tower::ServiceExt;

    #[tokio::test]
    async fn authenticated_dav_read_keeps_file_bytes_and_download_isolation() {
        let directory = crate::test_support::TestDirectory::new("dav-response");
        let persisted = crate::config::ConfigFile {
            shares: vec![Share {
                id: "contract-share".into(),
                storage_id: "primary".into(),
                name: "documents".into(),
                path: String::new(),
                username: Some("reader".into()),
                webdav_enabled: true,
                password_hash: Some(crate::config::hash_password("contract-password")),
                readonly: true,
            }],
            ..Default::default()
        };
        let state = crate::test_support::app_state(&directory, persisted).await;
        tokio::fs::write(
            state.config.storage_path.join("notes.txt"),
            b"ordinary notes",
        )
        .await
        .unwrap();
        let request = axum::http::Request::builder()
            .uri("/dav/documents/notes.txt")
            .header(header::HOST, "127.0.0.1:18473")
            .header(
                header::AUTHORIZATION,
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode("reader:contract-password")
                ),
            )
            .extension(axum::extract::ConnectInfo(
                "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(Body::empty())
            .unwrap();
        let response = crate::app::build_router(state)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;"));
        assert!(response
            .headers()
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .any(|value| value.to_str().unwrap().starts_with("sandbox;")));
        assert_eq!(
            &axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()[..],
            b"ordinary notes"
        );
    }
}
