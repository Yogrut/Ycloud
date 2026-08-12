use axum::{
    extract::{ConnectInfo, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, Semaphore};
use uuid::Uuid;

pub use crate::state::AppState;
use crate::{
    config,
    error::{AppError, AppResult},
    security::{clear_cookie, session_cookie},
};

// ── Rate limiter ──────────────────────────────────────────────────

/// Simple fixed-window rate limiter (per IP).
pub struct RateLimiter {
    window: std::time::Duration,
    max_requests: u32,
    entries: RwLock<HashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            window: std::time::Duration::from_secs(window_secs),
            max_requests,
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Returns `true` if the request is allowed.
    pub async fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.entries.write().await;
        // [稳定 + 性能] Bound attacker-controlled IP cardinality without
        // running a full cleanup scan on every request.
        if map.len() >= 10_000 {
            map.retain(|_, (_, start)| now.duration_since(*start) < self.window);
            if map.len() >= 10_000 && !map.contains_key(key) {
                return false;
            }
        }
        if let Some((count, start)) = map.get_mut(key) {
            if now.duration_since(*start) < self.window {
                if *count >= self.max_requests {
                    return false;
                }
                *count += 1;
            } else {
                *start = now;
                *count = 1;
            }
        } else {
            map.insert(key.to_string(), (1, now));
        }
        true
    }
}

/// Rate-limit middleware: 5 req / 60 s per IP for auth endpoints.
pub async fn rate_limit_middleware(
    axum::extract::State(limiter): axum::extract::State<Arc<RateLimiter>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let ip = addr.ip().to_string();
    if limiter.check(&ip).await {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::from_u16(429).unwrap_or(StatusCode::TOO_MANY_REQUESTS))
    }
}

/// Session lifetime: 24 hours for admin sessions, 7 days for share/gate access.
const SESSION_TTL: chrono::Duration = chrono::Duration::hours(24);
const ACCESS_TOKEN_TTL: chrono::Duration = chrono::Duration::days(7);

#[derive(Clone, Debug)]
pub struct Session {
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Default)]
pub struct SessionStore {
    sessions: RwLock<HashMap<String, Session>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn create(&self) -> String {
        let token = Uuid::new_v4().to_string();
        self.sessions.write().await.insert(
            token.clone(),
            Session {
                created_at: chrono::Utc::now(),
            },
        );
        token
    }
    pub async fn validate(&self, token: &str) -> bool {
        let sessions = self.sessions.read().await;
        match sessions.get(token) {
            Some(s) => {
                if chrono::Utc::now() - s.created_at > SESSION_TTL {
                    drop(sessions);
                    self.sessions.write().await.remove(token);
                    false
                } else {
                    true
                }
            }
            None => false,
        }
    }
    pub async fn remove(&self, token: &str) {
        self.sessions.write().await.remove(token);
    }
    pub async fn clear(&self) {
        self.sessions.write().await.clear();
    }
    /// Remove all expired sessions; call periodically from a background task.
    pub async fn cleanup(&self) {
        let cutoff = chrono::Utc::now() - SESSION_TTL;
        self.sessions
            .write()
            .await
            .retain(|_, s| s.created_at > cutoff);
    }
}
pub type SharedSessionStore = Arc<SessionStore>;

#[derive(Clone, Debug)]
pub struct AccessGrant {
    pub scope: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Default)]
pub struct AccessTokenStore {
    accesses: RwLock<HashMap<String, AccessGrant>>,
}

