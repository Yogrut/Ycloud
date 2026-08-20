use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::Response,
};
use futures_util::StreamExt;
use std::time::Duration;

use crate::{
    auth,
    config::Share,
    file_access::share_storage_path,
    login_security::LoginEntry,
    security::ClientIp,
    state::AppState,
    storage::{FileResponseMode, ResolvedPath},
    webdav_path::{
        display_relative_path, is_write_method, join_relative, parse_destination, parse_share_path,
        percent_encode,
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

    let destination = headers
        .get("destination")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if method == Method::DELETE && sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    match method.as_str() {
        "PROPFIND" => handle_propfind(&state, &share, &sub_path, &headers).await,
        "GET" => handle_get(&state, &share, &sub_path, &headers).await,
        "HEAD" => handle_head(&state, &share, &sub_path, &headers).await,
        "PUT" => handle_put(&state, &share, &sub_path, &headers, body).await,
        "DELETE" => handle_delete(&state, &share, &sub_path).await,
        "MKCOL" => handle_mkcol(&state, &share, &sub_path).await,
        "MOVE" => {
            handle_move_or_copy(&state, &share, &sub_path, destination, &headers, false).await
        }
        "COPY" => handle_move_or_copy(&state, &share, &sub_path, destination, &headers, true).await,
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
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    let target = resolve_existing(state, share, sub_path).await?;
    let metadata = state
        .storage
        .metadata(&target)
        .await
        .map_err(|error| error.status())?;
    let base_url = format!("/dav/{}", percent_encode(&share.name));
    let display_relative = display_relative_path(share, target.relative());
    let mut responses = vec![propfind_entry(&target, &metadata, display_relative)];

    let depth = headers
        .get("depth")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("1");
    if metadata.is_dir() && depth != "0" {
        let mut directory = tokio::fs::read_dir(target.absolute())
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        let mut count = 0;
        while count < state.storage.max_list_entries() {
            let Some(entry) = directory
                .next_entry()
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            else {
                break;
            };
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR)
            {
                continue;
            }
            let metadata = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if crate::storage::is_link_or_reparse_point(&metadata) {
                continue;
            }
            let storage_relative =
                join_relative(target.relative(), &entry.file_name().to_string_lossy());
            let child = state
                .storage
                .resolve_existing(&storage_relative)
                .await
                .map_err(|error| error.status())?;
            responses.push(propfind_entry(
                &child,
                &metadata,
                display_relative_path(share, child.relative()),
            ));
            count += 1;
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
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    let target = resolve_existing(state, share, sub_path).await?;
    state
        .storage
        .stream_file(&target, headers, FileResponseMode::WebDav)
        .await
        .map_err(|error| error.status())
}

async fn handle_head(
    state: &AppState,
    share: &Share,
    sub_path: &str,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    let mut response = handle_get(state, share, sub_path, headers).await?;
    *response.body_mut() = Body::empty();
    Ok(response)
}

async fn handle_put(
    state: &AppState,
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
    let mut writer = match expected_bytes {
        Some(bytes) => {
            state
                .storage
                .begin_atomic_write_with_expected(&storage_path, bytes)
                .await
        }
        None => state.storage.begin_atomic_write(&storage_path).await,
    }
    .map_err(|error| error.status())?;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        writer
            .write_chunk(&chunk)
            .await
            .map_err(|error| error.status())?;
    }
    writer.commit().await.map_err(|error| error.status())?;
    empty_response(StatusCode::CREATED)
}

async fn handle_delete(
    state: &AppState,
    share: &Share,
    sub_path: &str,
) -> Result<Response, StatusCode> {
    let target = resolve_existing(state, share, sub_path).await?;
    state
        .storage
        .remove(&target)
        .await
        .map_err(|error| error.status())?;
    empty_response(StatusCode::NO_CONTENT)
}

async fn handle_mkcol(
    state: &AppState,
    share: &Share,
    sub_path: &str,
) -> Result<Response, StatusCode> {
    if sub_path.trim_matches('/').is_empty() {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    let target = resolve_write(state, share, sub_path).await?;
    state
        .storage
        .create_directory(&target)
        .await
        .map_err(|error| error.status())?;
    empty_response(StatusCode::CREATED)
}

async fn handle_move_or_copy(
    state: &AppState,
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
    let source = resolve_existing(state, share, sub_path).await?;
    let target = resolve_write(state, share, &destination_path).await?;

    // [稳定] Reject destructive overwrite. Clients can DELETE explicitly,
    // making failure and recovery behavior observable instead of implicit.
    if headers
        .get("overwrite")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "F")
        && tokio::fs::try_exists(target.absolute())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::CONFLICT);
    }

    if copy {
        state
            .storage
            .copy_path(&source, &target)
            .await
            .map_err(|error| error.status())?;
    } else {
        state
            .storage
            .move_path(&source, &target)
            .await
            .map_err(|error| error.status())?;
    }
    empty_response(StatusCode::CREATED)
}

fn propfind_entry(
    path: &ResolvedPath,
    metadata: &std::fs::Metadata,
    display_relative: String,
) -> webdav_xml::PropfindResponseEntry {
    let is_dir = metadata.is_dir();
    webdav_xml::PropfindResponseEntry {
        href: if display_relative.is_empty() {
            "/".into()
        } else {
            format!("/{}", percent_encode(&display_relative))
        },
        displayname: path
            .absolute()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string(),
        is_dir,
        content_length: if is_dir { 0 } else { metadata.len() },
        last_modified: metadata
            .modified()
            .map(|modified| webdav_xml::to_rfc1123(&modified))
            .unwrap_or_else(|_| webdav_xml::to_rfc1123(&std::time::SystemTime::UNIX_EPOCH)),
        content_type: if is_dir {
            "httpd/unix-directory".into()
        } else {
            mime_guess::from_path(path.absolute())
                .first_or_octet_stream()
                .to_string()
        },
    }
}

async fn resolve_existing(
    state: &AppState,
    share: &Share,
    sub_path: &str,
) -> Result<ResolvedPath, StatusCode> {
    state
        .storage
        .resolve_existing(&share_storage_path(share, sub_path))
        .await
        .map_err(|error| error.status())
}

async fn resolve_write(
    state: &AppState,
    share: &Share,
    sub_path: &str,
) -> Result<ResolvedPath, StatusCode> {
    state
        .storage
        .resolve_for_write(&share_storage_path(share, sub_path))
        .await
        .map_err(|error| error.status())
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
