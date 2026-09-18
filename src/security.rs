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
    let client_ip = validated_client_ip(&config, peer_ip, request.headers())?;
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
        return config
            .bind_address
            .is_loopback()
            .then_some(())
            .ok_or(StatusCode::FORBIDDEN);
    }
    let ip = host.parse::<IpAddr>().map_err(|_| StatusCode::FORBIDDEN)?;
    let allowed = if config.allow_lan_http {
        trusted_lan_client(ip)
    } else {
        config.bind_address.is_loopback() && ip.is_loopback()
    };
    allowed.then_some(()).ok_or(StatusCode::FORBIDDEN)
}

fn validated_client_ip(
    config: &Config,
    peer_ip: IpAddr,
    headers: &axum::http::HeaderMap,
) -> Result<IpAddr, StatusCode> {
    if config.allow_lan_http && !config.is_public_mode() {
        return trusted_lan_client(peer_ip)
            .then_some(peer_ip)
            .ok_or(StatusCode::FORBIDDEN);
    }
    if !config.is_public_mode() {
        return Ok(peer_ip);
    }
    if !config.trusted_proxy_ips.contains(&peer_ip) {
        return Err(StatusCode::FORBIDDEN);
    }
    let forwarded_for = single_header(headers, "x-forwarded-for")?;
    let forwarded_proto = single_header(headers, "x-forwarded-proto")?;
    let host = single_header(headers, header::HOST.as_str())?;
    if forwarded_proto != "https" || Some(host) != config.public_host.as_deref() {
        return Err(StatusCode::FORBIDDEN);
    }
    forwarded_for
        .parse::<IpAddr>()
        .map_err(|_| StatusCode::BAD_REQUEST)
}

fn trusted_lan_client(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unicast_link_local()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || ip.to_ipv4_mapped().is_some_and(|mapped| {
                    mapped.is_loopback() || mapped.is_private() || mapped.is_link_local()
                })
        }
    }
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
        clear_cookie, csrf_request_allowed, origin_matches, session_cookie, single_header,
        trusted_lan_client, validate_request_host, validated_client_ip,
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
    fn forwarded_headers_must_be_single_values() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        assert_eq!(
            single_header(&headers, "x-forwarded-for").unwrap(),
            "203.0.113.9"
        );
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 127.0.0.1"),
        );
        assert!(single_header(&headers, "x-forwarded-for").is_err());
    }

    #[test]
    fn public_mode_rejects_untrusted_or_insecure_proxy_requests() {
        let mut config = local_config();
        config.public_base_url = Some("https://cloud.example".into());
        config.public_host = Some("cloud.example".into());
        config
            .trusted_proxy_ips
            .insert("127.0.0.1".parse().unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("cloud.example"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        assert_eq!(
            validated_client_ip(&config, "127.0.0.1".parse().unwrap(), &headers).unwrap(),
            "203.0.113.9".parse::<std::net::IpAddr>().unwrap()
        );
        assert!(validated_client_ip(&config, "192.0.2.10".parse().unwrap(), &headers).is_err());
        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(validated_client_ip(&config, "127.0.0.1".parse().unwrap(), &headers).is_err());
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(header::HOST, HeaderValue::from_static("evil.example"));
        assert!(validated_client_ip(&config, "127.0.0.1".parse().unwrap(), &headers).is_err());
    }

    #[test]
    fn direct_lan_mode_accepts_only_non_public_peers() {
        assert!(trusted_lan_client("127.0.0.1".parse().unwrap()));
        assert!(trusted_lan_client("192.168.2.86".parse().unwrap()));
        assert!(trusted_lan_client("10.0.0.8".parse().unwrap()));
        assert!(trusted_lan_client("169.254.10.2".parse().unwrap()));
        assert!(trusted_lan_client("fd00::8".parse().unwrap()));
        assert!(!trusted_lan_client("203.0.113.8".parse().unwrap()));
        assert!(!trusted_lan_client("2001:db8::8".parse().unwrap()));

        let mut config = local_config();
        config.bind_address = "0.0.0.0".parse().unwrap();
        config.allow_lan_http = true;
        let mut spoofed_headers = HeaderMap::new();
        spoofed_headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.99"));
        assert_eq!(
            validated_client_ip(&config, "192.168.2.10".parse().unwrap(), &spoofed_headers)
                .unwrap(),
            "192.168.2.10".parse::<std::net::IpAddr>().unwrap()
        );
        assert!(
            validated_client_ip(&config, "203.0.113.8".parse().unwrap(), &HeaderMap::new())
                .is_err()
        );
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
            allow_lan_http: false,
            public_base_url: None,
            public_host: None,
            trusted_proxy_ips: Default::default(),
            allowed_hosts: Default::default(),
            s3_allowed_endpoints: Default::default(),
            transaction_auth_key: [0x31; 32],
        }
    }
}
