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
    let client_ip = request_client_ip(&config, peer_ip, request.headers())?;
    let secure_cookies = config.secure_cookies;
    request.extensions_mut().insert(config);
    request.extensions_mut().insert(ClientIp(client_ip));
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

fn request_client_ip(
    config: &Config,
    peer: IpAddr,
    headers: &axum::http::HeaderMap,
) -> Result<IpAddr, StatusCode> {
    let peer = peer.to_canonical();
    if !config.trusted_proxy_ips.contains(&peer) || !headers.contains_key("x-real-ip") {
        return Ok(peer);
    }
    // The configured proxy must overwrite this single header. Do not infer
    // trust from a domain, private address, or an arbitrary forwarded chain.
    single_header(headers, "x-real-ip")?
        .parse::<IpAddr>()
        .map(|ip| ip.to_canonical())
        .map_err(|_| StatusCode::BAD_REQUEST)
}

pub(crate) fn same_authority(
    left: &axum::http::uri::Authority,
    right: &axum::http::uri::Authority,
    default_port: u16,
) -> bool {
    let port = |value: &axum::http::uri::Authority| {
        // Authority::port() returns None for an out-of-range explicit port
        // as well as an absent/empty port. Only those get the scheme default.
        value.port_u16().or_else(|| {
            matches!(value.as_str().strip_prefix(value.host()), Some("" | ":"))
                .then_some(default_port)
        })
    };
    !left.as_str().contains('@')
        && !right.as_str().contains('@')
        && left.host().eq_ignore_ascii_case(right.host())
        && port(left).is_some()
        && port(left) == port(right)
}

