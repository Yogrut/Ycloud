use axum::{
    extract::{Extension, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

mod login;
mod password;
mod session;

use login::{AccountLoginKind, AccountLoginSnapshot, GateLoginSnapshot};
pub use password::PasswordService;
pub(crate) use password::{
    valid_password_length, verify_admin_second_factor_proof, AdminSecondFactorProof,
};
pub use session::{
    AccessGrant, AccessTokenStore, RequestSubject, Session, SessionPrincipal, SessionStore,
    SharedAccessTokenStore, SharedSessionStore,
};
use session::{ACCESS_TOKEN_TTL, SESSION_TTL};

pub use crate::state::AppState;
use crate::{
    error::AppError,
    login_security::LoginEntry,
    security::ClientIp,
    security::{clear_cookie, session_cookie},
};

// ── Rate limiter ──────────────────────────────────────────────────

const MAX_RATE_LIMIT_KEYS: usize = 10_000;

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
        if map.len() >= MAX_RATE_LIMIT_KEYS {
            map.retain(|_, (_, start)| now.duration_since(*start) < self.window);
            if map.len() >= MAX_RATE_LIMIT_KEYS && !map.contains_key(key) {
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

/// Apply the configured per-IP limit to browser unlock endpoints.
pub async fn rate_limit_middleware(
    axum::extract::State(limiter): axum::extract::State<Arc<RateLimiter>>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let ip = ip.to_string();
    if limiter.check(&ip).await {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

// ── Request / response types ──────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
    #[serde(default)]
    pub totp_code: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub message: String,
    pub is_admin: bool,
    pub totp_required: bool,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub logged_in: bool,
    pub is_admin: bool,
    pub username: Option<String>,
    pub web_password_required: bool,
}

pub async fn login_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    authenticate_account(state, ip, headers, body, AccountLoginKind::Administrator).await
}

pub async fn user_login_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    authenticate_account(state, ip, headers, body, AccountLoginKind::User).await
}

async fn authenticate_account(
    state: AppState,
    ip: std::net::IpAddr,
    headers: axum::http::HeaderMap,
    body: LoginRequest,
    kind: AccountLoginKind,
) -> Response {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let snapshot = {
        let config = state.config_file.read().await;
        AccountLoginSnapshot::capture(&config, body.username.as_deref().unwrap_or(""), kind)
    };
    let entry = kind.entry();
    let administrator = matches!(kind, AccountLoginKind::Administrator);
    let policy = snapshot.policy;
    // Serialize the admission check with its success/failure update. Without
    // this guard, parallel requests can all pass before the failure is recorded.
    let _attempt_guard = state.login_attempts.for_entry(entry).lock().await;
    match state.login_security.is_blocked(entry, ip).await {
        Ok(true) => return limited_login_response(policy.block_seconds),
        Ok(false) => {}
        Err(error) => return login_security_error(error),
    }
    let password_matches = valid_password_length(&body.password)
        && state
            .passwords
            .verify(snapshot.password_hash.clone(), body.password)
            .await;
    let password_valid = password_matches && snapshot.principal.is_some();
    let supplied_second_factor = body
        .totp_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if requires_second_factor_challenge(
        administrator,
        password_valid,
        snapshot
            .second_factor
            .as_ref()
            .map(|factor| factor.secret.as_str()),
        supplied_second_factor,
    ) {
        return Json(LoginResponse {
            success: false,
            message: "请输入动态验证码或恢复码".into(),
            is_admin: true,
            totp_required: true,
        })
        .into_response();
    }
    let second_factor_proof = match snapshot.second_factor.as_ref() {
        Some(factor) if password_valid => {
            verify_admin_second_factor_proof(
                &state.passwords,
                &factor.secret,
                &factor.recovery_hashes,
                supplied_second_factor.unwrap_or(""),
            )
            .await
        }
        _ => None,
    };
    let authenticated =
        password_valid && (snapshot.second_factor.is_none() || second_factor_proof.is_some());
    let Some(principal) = snapshot.principal.clone().filter(|_| authenticated) else {
        return failed_login_response(&state, entry, ip, user_agent, policy).await;
    };

    // Verification is expensive and runs outside this lock. Recheck the exact
    // credential snapshot under the same guard used by credential changes,
    // then keep the guard through proof consumption and session creation.
    let auth_guard = state.auth_transitions.lock().await;
    let snapshot_is_current = {
        let config = state.config_file.read().await;
        snapshot.is_current(&config, second_factor_proof.as_ref())
    };
    if !snapshot_is_current {
        drop(auth_guard);
        return failed_login_response(&state, entry, ip, user_agent, policy).await;
    }
    match second_factor_proof {
        Some(AdminSecondFactorProof::TotpCounter(counter)) => {
            if !state.admin_totp_replay.consume(counter).await {
                drop(auth_guard);
                return failed_login_response(&state, entry, ip, user_agent, policy).await;
            }
        }
        Some(AdminSecondFactorProof::RecoveryHash(used_hash)) => {
            if let Err(error) = state
                .update_config(move |config| {
                    let index = config
                        .admin_recovery_code_hashes
                        .iter()
                        .position(|hash| hash == &used_hash)
                        .ok_or(AppError::Forbidden)?;
                    config.admin_recovery_code_hashes.remove(index);
                    Ok(())
                })
                .await
            {
                return error.into_response();
            }
        }
        None => {}
    }
    if let Err(error) = state
        .login_security
        .record_success(entry, ip, user_agent)
        .await
    {
        return login_security_error(error);
    }
    let token = match principal {
        SessionPrincipal::Administrator => state.sessions.create().await,
        SessionPrincipal::User(id) => state.sessions.create_user(id).await,
    };
    drop(auth_guard);
    json_with_cookie(
        LoginResponse {
            success: true,
            message: "Authenticated".into(),
            is_admin: administrator,
            totp_required: false,
        },
        session_cookie(
            "session",
            &token,
            SESSION_TTL.num_seconds() as u64,
            state.config.secure_cookies,
        ),
    )
}

fn requires_second_factor_challenge(
    administrator: bool,
    password_valid: bool,
    totp_secret: Option<&str>,
    supplied_second_factor: Option<&str>,
) -> bool {
    administrator && password_valid && totp_secret.is_some() && supplied_second_factor.is_none()
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
        is_admin: false,
        totp_required: false,
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
    let principal = match extract_session_token(&headers) {
        Some(token) => state.sessions.principal(&token).await,
        None => None,
    };
    let mut response = {
        let config = state.config_file.read().await;
        let mut response = MeResponse {
            logged_in: false,
            is_admin: false,
            username: None,
            web_password_required: config.global_web_password_hash.is_some(),
        };
        match principal {
            Some(SessionPrincipal::Administrator) => {
                response.logged_in = true;
                response.is_admin = true;
                response.username = Some(config.admin_username.clone());
            }
            Some(SessionPrincipal::User(id)) => {
                response.username = config
                    .user_accounts
                    .iter()
                    .find(|account| account.id == id && account.enabled)
                    .map(|account| account.username.clone());
                response.logged_in = response.username.is_some();
            }
            None => {}
        }
        response
    };
    if !response.logged_in {
        if let Some(token) = extract_gate_token(&headers) {
            if state.gate_access.get_scope(&token).await.is_some() {
                response.logged_in = true;
            }
        }
    }

    Json(response)
}

pub async fn gate_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let snapshot = {
        let config = state.config_file.read().await;
        GateLoginSnapshot::capture(&config)
    };
    let is_loopback = client_ip
        .map(|Extension(ClientIp(address))| address.is_loopback())
        .unwrap_or(false);
    let ip = client_ip
        .map(|Extension(ClientIp(address))| address)
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let web_policy = snapshot.policy;
    let _attempt_guard = state.login_attempts.for_entry(LoginEntry::Web).lock().await;
    match state.login_security.is_blocked(LoginEntry::Web, ip).await {
        Ok(true) => return limited_login_response(web_policy.block_seconds),
        Ok(false) => {}
        Err(error) => return login_security_error(error),
    }
    let authenticated = match snapshot.password_hash.as_ref() {
        Some(hash) if valid_password_length(&body.password) => {
            state.passwords.verify(hash.clone(), body.password).await
        }
        Some(_) => false,
        None => is_loopback,
    };
    if !authenticated {
        return failed_login_response(&state, LoginEntry::Web, ip, user_agent, web_policy).await;
    }

    let auth_guard = state.auth_transitions.lock().await;
    if state.config_file.read().await.global_web_password_hash != snapshot.password_hash {
        drop(auth_guard);
        return failed_login_response(&state, LoginEntry::Web, ip, user_agent, web_policy).await;
    }
    if let Err(error) = state
        .login_security
        .record_success(LoginEntry::Web, ip, user_agent)
        .await
    {
        return login_security_error(error);
    }

    let token = state.gate_access.create("__gate__".into()).await;
    drop(auth_guard);
    json_with_cookie(
        LoginResponse {
            success: true,
            message: "Authenticated".into(),
            is_admin: false,
            totp_required: false,
        },
        session_cookie(
            "gate_access",
            &token,
            ACCESS_TOKEN_TTL.num_seconds() as u64,
            state.config.secure_cookies,
        ),
    )
}

async fn failed_login_response(
    state: &AppState,
    entry: LoginEntry,
    ip: std::net::IpAddr,
    user_agent: Option<&str>,
    policy: crate::login_security::LoginPolicy,
) -> Response {
    match state
        .login_security
        .record_failure(entry, ip, user_agent, policy)
        .await
    {
        Ok(()) => invalid_login_response(entry),
        Err(error) => login_security_error(error),
    }
}

fn invalid_login_response(entry: LoginEntry) -> Response {
    let message = match entry {
        LoginEntry::Admin => "用户名或密码错误",
        LoginEntry::Account => "用户名或密码错误",
        LoginEntry::Web => "访问密码错误",
        LoginEntry::WebDav => "WebDAV 用户名或密码错误",
    };
    Json(LoginResponse {
        success: false,
        message: message.into(),
        is_admin: false,
        totp_required: false,
    })
    .into_response()
}

fn limited_login_response(block_seconds: i64) -> Response {
    let retry_after = block_seconds.to_string();
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(LoginResponse {
            success: false,
            message: "尝试次数过多，请在限制结束后重试".into(),
            is_admin: false,
            totp_required: false,
        }),
    )
        .into_response();
    if let Ok(value) = retry_after.parse() {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn login_security_error(error: anyhow::Error) -> Response {
    tracing::error!(%error, "failed to update persistent login security state");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(LoginResponse {
            success: false,
            message: "登录安全状态暂时不可用".into(),
            is_admin: false,
            totp_required: false,
        }),
    )
        .into_response()
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
    if is_admin_authenticated(&state, request.headers()).await {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

/// Browser write APIs require an authenticated administrator or ordinary
/// account. The handler then applies the operation-specific storage grant.
/// A valid web-gate session remains read-only and is deliberately insufficient.
pub async fn write_auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if current_principal(&state, request.headers()).await.is_some() {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::FORBIDDEN)
}

pub async fn current_principal(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<SessionPrincipal> {
    let token = extract_session_token(headers)?;
    let principal = state.sessions.principal(&token).await?;
    match &principal {
        SessionPrincipal::Administrator => Some(principal),
        SessionPrincipal::User(id) => state
            .config_file
            .read()
            .await
            .user_accounts
            .iter()
            .any(|account| account.id == *id && account.enabled)
            .then_some(principal),
    }
}

pub async fn current_request_subject(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<RequestSubject> {
    if let Some(token) = extract_session_token(headers) {
        if current_principal(state, headers).await.is_some() {
            return Some(RequestSubject::Session(token));
        }
    }
    if let Some(token) = extract_gate_token(headers) {
        if state.gate_access.get_scope(&token).await.is_some() {
            return Some(RequestSubject::Gate(token));
        }
    }
    None
}

pub async fn is_admin_authenticated(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    current_principal(state, headers).await == Some(SessionPrincipal::Administrator)
}

// ── General auth middleware ───────────────────────────────────────

/// Browser file APIs accept an administrator session, an enabled ordinary
/// account session, or the read-only web gate token.
pub async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = request.headers();

    if current_principal(&state, headers).await.is_some() {
        return Ok(next.run(request).await);
    }
    if let Some(token) = extract_gate_token(headers) {
        if state.gate_access.get_scope(&token).await.is_some() {
            return Ok(next.run(request).await);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

#[cfg(test)]
mod tests {
    use super::{
        authenticate_account, extract_basic_auth, invalid_login_response, limited_login_response,
        requires_second_factor_challenge, AccountLoginKind, LoginRequest,
    };
    use crate::config::{hash_password, ConfigFile, UserAccount};
    use crate::login_security::LoginEntry;
    use crate::test_support::{app_state, TestDirectory};
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};

    #[tokio::test]
    async fn pending_login_cannot_reissue_a_session_after_password_change() {
        let directory = TestDirectory::new("login-credential-change");
        let config = ConfigFile {
            user_accounts: vec![UserAccount {
                id: "reader-id".into(),
                username: "reader".into(),
                password_hash: hash_password("original-password"),
                enabled: true,
                permissions: vec![],
            }],
            ..ConfigFile::default()
        };
        let state = app_state(&directory, config).await;
        let previous_session = state.sessions.create_user("reader-id".into()).await;
        let auth_guard = state.auth_transitions.lock().await;
        let login_state = state.clone();
        let pending_login = tokio::spawn(async move {
            authenticate_account(
                login_state,
                [127, 0, 0, 1].into(),
                HeaderMap::new(),
                LoginRequest {
                    username: Some("reader".into()),
                    password: "original-password".into(),
                    totp_code: None,
                },
                AccountLoginKind::User,
            )
            .await
        });
        // Admission follows snapshot capture. Hold the credential transition
        // until this request has captured the original password, without sleeps.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if state
                    .login_attempts
                    .for_entry(LoginEntry::Account)
                    .try_lock()
                    .is_err()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("login should capture its credentials before the transition");
        let replacement_hash = hash_password("replacement-password");
        state
            .update_config(move |config| {
                config.user_accounts[0].password_hash = replacement_hash;
                Ok(())
            })
            .await
            .unwrap();
        state.sessions.revoke_user("reader-id").await;
        drop(auth_guard);

        let response = tokio::time::timeout(std::time::Duration::from_secs(10), pending_login)
            .await
            .expect("pending login should finish after the transition")
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["success"], false);
        assert!(state.sessions.principal(&previous_session).await.is_none());
    }

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

    #[test]
    fn failed_and_limited_logins_have_distinct_http_states() {
        assert_eq!(
            invalid_login_response(LoginEntry::Admin).status(),
            StatusCode::OK
        );

        let limited = limited_login_response(3600);
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.headers().get(header::RETRY_AFTER).unwrap(), "3600");
    }

    #[test]
    fn second_factor_is_requested_only_after_a_valid_admin_password() {
        assert!(!requires_second_factor_challenge(true, true, None, None));
        assert!(!requires_second_factor_challenge(
            true,
            false,
            Some("secret"),
            None
        ));
        assert!(!requires_second_factor_challenge(
            false,
            true,
            Some("secret"),
            None
        ));
        assert!(requires_second_factor_challenge(
            true,
            true,
            Some("secret"),
            None
        ));
        assert!(!requires_second_factor_challenge(
            true,
            true,
            Some("secret"),
            Some("123456")
        ));
    }
}
