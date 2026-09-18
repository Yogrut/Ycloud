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
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};
use uuid::Uuid;

pub use crate::state::AppState;
use crate::{
    config,
    error::{AppError, AppResult},
    login_security::LoginEntry,
    security::ClientIp,
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

    pub async fn is_blocked(&self, key: &str) -> bool {
        let now = Instant::now();
        self.entries
            .read()
            .await
            .get(key)
            .is_some_and(|(count, start)| {
                now.duration_since(*start) < self.window && *count >= self.max_requests
            })
    }

    pub async fn record_failure(&self, key: &str) {
        let _ = self.check(key).await;
    }
}

/// Rate-limit middleware: 5 req / 60 s per IP for auth endpoints.
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
        Err(StatusCode::from_u16(429).unwrap_or(StatusCode::TOO_MANY_REQUESTS))
    }
}

/// Session lifetime: 24 hours for admin sessions, 7 days for share/gate access.
const SESSION_TTL: chrono::Duration = chrono::Duration::hours(24);
const ACCESS_TOKEN_TTL: chrono::Duration = chrono::Duration::days(7);
const MAX_SESSIONS: usize = 4_096;
const MAX_SESSIONS_PER_PRINCIPAL: usize = 16;
const MAX_ACCESS_TOKENS: usize = 4_096;
const MAX_ACCESS_TOKENS_PER_SCOPE: usize = 16;

