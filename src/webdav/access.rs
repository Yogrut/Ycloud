//! WebDAV mount authentication, persistent login restrictions, and read-only grants.
//! Protocol dispatch and file I/O remain in the parent module.
use std::{net::IpAddr, time::Duration};

use axum::http::{header, HeaderMap, Method, StatusCode};

use crate::{
    auth,
    config::Share,
    login_security::LoginEntry,
    state::AppState,
    webdav_path::{is_write_method, parse_share_path},
};

const PASSWORD_VERIFY_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) async fn verify_share_access(
    state: &AppState,
    dav_path: &str,
    headers: &HeaderMap,
    method: &Method,
    client_ip: Option<IpAddr>,
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
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let failure_key = client_ip.unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));
    let _attempt_guard = if headers.contains_key(header::AUTHORIZATION) {
        Some(
            state
                .login_attempts
                .for_entry(LoginEntry::WebDav)
                .lock()
                .await,
        )
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
                    user_agent,
                    LoginEntry::WebDav.fixed_policy(),
                )
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        }
        return Err(status);
    }
    state
        .login_security
        .record_success(LoginEntry::WebDav, failure_key, user_agent)
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
    if !auth::valid_password_length(&password) {
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
        .verify_with_timeout(password_hash.clone(), password, PASSWORD_VERIFY_TIMEOUT)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(StatusCode::UNAUTHORIZED),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};

    use crate::{
        config::{hash_password, ConfigFile},
        login_security::EventQuery,
        test_support::{app_state, TestDirectory},
    };

    fn readonly_mount() -> Share {
        Share {
            id: "dav-mount".into(),
            storage_id: "primary".into(),
            name: "documents".into(),
            path: String::new(),
            username: Some("reader".into()),
            webdav_enabled: true,
            password_hash: Some(hash_password("mount-password")),
            readonly: true,
        }
    }

    #[tokio::test]
    async fn basic_challenges_do_not_count_as_failed_credentials() {
        let directory = TestDirectory::new("dav-login-restrictions");
        let state = app_state(
            &directory,
            ConfigFile {
                shares: vec![readonly_mount()],
                ..ConfigFile::with_test_storage()
            },
        )
        .await;
        let ip = IpAddr::from([127, 0, 0, 1]);
        let mut headers = HeaderMap::new();
        let maximum_failures = LoginEntry::WebDav.fixed_policy().maximum_failures;
        for _ in 0..=maximum_failures {
            assert_eq!(
                verify_share_access(&state, "documents", &headers, &Method::GET, Some(ip))
                    .await
                    .err(),
                Some(StatusCode::UNAUTHORIZED)
            );
        }
        let events = state
            .login_security
            .query_events(EventQuery::default())
            .await;
        assert_eq!(events.total, 0);
        assert!(!state
            .login_security
            .is_blocked(LoginEntry::WebDav, ip)
            .await
            .unwrap());

        headers.insert(header::AUTHORIZATION, "Basic invalid".parse().unwrap());
        for _ in 0..maximum_failures {
            assert_eq!(
                verify_share_access(&state, "documents", &headers, &Method::GET, Some(ip))
                    .await
                    .err(),
                Some(StatusCode::UNAUTHORIZED)
            );
        }
        assert_eq!(
            verify_share_access(&state, "documents", &headers, &Method::GET, Some(ip))
                .await
                .err(),
            Some(StatusCode::TOO_MANY_REQUESTS)
        );
        let events = state
            .login_security
            .query_events(EventQuery::default())
            .await;
        assert_eq!(events.total, maximum_failures as usize);
        assert!(events.events.iter().all(|view| !view.event.success));
    }

    #[tokio::test]
    async fn readonly_mount_authenticates_reads_but_denies_each_write_method() {
        let directory = TestDirectory::new("dav-readonly-access");
        let state = app_state(
            &directory,
            ConfigFile {
                shares: vec![readonly_mount()],
                ..ConfigFile::with_test_storage()
            },
        )
        .await;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Basic {}", STANDARD.encode("reader:mount-password"))
                .parse()
                .unwrap(),
        );
        let ip = IpAddr::from([127, 0, 0, 1]);
        for method in ["PUT", "DELETE", "MKCOL", "MOVE", "COPY"] {
            assert_eq!(
                verify_share_access(
                    &state,
                    "documents/notes.txt",
                    &headers,
                    &method.parse().unwrap(),
                    Some(ip),
                )
                .await
                .err(),
                Some(StatusCode::FORBIDDEN),
                "readonly mount accepted {method}"
            );
        }
        let (share, path) = verify_share_access(
            &state,
            "documents/notes.txt",
            &headers,
            &Method::GET,
            Some(ip),
        )
        .await
        .unwrap();
        assert_eq!(share.id, "dav-mount");
        assert_eq!(share.storage_id, "primary");
        assert_eq!(path, "notes.txt");
        let events = state
            .login_security
            .query_events(EventQuery::default())
            .await;
        // Authorization denials do not consume the failed-password allowance.
        assert_eq!(events.total, 1);
        assert!(events.events[0].event.success);
        assert!(!state
            .login_security
            .is_blocked(LoginEntry::WebDav, ip)
            .await
            .unwrap());
    }
}
