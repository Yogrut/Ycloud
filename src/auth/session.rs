//! Bounded account sessions and scoped browser access tokens.
use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;
use uuid::Uuid;

/// Account sessions last 24 hours; scoped gate/folder grants last 7 days.
pub(super) const SESSION_TTL: chrono::Duration = chrono::Duration::hours(24);
pub(super) const ACCESS_TOKEN_TTL: chrono::Duration = chrono::Duration::days(7);
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
        if principal == SessionPrincipal::Administrator {
            sessions.retain(|_, session| session.principal != SessionPrincipal::Administrator);
        }
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
    #[cfg(test)]
    pub async fn validate(&self, token: &str) -> bool {
        self.principal(token).await.is_some()
    }
    pub async fn remove(&self, token: &str) {
        self.sessions.write().await.remove(token);
    }
    pub async fn clear(&self) {
        self.sessions.write().await.clear();
    }
    pub async fn revoke_administrator(&self) {
        self.sessions
            .write()
            .await
            .retain(|_, session| session.principal != SessionPrincipal::Administrator);
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

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn administrator_has_one_session_and_revocation_preserves_users() {
        let sessions = SessionStore::new();
        let user = sessions.create_user("ordinary".into()).await;
        let old_admin = sessions.create().await;
        let admin = sessions.create().await;
        assert!(!sessions.validate(&old_admin).await);
        assert!(sessions.validate(&admin).await);
        assert!(sessions.validate(&user).await);
        sessions.revoke_administrator().await;
        assert!(!sessions.validate(&admin).await);
        assert!(sessions.validate(&user).await);
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