fn validate_request_host(
    config: &Config,
    headers: &axum::http::HeaderMap,
) -> Result<(), StatusCode> {
    let raw_host = single_header(headers, header::HOST.as_str())?;
    let normalized = raw_host.to_ascii_lowercase();
    if let Some(public_host) = config.public_host.as_deref() {
        let expected = public_host
            .parse::<axum::http::uri::Authority>()
            .map_err(|_| StatusCode::FORBIDDEN)?;
        let actual = raw_host
            .parse::<axum::http::uri::Authority>()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        return same_authority(
            &expected,
            &actual,
            if config.secure_cookies { 443 } else { 80 },
        )
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
    State(state): State<crate::state::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let config = request
        .extensions()
        .get::<Config>()
        .cloned()
        .unwrap_or_else(|| state.config.clone());
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
    let policy = state.config_file.read().await;
    let mut origins = std::collections::BTreeSet::new();
    for instance in policy
        .storage_instances
        .iter()
        .filter(|instance| instance.enabled)
    {
        if let crate::config::StorageBackendConfig::S3(settings) = &instance.backend {
            if !settings.relay_upload {
                if let Ok(endpoint) = settings.endpoint.parse::<axum::http::Uri>() {
                    if let (Some(scheme), Some(authority)) =
                        (endpoint.scheme_str(), endpoint.authority())
                    {
                        origins.insert(format!("{scheme}://{authority}"));
                        if settings.addressing_style
                            == crate::config::S3AddressingStyle::VirtualHosted
                        {
                            origins.insert(format!("{scheme}://{}.{authority}", settings.bucket));
                        }
                    }
                }
            }
        }
    }
    let csp = format!("default-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; \
        form-action 'self'; script-src 'self'; style-src 'self'; \
        img-src 'self' data: blob:; media-src 'self' blob:; frame-src 'self'; connect-src 'self' {}", origins.into_iter().collect::<Vec<_>>().join(" "));
    let value = HeaderValue::from_str(&csp).unwrap_or_else(|_| HeaderValue::from_static(
        "default-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; frame-src 'self'; connect-src 'self'",
    ));
    headers.append(header::CONTENT_SECURITY_POLICY, value);
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
        clear_cookie, csrf_request_allowed, origin_matches, request_client_ip, session_cookie,
        validate_request_host,
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
    fn bound_hosts_use_effective_ports_without_accepting_other_authorities() {
        let mut config = local_config();
        config.public_host = Some("cloud.example".into());
        config.secure_cookies = true;
        for (host, accepted) in [
            ("cloud.example", true),
            ("cloud.example:", true),
            ("CLOUD.EXAMPLE:443", true),
            ("cloud.example:80", false),
            ("other.example:443", false),
            ("user@cloud.example", false),
            ("cloud.example:99999", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
            assert_eq!(
                validate_request_host(&config, &headers).is_ok(),
                accepted,
                "{host}"
            );
        }
        config.public_host = Some("cloud.example:8443".into());
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("cloud.example"));
        assert!(validate_request_host(&config, &headers).is_err());
        headers.insert(header::HOST, HeaderValue::from_static("cloud.example:8443"));
        assert!(validate_request_host(&config, &headers).is_ok());
    }

    #[test]
    fn real_ip_is_used_only_for_explicitly_trusted_peers() {
        let mut config = local_config();
        let peer = "192.0.2.10".parse().unwrap();
        let client = "198.51.100.25".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.25"));
        assert_eq!(request_client_ip(&config, peer, &headers), Ok(peer));
        config.trusted_proxy_ips.insert(peer);
        assert_eq!(request_client_ip(&config, peer, &headers), Ok(client));
        headers.remove("x-real-ip");
        headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.25"));
        assert_eq!(request_client_ip(&config, peer, &headers), Ok(peer));
    }

    #[test]
    fn trusted_real_ip_requires_one_valid_address_and_normalizes_mapped_ipv4() {
        let mut config = local_config();
        let peer = "192.0.2.10".parse().unwrap();
        config.trusted_proxy_ips.insert(peer);
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-real-ip",
            HeaderValue::from_static("::ffff:198.51.100.25"),
        );
        assert_eq!(
            request_client_ip(&config, "::ffff:192.0.2.10".parse().unwrap(), &headers),
            Ok("198.51.100.25".parse().unwrap())
        );
        for value in [
            "not-an-ip",
            "198.51.100.25:8080",
            "198.51.100.25, 192.0.2.1",
        ] {
            headers.insert("x-real-ip", HeaderValue::from_str(value).unwrap());
            assert_eq!(
                request_client_ip(&config, peer, &headers),
                Err(axum::http::StatusCode::BAD_REQUEST)
            );
        }
        headers.insert("x-real-ip", HeaderValue::from_static("2001:db8::25"));
        assert_eq!(
            request_client_ip(&config, peer, &headers),
            Ok("2001:db8::25".parse().unwrap())
        );
        headers.append("x-real-ip", HeaderValue::from_static("198.51.100.25"));
        assert_eq!(
            request_client_ip(&config, peer, &headers),
            Err(axum::http::StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            request_client_ip(&config, "192.0.2.11".parse().unwrap(), &headers),
            Ok("192.0.2.11".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn proxy_middleware_passes_the_selected_address_to_request_consumers() {
        use axum::{
            extract::{ConnectInfo, Extension},
            routing::get,
            Router,
        };
        use tower::ServiceExt;
        let directory = crate::test_support::TestDirectory::new("proxy-client-address");
        let mut state = crate::test_support::app_state(
            &directory,
            crate::config::ConfigFile::with_test_storage(),
        )
        .await;
        state
            .config
            .trusted_proxy_ips
            .insert("192.0.2.10".parse().unwrap());
        let router = Router::new()
            .route(
                "/",
                get(
                    |Extension(super::ClientIp(ip)): Extension<super::ClientIp>| async move {
                        ip.to_string()
                    },
                ),
            )
            .layer(axum::middleware::from_fn_with_state(
                state,
                super::proxy_boundary_middleware,
            ));
        for (peer, expected) in [
            ("192.0.2.10", "198.51.100.25"),
            ("192.0.2.11", "192.0.2.11"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/")
                        .header(header::HOST, "localhost:18473")
                        .header("x-real-ip", "198.51.100.25")
                        .extension(ConnectInfo(std::net::SocketAddr::new(
                            peer.parse().unwrap(),
                            12345,
                        )))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(
                &axum::body::to_bytes(response.into_body(), 128)
                    .await
                    .unwrap()[..],
                expected.as_bytes()
            );
        }
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
            trusted_proxy_ips: Default::default(),
            transaction_auth_key: [0x31; 32],
        }
    }
}