impl AccessTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn create(&self, scope: String) -> String {
        let token = Uuid::new_v4().to_string();
        self.accesses.write().await.insert(
            token.clone(),
            AccessGrant {
                scope,
                created_at: chrono::Utc::now(),
            },
        );
        token
    }
    pub async fn get_scope(&self, token: &str) -> Option<String> {
        let accesses = self.accesses.read().await;
        match accesses.get(token) {
            Some(a) => {
                if chrono::Utc::now() - a.created_at > ACCESS_TOKEN_TTL {
                    drop(accesses);
                    self.accesses.write().await.remove(token);
                    None
                } else {
                    Some(a.scope.clone())
                }
            }
            None => None,
        }
    }
    pub async fn remove(&self, token: &str) {
        self.accesses.write().await.remove(token);
    }
    pub async fn remove_scope(&self, scope: &str) {
        self.accesses
            .write()
            .await
            .retain(|_, access| access.scope != scope);
    }
    /// Remove all expired accesses; call periodically from a background task.
    pub async fn cleanup(&self) {
        let cutoff = chrono::Utc::now() - ACCESS_TOKEN_TTL;
        self.accesses
            .write()
            .await
            .retain(|_, a| a.created_at > cutoff);
    }
}
pub type SharedAccessTokenStore = Arc<AccessTokenStore>;

#[derive(Clone)]
pub struct PasswordService {
    gate: Arc<Semaphore>,
}

impl PasswordService {
    pub fn new(max_parallel_operations: usize) -> Self {
        Self {
            gate: Arc::new(Semaphore::new(max_parallel_operations.max(1))),
        }
    }

    pub async fn verify(&self, hash: String, password: String) -> bool {
        let Ok(_permit) = self.gate.acquire().await else {
            return false;
        };
        tokio::task::spawn_blocking(move || config::verify_password(&hash, &password))
            .await
            .unwrap_or(false)
    }

    pub async fn hash(&self, password: String) -> AppResult<String> {
        let _permit =
            self.gate.acquire().await.map_err(|_| {
                AppError::ServiceUnavailable("Authentication is shutting down".into())
            })?;
        let hash = tokio::task::spawn_blocking(move || config::hash_password(&password))
            .await
            .map_err(|error| AppError::with_source("password hashing task failed", error))?;
        if hash.starts_with("__hash_error__") {
            Err(AppError::internal("password hashing failed"))
        } else {
            Ok(hash)
        }
    }
}

// ── Request / response types ──────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub logged_in: bool,
    pub is_admin: bool,
    pub web_password_required: bool,
}

pub async fn login_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Response {
    let (expected_username, password_hash) = {
        let config = state.config_file.read().await;
        (
            config.admin_username.clone(),
            config.admin_password_hash.clone(),
        )
    };
    let supplied_username = body.username.as_deref().unwrap_or("admin");
    let authenticated = supplied_username == expected_username
        && valid_password_length(&body.password)
        && state.passwords.verify(password_hash, body.password).await;

    if authenticated {
        let token = state.sessions.create().await;
        json_with_cookie(
            LoginResponse {
                success: true,
                message: "Authenticated".into(),
            },
            session_cookie(
                "session",
                &token,
                SESSION_TTL.num_seconds() as u64,
                state.config.secure_cookies,
            ),
        )
    } else {
        Json(LoginResponse {
            success: false,
            message: "Invalid username or password".into(),
        })
        .into_response()
    }
}

