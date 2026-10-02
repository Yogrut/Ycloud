use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};

mod access;

use access::verify_share_access;

use crate::{
    config::Share,
    file_access::share_storage_path,
    security::ClientIp,
    state::AppState,
    storage::FileResponseMode,
    storage_backend::{BackendMetadata, StorageBackend},
    webdav_path::{display_relative_path, parse_destination, percent_encode},
    webdav_xml,
};

const MAX_CONTROL_BODY_BYTES: usize = 64 * 1024;

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
        axum::body::to_bytes(body, MAX_CONTROL_BODY_BYTES)
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

    if method == Method::DELETE && sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    match method.as_str() {
        "PROPFIND" => handle_propfind(&state, &backend, &share, &sub_path, &headers).await,
        "GET" | "HEAD" => handle_read(&state, &backend, &share, &sub_path, &method, &headers).await,
        "PUT" => handle_put(&state, &backend, &share, &sub_path, &headers, body).await,
        "DELETE" => handle_delete(&backend, &share, &sub_path).await,
        "MKCOL" => handle_mkcol(&backend, &share, &sub_path).await,
        "MOVE" => handle_move_or_copy(&backend, &share, &sub_path, &headers, false).await,
        "COPY" => handle_move_or_copy(&backend, &share, &sub_path, &headers, true).await,
        // [稳定 + 安全] DAV class-2 locks were removed because the previous
        // implementation returned tokens without storing or enforcing them.
        "LOCK" | "UNLOCK" | "PROPPATCH" => Err(StatusCode::NOT_IMPLEMENTED),
        _ => Err(StatusCode::METHOD_NOT_ALLOWED),
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

async fn handle_read(
    state: &AppState,
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    method: &Method,
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
    // HEAD keeps the same file headers without transferring or charging bytes.
    if method == Method::HEAD {
        *response.body_mut() = Body::empty();
        return Ok(response);
    }
    let response = state
        .traffic
        .download(response, "webdav".into())
        .await
        .map_err(|error| error.status())?;
    Ok(state.download_limiter.wrap_response(response))
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
    let result = backend
        .create_directory(&share_storage_path(share, sub_path))
        .await;
    mutation_response(result, StatusCode::CREATED)
}

async fn handle_move_or_copy(
    backend: &StorageBackend,
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
    copy: bool,
) -> Result<Response, StatusCode> {
    if sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let destination = headers
        .get("destination")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let destination_path =
        parse_destination(destination, &share.name, request_host).ok_or(StatusCode::FORBIDDEN)?;
    let source = share_storage_path(share, sub_path);
    let target = share_storage_path(share, &destination_path);

    let result = if copy {
        backend.copy_path(&source, &target).await
    } else {
        backend.move_path(&source, &target).await
    };
    mutation_response(result, StatusCode::CREATED)
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
    use crate::error::{AppError, CleanupState, CommitState};
    use base64::Engine;
    use tower::ServiceExt;

    #[tokio::test]
    async fn mutation_response_preserves_commit_evidence_and_retry_rules() {
        for (commit, cleanup, status, expected_commit, retry) in [
            (
                CommitState::NotCommitted,
                CleanupState::Complete,
                StatusCode::INTERNAL_SERVER_ERROR,
                "not_committed",
                "after_correction",
            ),
            (
                CommitState::Committed,
                CleanupState::Pending,
                StatusCode::CONFLICT,
                "committed",
                "do_not_repeat",
            ),
            (
                CommitState::Unknown,
                CleanupState::Unknown,
                StatusCode::CONFLICT,
                "unknown",
                "verify_first",
            ),
        ] {
            let error = AppError::internal("private").with_operation(commit, cleanup);
            let response = mutation_response::<()>(Err(error), StatusCode::CREATED).unwrap();
            assert_eq!(response.status(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["operation"]["commit"], expected_commit);
            assert_eq!(
                body["error"]["operation"]["cleanup"],
                serde_json::to_value(cleanup).unwrap()
            );
            assert_eq!(body["error"]["operation"]["retry"], retry);
            assert!(!String::from_utf8_lossy(&bytes).contains("private"));
        }
    }

    #[tokio::test]
    async fn mutation_response_keeps_success_and_ordinary_error_contracts() {
        for status in [StatusCode::CREATED, StatusCode::NO_CONTENT] {
            let response = mutation_response(Ok(()), status).unwrap();
            assert_eq!(response.status(), status);
            assert!(axum::body::to_bytes(response.into_body(), 16)
                .await
                .unwrap()
                .is_empty());
        }
        for (error, status) in [
            (AppError::NotFound, StatusCode::NOT_FOUND),
            (AppError::Forbidden, StatusCode::FORBIDDEN),
            (
                AppError::BadRequest("invalid path".into()),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            assert_eq!(
                mutation_response::<()>(Err(error), StatusCode::CREATED).err(),
                Some(status)
            );
        }
    }

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
            ..crate::config::ConfigFile::with_test_storage()
        };
        let state = crate::test_support::app_state(&directory, persisted).await;
        tokio::fs::write(
            state.config.storage_path.join("notes.txt"),
            b"ordinary notes",
        )
        .await
        .unwrap();
        let app = crate::app::build_router(state.clone());
        let mut transferred_bytes = 0;
        for (method, range, expected_status, expected_length, expected_body) in [
            (Method::HEAD, None, StatusCode::OK, "14", &b""[..]),
            (
                Method::GET,
                None,
                StatusCode::OK,
                "14",
                &b"ordinary notes"[..],
            ),
            (
                Method::HEAD,
                Some("bytes=9-13"),
                StatusCode::PARTIAL_CONTENT,
                "5",
                &b""[..],
            ),
            (
                Method::GET,
                Some("bytes=9-13"),
                StatusCode::PARTIAL_CONTENT,
                "5",
                &b"notes"[..],
            ),
        ] {
            let mut request = axum::http::Request::builder()
                .method(method)
                .uri("/dav/documents/notes.txt")
                .header(header::HOST, "127.0.0.1:18473")
                .header(
                    header::AUTHORIZATION,
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD
                            .encode("reader:contract-password")
                    ),
                )
                .extension(axum::extract::ConnectInfo(
                    "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
                ));
            if let Some(range) = range {
                request = request.header(header::RANGE, range);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected_status);
            assert_eq!(response.headers()[header::CONTENT_LENGTH], expected_length);
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
                expected_body
            );
            transferred_bytes += expected_body.len() as u64;
            let axum::Json(traffic) = crate::traffic::info(
                State(state.clone()),
                axum::extract::Query(crate::traffic::TrafficQuery {
                    start: None,
                    end: None,
                }),
            )
            .await
            .unwrap();
            assert_eq!(traffic.total.download, transferred_bytes);
        }
    }
}
