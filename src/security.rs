use axum::{
    extract::{ConnectInfo, Request, State},
    http::{
        header::{self, HeaderName, HeaderValue},
        Method, StatusCode,
    },
    middleware::Next,
    response::Response,
};
use std::net::{IpAddr, SocketAddr};

use crate::config::Config;

#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

pub fn session_cookie(name: &str, value: &str, max_age: u64, secure: bool) -> String {
    let secure_attribute = if secure { "; Secure" } else { "" };
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure_attribute}"
    )
}

pub fn clear_cookie(name: &str, secure: bool) -> String {
    session_cookie(name, "", 0, secure)
}

pub async fn proxy_boundary_middleware(
    State(state): State<crate::state::AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip())
        .or_else(|| {
            state
                .config
                .bind_address
                .is_loopback()
                .then_some(state.config.bind_address)
        })
        .ok_or(StatusCode::FORBIDDEN)?;
    let config = crate::domain_binding::policy(&state).await;
    validate_request_host(&config, request.headers())?;
    let secure_cookies = config.secure_cookies;
    request.extensions_mut().insert(config);
    request.extensions_mut().insert(ClientIp(peer_ip));
    let mut response = next.run(request).await;
    // Handlers share deployment state; HTTPS bindings must also secure cookies
    // when the deployment started in LAN mode. Never strip an existing Secure flag.
    if secure_cookies {
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| {
                let raw = value
                    .to_str()
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                if raw
                    .split(';')
                    .any(|part| part.trim().eq_ignore_ascii_case("secure"))
                {
                    Ok(value.clone())
                } else {
                    HeaderValue::from_str(&format!("{raw}; Secure"))
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        response.headers_mut().remove(header::SET_COOKIE);
        for mut cookie in cookies {
            cookie.set_sensitive(true);
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }
    Ok(response)
}

fn validate_request_host(
    config: &Config,
    headers: &axum::http::HeaderMap,
) -> Result<(), StatusCode> {
    let raw_host = single_header(headers, header::HOST.as_str())?;
    let normalized = raw_host.to_ascii_lowercase();
    if let Some(public_host) = config.public_host.as_deref() {
        return (normalized == public_host.to_ascii_lowercase())
            .then_some(())
            .ok_or(StatusCode::FORBIDDEN);
    }
    if config.allowed_hosts.contains(&normalized) {
        return Ok(());
    }
    let authority = raw_host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let port_matches = authority.port_u16() == Some(config.port)
        || (authority.port_u16().is_none() && config.port == 80);
    if !port_matches {
        return Err(StatusCode::FORBIDDEN);
    }
    let host = authority.host();
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    host.parse::<IpAddr>()
        .map(|_| ())
        .map_err(|_| StatusCode::FORBIDDEN)
}

fn single_header<'a>(
    headers: &'a axum::http::HeaderMap,
    name: &str,
) -> Result<&'a str, StatusCode> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .ok_or(StatusCode::BAD_REQUEST)?
        .to_str()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if values.next().is_some() || value.contains(',') || value.trim() != value || value.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(value)
}

pub async fn csrf_middleware(
    State(config): State<Config>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = request.extensions().get::<Config>().unwrap_or(&config);
    if !csrf_request_allowed(config, request.method(), request.headers()) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

fn csrf_request_allowed(config: &Config, method: &Method, headers: &axum::http::HeaderMap) -> bool {
    if !is_mutating(method) {
        return true;
    }
    let browser_context = uses_cookie_auth(headers)
        || headers.contains_key(header::ORIGIN)
        || headers.contains_key("sec-fetch-site");
    if !browser_context {
        // Preserve non-browser API clients, which do not send browser origin
        // metadata. Browser writes are checked even before login sets a cookie,
        // preventing login CSRF from replacing the current browser identity.
        return true;
    }
    let same_origin_fetch = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "same-origin");
    same_origin_fetch || origin_matches(config, headers)
}

