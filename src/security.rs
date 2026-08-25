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
    State(config): State<Config>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip())
        .or_else(|| {
            config
                .bind_address
                .is_loopback()
                .then_some(config.bind_address)
        })
        .ok_or(StatusCode::FORBIDDEN)?;
    let client_ip = validated_client_ip(&config, peer_ip, request.headers())?;
    request.extensions_mut().insert(ClientIp(client_ip));
    Ok(next.run(request).await)
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
    if is_mutating(request.method()) && uses_cookie_auth(request.headers()) {
        let same_origin_fetch = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "same-origin");
        let origin_matches = origin_matches(&config, request.headers());
        if !same_origin_fetch && !origin_matches {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(next.run(request).await)
}

pub async fn security_headers_middleware(
    State(config): State<Config>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
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
    headers.insert(
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
        clear_cookie, origin_matches, session_cookie, single_header, trusted_lan_client,
        validated_client_ip,
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
            s3_allowed_endpoints: Default::default(),
        }
    }
}