pub async fn logout_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(token) = extract_session_token(&headers) {
        state.sessions.remove(&token).await;
    }
    if let Some(token) = extract_gate_token(&headers) {
        state.gate_access.remove(&token).await;
    }
    let folder_cookies: Vec<(String, String)> = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(|cookies| {
            cookies
                .split(';')
                .filter_map(|part| {
                    let (name, value) = part.trim().split_once('=')?;
                    name.starts_with("folder_key_")
                        .then(|| (name.to_string(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    for (_, token) in &folder_cookies {
        state.folder_access.remove(token).await;
    }

    let mut response = Json(LoginResponse {
        success: true,
        message: "Logged out".into(),
    })
    .into_response();
    for name in ["session", "gate_access"] {
        if let Ok(value) =
            header::HeaderValue::from_str(&clear_cookie(name, state.config.secure_cookies))
        {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    for (name, _) in folder_cookies {
        if let Ok(value) =
            header::HeaderValue::from_str(&clear_cookie(&name, state.config.secure_cookies))
        {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

pub async fn me_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    let mut logged_in = false;
    let mut is_admin = false;

    if let Some(token) = extract_session_token(&headers) {
        if state.sessions.validate(&token).await {
            logged_in = true;
            is_admin = true;
        }
    }
    if !logged_in {
        if let Some(token) = extract_gate_token(&headers) {
            if state.gate_access.get_scope(&token).await.is_some() {
                logged_in = true;
            }
        }
    }

    Json(MeResponse {
        logged_in,
        is_admin,
        web_password_required: state
            .config_file
            .read()
            .await
            .global_web_password_hash
            .is_some(),
    })
}

pub async fn gate_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    Json(body): Json<LoginRequest>,
) -> Response {
    let password_hash = state
        .config_file
        .read()
        .await
        .global_web_password_hash
        .clone();
    let is_loopback = connect_info
        .map(|ConnectInfo(address)| address.ip().is_loopback())
        .unwrap_or(true);
    let authenticated = match password_hash {
        Some(hash) if valid_password_length(&body.password) => {
            state.passwords.verify(hash, body.password).await
        }
        Some(_) => false,
        None => is_loopback,
    };
    if !authenticated {
        return Json(LoginResponse {
            success: false,
            message: "Invalid password".into(),
        })
        .into_response();
    }

    let token = state.gate_access.create("__gate__".into()).await;
    json_with_cookie(
        LoginResponse {
            success: true,
            message: "Authenticated".into(),
        },
        session_cookie(
            "gate_access",
            &token,
            ACCESS_TOKEN_TTL.num_seconds() as u64,
            state.config.secure_cookies,
        ),
    )
}

fn valid_password_length(password: &str) -> bool {
    !password.is_empty() && password.len() <= 1_024
}

fn json_with_cookie<T: Serialize>(body: T, cookie: String) -> Response {
    let mut response = Json(body).into_response();
    if let Ok(value) = header::HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

// ── Token extraction helpers ──────────────────────────────────────

pub fn extract_gate_token(headers: &axum::http::HeaderMap) -> Option<String> {
    extract_cookie(headers, "gate_access")
}

pub fn extract_session_token(headers: &axum::http::HeaderMap) -> Option<String> {
    extract_cookie(headers, "session")
}

pub fn extract_cookie(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{name}=");
    cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix(&prefix).map(|v| v.to_string()))
}

pub fn extract_basic_auth(headers: &axum::http::HeaderMap) -> Option<(String, String)> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = auth.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let encoded = encoded.trim();
    if encoded.is_empty() || encoded.contains(char::is_whitespace) {
        return None;
    }
    let decoded = BASE64.decode(encoded).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.split_once(':')
        .map(|(u, p)| (u.to_string(), p.to_string()))
}

// ── Auth middleware ───────────────────────────────────────────────

/// Admin-only middleware — rejects browser gate tokens.
pub async fn admin_auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = request.headers();

    // Admin session
    if let Some(token) = extract_session_token(headers) {
        if state.sessions.validate(&token).await {
            return Ok(next.run(request).await);
        }
    }

    if valid_admin_basic_auth(&state, headers).await {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

// ── General auth middleware ───────────────────────────────────────

/// Browser file APIs accept only an administrator session or the web gate token.
pub async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = request.headers();

    if let Some(token) = extract_session_token(headers) {
        if state.sessions.validate(&token).await {
            return Ok(next.run(request).await);
        }
    }
    if let Some(token) = extract_gate_token(headers) {
        if state.gate_access.get_scope(&token).await.is_some() {
            return Ok(next.run(request).await);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

async fn valid_admin_basic_auth(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let Some((username, password)) = extract_basic_auth(headers) else {
        return false;
    };
    if !valid_password_length(&password) {
        return false;
    }
    let (expected_username, password_hash) = {
        let config = state.config_file.read().await;
        (
            config.admin_username.clone(),
            config.admin_password_hash.clone(),
        )
    };
    username == expected_username && state.passwords.verify(password_hash, password).await
}

#[cfg(test)]
mod tests {
    use super::extract_basic_auth;
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn basic_auth_scheme_is_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("basic eW9ncnV0OnNlY3JldA=="),
        );
        assert_eq!(
            extract_basic_auth(&headers),
            Some(("yogrut".into(), "secret".into()))
        );

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer eW9ncnV0OnNlY3JldA=="),
        );
        assert_eq!(extract_basic_auth(&headers), None);
    }
}