pub async fn security_headers_middleware(
    State(config): State<Config>,
    request: Request,
    next: Next,
) -> Response {
    let config = request
        .extensions()
        .get::<Config>()
        .cloned()
        .unwrap_or(config);
    let sensitive_response =
        request.uri().path().starts_with("/api/") || request.uri().path().starts_with("/dav/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if sensitive_response {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if config.is_public_mode() {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    // Multiple CSP policies are enforced together. Do not replace the stricter
    // sandbox policy attached by the untrusted-file response layer.
    headers.append(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; \
             form-action 'self'; script-src 'self'; style-src 'self'; \
             img-src 'self' data: blob:; media-src 'self' blob:; frame-src 'self'",
        ),
    );
    response
}

fn is_mutating(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn uses_cookie_auth(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|cookie| {
            cookie.contains("session=")
                || cookie.contains("gate_access=")
                || cookie.contains("folder_key_")
        })
}

fn origin_matches(config: &Config, headers: &axum::http::HeaderMap) -> bool {
    if let Some(expected) = config.public_base_url.as_deref() {
        return headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|origin| origin == expected);
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|origin_host| origin_host == host)
}

#[cfg(test)]
mod tests {
    use super::{
        clear_cookie, csrf_request_allowed, origin_matches, session_cookie, validate_request_host,
    };
    use crate::config::Config;
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn session_cookie_is_http_only_and_strict() {
        let cookie = session_cookie("session", "token", 60, true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(clear_cookie("session", false).contains("Max-Age=0"));
    }

    #[test]
    fn origin_must_match_host_exactly() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("cloud.local:18473"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://cloud.local:18473"),
        );
        assert!(origin_matches(&local_config(), &headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.cloud.local:18473"),
        );
        assert!(!origin_matches(&local_config(), &headers));
    }

    #[test]
    fn browser_login_write_requires_same_origin_before_cookie_exists() {
        let config = local_config();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("cloud.local:18473"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://attacker.example"),
        );
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(!csrf_request_allowed(
            &config,
            &axum::http::Method::POST,
            &headers
        ));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://cloud.local:18473"),
        );
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        assert!(csrf_request_allowed(
            &config,
            &axum::http::Method::POST,
            &headers
        ));

        let mut api_headers = HeaderMap::new();
        api_headers.insert(header::HOST, HeaderValue::from_static("cloud.local:18473"));
        assert!(csrf_request_allowed(
            &config,
            &axum::http::Method::POST,
            &api_headers
        ));
    }

    #[test]
    fn local_mode_rejects_dns_rebinding_hosts() {
        let config = local_config();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:18473"));
        assert!(validate_request_host(&config, &headers).is_ok());
        headers.insert(
            header::HOST,
            HeaderValue::from_static("attacker.example:18473"),
        );
        assert_eq!(
            validate_request_host(&config, &headers),
            Err(axum::http::StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn explicit_local_host_is_exact_and_port_bound() {
        let mut config = local_config();
        config.allowed_hosts.insert("ycloud.test:18473".into());
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("ycloud.test:18473"));
        assert!(validate_request_host(&config, &headers).is_ok());
        headers.insert(
            header::HOST,
            HeaderValue::from_static("sub.ycloud.test:18473"),
        );
        assert!(validate_request_host(&config, &headers).is_err());
    }

    #[test]
    fn direct_http_accepts_public_and_private_ip_hosts() {
        let config = local_config();
        for host in ["127.0.0.1:18473", "192.168.2.86:18473", "203.0.113.8:18473"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
            assert!(validate_request_host(&config, &headers).is_ok(), "{host}");
        }
    }

    fn local_config() -> Config {
        Config {
            bind_address: "127.0.0.1".parse().unwrap(),
            port: 18_473,
            storage_path: "storage".into(),
            local_mounts: crate::storage_catalog::LocalMountCatalog::new(
                "storage".into(),
                Vec::new(),
            )
            .unwrap(),
            config_path: "config.json".into(),
            max_upload_bytes: 1,
            max_upload_batch_bytes: 1,
            max_upload_batch_entries: 1,
            max_archive_bytes: 1,
            max_archive_entries: 1,
            io_concurrency: 1,
            max_list_entries: 1,
            request_timeout_secs: 1,
            upload_timeout_secs: 1,
            disk_reserve_bytes: 0,
            secure_cookies: false,
            public_base_url: None,
            public_host: None,
            allowed_hosts: Default::default(),
            s3_allowed_endpoints: Default::default(),
            transaction_auth_key: [0x31; 32],
        }
    }
}
