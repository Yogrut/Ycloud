use axum::{
    extract::Request,
    http::{
        header::{self, HeaderName, HeaderValue},
        Method, StatusCode,
    },
    middleware::Next,
    response::Response,
};

pub fn session_cookie(name: &str, value: &str, max_age: u64, secure: bool) -> String {
    let secure_attribute = if secure { "; Secure" } else { "" };
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure_attribute}"
    )
}

pub fn clear_cookie(name: &str, secure: bool) -> String {
    session_cookie(name, "", 0, secure)
}

pub async fn csrf_middleware(request: Request, next: Next) -> Result<Response, StatusCode> {
    if is_mutating(request.method()) && uses_cookie_auth(request.headers()) {
        let same_origin_fetch = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "same-origin");
        let origin_matches = origin_matches_host(request.headers());
        if !same_origin_fetch && !origin_matches {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(next.run(request).await)
}

pub async fn security_headers_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
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
             form-action 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
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

fn origin_matches_host(headers: &axum::http::HeaderMap) -> bool {
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
    use super::{clear_cookie, origin_matches_host, session_cookie};
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
        headers.insert(header::HOST, HeaderValue::from_static("cloud.local:3000"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://cloud.local:3000"),
        );
        assert!(origin_matches_host(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.cloud.local:3000"),
        );
        assert!(!origin_matches_host(&headers));
    }
}