#[derive(Clone, Debug)]
pub struct Session {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub principal: SessionPrincipal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionPrincipal {
    Administrator,
    User(String),
}

/// Exact browser credential that initiated a short-lived server-side task.
///
/// Tickets bind to the credential token rather than only the account so a
/// leaked ticket cannot be consumed from another session of the same account.
/// The value remains in memory and must never be logged or serialized.
#[derive(Clone, Eq, PartialEq)]
pub enum RequestSubject {
    Session(String),
    Gate(String),
}

pub struct SessionStore {
    sessions: RwLock<HashMap<String, Session>>,
    max_total: usize,
    max_per_principal: usize,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::with_limits(MAX_SESSIONS, MAX_SESSIONS_PER_PRINCIPAL)
    }
    fn with_limits(max_total: usize, max_per_principal: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_total: max_total.max(1),
            max_per_principal: max_per_principal.max(1),
        }
    }
    pub async fn create(&self) -> String {
        self.create_for(SessionPrincipal::Administrator).await
    }
    pub async fn create_user(&self, user_id: String) -> String {
        self.create_for(SessionPrincipal::User(user_id)).await
    }
    async fn create_for(&self, principal: SessionPrincipal) -> String {
        let token = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let mut sessions = self.sessions.write().await;
        sessions.retain(|_, session| now - session.created_at <= SESSION_TTL);
        while sessions
            .values()
            .filter(|session| session.principal == principal)
            .count()
            >= self.max_per_principal
        {
            remove_oldest_where(&mut sessions, |session| session.principal == principal);
        }
        while sessions.len() >= self.max_total {
            remove_oldest_where(&mut sessions, |_| true);
        }
        sessions.insert(
            token.clone(),
            Session {
                created_at: now,
                principal,
            },
        );
        token
    }
    pub async fn principal(&self, token: &str) -> Option<SessionPrincipal> {
        let sessions = self.sessions.read().await;
        match sessions.get(token) {
            Some(session) if chrono::Utc::now() - session.created_at <= SESSION_TTL => {
                Some(session.principal.clone())
            }
            Some(_) => {
                drop(sessions);
                self.sessions.write().await.remove(token);
                None
            }
            None => None,
        }
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
    pub async fn revoke_user(&self, user_id: &str) {
        self.sessions.write().await.retain(
            |_, session| !matches!(&session.principal, SessionPrincipal::User(id) if id == user_id),
        );
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

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}
pub type SharedSessionStore = Arc<SessionStore>;

#[derive(Clone, Debug)]
pub struct AccessGrant {
    pub scope: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct AccessTokenStore {
    accesses: RwLock<HashMap<String, AccessGrant>>,
    max_total: usize,
    max_per_scope: usize,
}

impl AccessTokenStore {
    pub fn new() -> Self {
        Self::with_limits(MAX_ACCESS_TOKENS, MAX_ACCESS_TOKENS_PER_SCOPE)
    }
    fn with_limits(max_total: usize, max_per_scope: usize) -> Self {
        Self {
            accesses: RwLock::new(HashMap::new()),
            max_total: max_total.max(1),
            max_per_scope: max_per_scope.max(1),
        }
    }
    pub async fn create(&self, scope: String) -> String {
        let token = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let mut accesses = self.accesses.write().await;
        accesses.retain(|_, access| now - access.created_at <= ACCESS_TOKEN_TTL);
        while accesses
            .values()
            .filter(|access| access.scope == scope)
            .count()
            >= self.max_per_scope
        {
            remove_oldest_where(&mut accesses, |access| access.scope == scope);
        }
        while accesses.len() >= self.max_total {
            remove_oldest_where(&mut accesses, |_| true);
        }
        accesses.insert(
            token.clone(),
            AccessGrant {
                scope,
                created_at: now,
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
    pub async fn clear(&self) {
        self.accesses.write().await.clear();
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

impl Default for AccessTokenStore {
    fn default() -> Self {
        Self::new()
    }
}
pub type SharedAccessTokenStore = Arc<AccessTokenStore>;

fn remove_oldest_where<T>(entries: &mut HashMap<String, T>, matches: impl Fn(&T) -> bool)
where
    T: CreatedAt,
{
    let oldest = entries
        .iter()
        .filter(|(_, value)| matches(value))
        .min_by_key(|(_, value)| value.created_at())
        .map(|(token, _)| token.clone());
    if let Some(token) = oldest {
        entries.remove(&token);
    }
}

trait CreatedAt {
    fn created_at(&self) -> chrono::DateTime<chrono::Utc>;
}

impl CreatedAt for Session {
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }
}

impl CreatedAt for AccessGrant {
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }
}

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

    pub async fn verify_with_timeout(
        &self,
        hash: String,
        password: String,
        wait: Duration,
    ) -> AppResult<bool> {
        let permit = tokio::time::timeout(wait, self.gate.acquire())
            .await
            .map_err(|_| AppError::ServiceUnavailable("Authentication service is busy".into()))?
            .map_err(|_| AppError::ServiceUnavailable("Authentication is shutting down".into()))?;
        let result = tokio::task::spawn_blocking(move || config::verify_password(&hash, &password))
            .await
            .map_err(|error| AppError::with_source("password verification task failed", error))?;
        drop(permit);
        Ok(result)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdminSecondFactorProof {
    TotpCounter(u64),
    RecoveryHash(String),
}

/// Verifies either administrator second-factor credential and preserves the
/// evidence needed by the caller to consume it atomically with authentication.
pub(crate) async fn verify_admin_second_factor_proof(
    passwords: &PasswordService,
    secret: &str,
    recovery_hashes: &[String],
    supplied: &str,
) -> Option<AdminSecondFactorProof> {
    if let Some(counter) = crate::totp::verify_now_counter(secret, supplied) {
        return Some(AdminSecondFactorProof::TotpCounter(counter));
    }
    let recovery = crate::totp::normalize_recovery_code(supplied);
    if recovery.len() != 10 {
        return None;
    }
    for hash in recovery_hashes {
        if passwords.verify(hash.clone(), recovery.clone()).await {
            return Some(AdminSecondFactorProof::RecoveryHash(hash.clone()));
        }
    }
    None
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
    authenticate_account(state, ip, headers, body, true).await
}

pub async fn user_login_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    authenticate_account(state, ip, headers, body, false).await
}

async fn authenticate_account(
    state: AppState,
    ip: std::net::IpAddr,
    headers: axum::http::HeaderMap,
    body: LoginRequest,
    administrator: bool,
) -> Response {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let (
        admin_policy,
        admin_username,
        admin_hash,
        admin_totp_secret,
        recovery_hashes,
        ordinary_candidate,
    ) = {
        let config = state.config_file.read().await;
        let supplied = body.username.as_deref().unwrap_or("");
        (
            crate::login_security::LoginPolicy {
                maximum_failures: config.admin_login_failures,
                block_seconds: config.admin_login_block_seconds as i64,
            },
            config.admin_username.clone(),
            config.admin_password_hash.clone(),
            config.admin_totp_secret.clone(),
            config.admin_recovery_code_hashes.clone(),
            config
                .user_accounts
                .iter()
                .find(|account| account.username == supplied)
                .map(|account| {
                    (
                        account.id.clone(),
                        account.password_hash.clone(),
                        account.enabled,
                    )
                }),
        )
    };
    let supplied_username = body.username.as_deref().unwrap_or("");
    let entry = if administrator {
        LoginEntry::Admin
    } else {
        LoginEntry::Account
    };
    let policy = if administrator {
        admin_policy
    } else {
        LoginEntry::Account.fixed_policy()
    };
    // Serialize the admission check with its success/failure update. Without
    // this guard, parallel requests can all pass `is_blocked` before any of
    // them increments the persistent failure counter.
    let _attempt_guard = state.login_attempts.lock().await;
    match state.login_security.is_blocked(entry, ip).await {
        Ok(true) => return limited_login_response(policy.block_seconds),
        Ok(false) => {}
        Err(error) => return login_security_error(error),
    }
    // Always run Argon2, including for an unknown username, so account
    // existence is not exposed by a cheap timing distinction.
    let password_hash = if administrator {
        admin_hash.clone()
    } else {
        ordinary_candidate
            .as_ref()
            .map(|(_, hash, _)| hash.clone())
            .unwrap_or_else(|| admin_hash.clone())
    };
    let password_matches = valid_password_length(&body.password)
        && state.passwords.verify(password_hash, body.password).await;
    let password_valid = password_matches
        && if administrator {
            supplied_username == admin_username
        } else {
            ordinary_candidate.is_some()
        };
    let supplied_second_factor = body
        .totp_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if requires_second_factor_challenge(
        administrator,
        password_valid,
        admin_totp_secret.as_deref(),
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
    let second_factor_proof = if administrator && password_valid {
        if let Some(secret) = admin_totp_secret.as_deref() {
            verify_admin_second_factor_proof(
                &state.passwords,
                secret,
                &recovery_hashes,
                body.totp_code.as_deref().unwrap_or("").trim(),
            )
            .await
        } else {
            None
        }
    } else {
        None
    };
    let second_factor_valid =
        !administrator || admin_totp_secret.is_none() || second_factor_proof.is_some();
    let used_totp_counter = match second_factor_proof.as_ref() {
        Some(AdminSecondFactorProof::TotpCounter(counter)) => Some(*counter),
        _ => None,
    };
    let used_recovery_hash = match second_factor_proof {
        Some(AdminSecondFactorProof::RecoveryHash(hash)) => Some(hash),
        _ => None,
    };
    let authenticated = password_valid
        && second_factor_valid
        && (administrator
            || ordinary_candidate
                .as_ref()
                .is_some_and(|(_, _, enabled)| *enabled));

    if authenticated {
        // Credential verification is intentionally expensive and happens
        // outside this lock. Before issuing a session, serialize against all
        // credential changes and prove that the verified snapshot is still
        // current. This closes the "verify old credential, then revoke, then
        // issue a new session" race.
        let auth_guard = state.auth_transitions.lock().await;
        let snapshot_is_current = {
            let config = state.config_file.read().await;
            if administrator {
                config.admin_username == admin_username
                    && config.admin_password_hash == admin_hash
                    && config.admin_totp_secret == admin_totp_secret
                    && used_recovery_hash.as_ref().is_none_or(|used_hash| {
                        config.admin_recovery_code_hashes.contains(used_hash)
                    })
            } else {
                ordinary_candidate
                    .as_ref()
                    .and_then(|(id, password_hash, _)| {
                        config
                            .user_accounts
                            .iter()
                            .find(|account| account.id == *id)
                            .map(|account| {
                                account.enabled
                                    && account.username == supplied_username
                                    && account.password_hash == *password_hash
                            })
                    })
                    .unwrap_or(false)
            }
        };
        if !snapshot_is_current {
            drop(auth_guard);
            if let Err(error) = state
                .login_security
                .record_failure(entry, ip, user_agent, policy)
                .await
            {
                return login_security_error(error);
            }
            return invalid_login_response(entry);
        }
        if let Some(counter) = used_totp_counter {
            if !state.admin_totp_replay.consume(counter).await {
                drop(auth_guard);
                if let Err(error) = state
                    .login_security
                    .record_failure(entry, ip, user_agent, policy)
                    .await
                {
                    return login_security_error(error);
                }
                return invalid_login_response(entry);
            }
        }
        if let Some(used_hash) = used_recovery_hash {
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
        if let Err(error) = state
            .login_security
            .record_success(entry, ip, user_agent)
            .await
        {
            return login_security_error(error);
        }
        let token = if administrator {
            state.sessions.create().await
        } else {
            state
                .sessions
                .create_user(
                    ordinary_candidate
                        .as_ref()
                        .map(|(id, _, _)| id.clone())
                        .unwrap_or_default(),
                )
                .await
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
    } else {
        if let Err(error) = state
            .login_security
            .record_failure(entry, ip, user_agent, policy)
            .await
        {
            return login_security_error(error);
        }
        invalid_login_response(entry)
    }
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
    let mut logged_in = false;
    let mut is_admin = false;
    let mut username = None;

    if let Some(token) = extract_session_token(&headers) {
        if let Some(principal) = state.sessions.principal(&token).await {
            logged_in = true;
            match principal {
                SessionPrincipal::Administrator => {
                    is_admin = true;
                    username = Some(state.config_file.read().await.admin_username.clone());
                }
                SessionPrincipal::User(id) => {
                    username = state
                        .config_file
                        .read()
                        .await
                        .user_accounts
                        .iter()
                        .find(|account| account.id == id && account.enabled)
                        .map(|account| account.username.clone());
                    logged_in = username.is_some();
                }
            }
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
        username,
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
    client_ip: Option<Extension<ClientIp>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let password_hash = state
        .config_file
        .read()
        .await
        .global_web_password_hash
        .clone();
    let is_loopback = client_ip
        .map(|Extension(ClientIp(address))| address.is_loopback())
        .unwrap_or(false);
    let ip = client_ip
        .map(|Extension(ClientIp(address))| address)
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let web_policy = {
        let config = state.config_file.read().await;
        crate::login_security::LoginPolicy {
            maximum_failures: config.web_login_failures,
            block_seconds: config.web_login_block_seconds as i64,
        }
    };
    let _attempt_guard = state.login_attempts.lock().await;
    match state.login_security.is_blocked(LoginEntry::Web, ip).await {
        Ok(true) => return limited_login_response(web_policy.block_seconds),
        Ok(false) => {}
        Err(error) => return login_security_error(error),
    }
    let authenticated = match password_hash.as_ref() {
        Some(hash) if valid_password_length(&body.password) => {
            state.passwords.verify(hash.clone(), body.password).await
        }
        Some(_) => false,
        None => is_loopback,
    };
    if !authenticated {
        if let Err(error) = state
            .login_security
            .record_failure(LoginEntry::Web, ip, user_agent, web_policy)
            .await
        {
            return login_security_error(error);
        }
        return invalid_login_response(LoginEntry::Web);
    }

    let auth_guard = state.auth_transitions.lock().await;
    if state.config_file.read().await.global_web_password_hash != password_hash {
        drop(auth_guard);
        if let Err(error) = state
            .login_security
            .record_failure(LoginEntry::Web, ip, user_agent, web_policy)
            .await
        {
            return login_security_error(error);
        }
        return invalid_login_response(LoginEntry::Web);
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

fn valid_password_length(password: &str) -> bool {
    let characters = password.chars().count();
    (1..=1_024).contains(&characters) && password.len() <= 4_096
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
        extract_basic_auth, invalid_login_response, limited_login_response,
        requires_second_factor_challenge, verify_admin_second_factor_proof, AccessTokenStore,
        AdminSecondFactorProof, PasswordService, SessionStore,
    };
    use crate::login_security::LoginEntry;
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};

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

    #[tokio::test]
    async fn shared_admin_second_factor_verifier_returns_consumable_evidence() {
        let passwords = PasswordService::new(1);
        let secret = crate::totp::generate_secret();
        let code = crate::totp::current_code_for_test(&secret);
        let proof = verify_admin_second_factor_proof(&passwords, &secret, &[], &code)
            .await
            .unwrap();
        let AdminSecondFactorProof::TotpCounter(counter) = proof else {
            panic!("current TOTP must produce counter evidence");
        };
        let replay = crate::totp::TotpReplayStore::default();
        assert!(replay.consume(counter).await);
        assert!(!replay.consume(counter).await);

        let recovery = crate::totp::normalize_recovery_code("ABCDE-23456");
        let hash = passwords.hash(recovery).await.unwrap();
        let proof = verify_admin_second_factor_proof(
            &passwords,
            &secret,
            std::slice::from_ref(&hash),
            "abcde-23456",
        )
        .await;
        assert_eq!(proof, Some(AdminSecondFactorProof::RecoveryHash(hash)));
    }

    #[tokio::test]
    async fn credential_change_can_revoke_all_session_classes() {
        let sessions = SessionStore::new();
        let accesses = AccessTokenStore::new();
        let session = sessions.create().await;
        let gate = accesses.create("__gate__".into()).await;
        assert!(sessions.validate(&session).await);
        assert!(accesses.get_scope(&gate).await.is_some());

        sessions.clear().await;
        accesses.clear().await;
        assert!(!sessions.validate(&session).await);
        assert!(accesses.get_scope(&gate).await.is_none());
    }

    #[tokio::test]
    async fn ordinary_account_sessions_keep_their_identity_and_can_be_revoked_selectively() {
        let sessions = SessionStore::new();
        let administrator = sessions.create().await;
        let first = sessions.create_user("first-user".into()).await;
        let second = sessions.create_user("second-user".into()).await;

        assert_eq!(
            sessions.principal(&first).await,
            Some(super::SessionPrincipal::User("first-user".into()))
        );
        sessions.revoke_user("first-user").await;

        assert!(sessions.principal(&first).await.is_none());
        assert_eq!(
            sessions.principal(&administrator).await,
            Some(super::SessionPrincipal::Administrator)
        );
        assert_eq!(
            sessions.principal(&second).await,
            Some(super::SessionPrincipal::User("second-user".into()))
        );
    }

    #[tokio::test]
    async fn folder_credential_change_revokes_only_matching_scope() {
        let accesses = AccessTokenStore::new();
        let changed = accesses.create("locked/a".into()).await;
        let other = accesses.create("locked/b".into()).await;
        accesses.remove_scope("locked/a").await;
        assert!(accesses.get_scope(&changed).await.is_none());
        assert_eq!(
            accesses.get_scope(&other).await.as_deref(),
            Some("locked/b")
        );
    }

    #[tokio::test]
    async fn sessions_are_bounded_per_principal_and_globally() {
        let sessions = SessionStore::with_limits(3, 2);
        let first = sessions.create_user("same-user".into()).await;
        let second = sessions.create_user("same-user".into()).await;
        let newest = sessions.create_user("same-user".into()).await;
        let surviving_same_user = usize::from(sessions.validate(&first).await)
            + usize::from(sessions.validate(&second).await)
            + usize::from(sessions.validate(&newest).await);
        assert_eq!(surviving_same_user, 2);
        assert!(sessions.validate(&newest).await);

        let administrator = sessions.create().await;
        let other = sessions.create_user("other-user".into()).await;
        assert!(sessions.validate(&administrator).await);
        assert!(sessions.validate(&other).await);
        assert!(sessions.sessions.read().await.len() <= 3);
    }

    #[tokio::test]
    async fn access_tokens_are_bounded_per_scope() {
        let accesses = AccessTokenStore::with_limits(3, 2);
        let first = accesses.create("same-scope".into()).await;
        let second = accesses.create("same-scope".into()).await;
        let newest = accesses.create("same-scope".into()).await;

        let first_valid = accesses.get_scope(&first).await.is_some();
        let second_valid = accesses.get_scope(&second).await.is_some();
        assert_eq!(usize::from(first_valid) + usize::from(second_valid), 1);
        assert!(accesses.get_scope(&newest).await.is_some());
        assert!(accesses.accesses.read().await.len() <= 2);
    }
}
