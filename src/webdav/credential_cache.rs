//! Bounded, process-local proofs of successful WebDAV password verification.
//! Authorization and login restrictions are deliberately not cached here.
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use ring::hmac;
use tokio::sync::Mutex;

use crate::config::Share;

const MAX_VERIFIED_CREDENTIALS: usize = 64;
const VERIFICATION_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct CredentialFingerprint([u8; 32]);

struct VerifiedCredential {
    fingerprint: CredentialFingerprint,
    expires_at: Instant,
}

#[derive(Clone)]
pub(crate) struct CredentialCache {
    key: Arc<hmac::Key>,
    entries: Arc<Mutex<VecDeque<VerifiedCredential>>>,
}

impl Default for CredentialCache {
    fn default() -> Self {
        let material = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Self {
            key: Arc::new(hmac::Key::new(hmac::HMAC_SHA256, material.as_bytes())),
            entries: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl CredentialCache {
    pub(super) fn fingerprint(&self, share: &Share, password: &str) -> CredentialFingerprint {
        // Include the entire current mount: credential, path, storage, enabled,
        // and permission changes cannot reuse a proof for the old configuration.
        // Only the keyed digest is retained, never the password or its plain hash.
        let payload = serde_json::to_vec(&(share, password))
            .expect("WebDAV credential inputs are serializable");
        let tag = hmac::sign(&self.key, &payload);
        let mut bytes = [0; 32];
        bytes.copy_from_slice(tag.as_ref());
        CredentialFingerprint(bytes)
    }

    pub(super) async fn contains(&self, fingerprint: CredentialFingerprint) -> bool {
        let mut entries = self.entries.lock().await;
        let now = Instant::now();
        entries.retain(|entry| entry.expires_at > now);
        entries.iter().any(|entry| entry.fingerprint == fingerprint)
    }

    /// Called only after the existing password verifier has returned success.
    pub(super) async fn remember(&self, fingerprint: CredentialFingerprint) {
        let mut entries = self.entries.lock().await;
        let now = Instant::now();
        entries.retain(|entry| entry.expires_at > now);
        if entries.iter().any(|entry| entry.fingerprint == fingerprint) {
            // Cache hits must not extend the original verification lifetime.
            return;
        }
        if entries.len() == MAX_VERIFIED_CREDENTIALS {
            entries.pop_front();
        }
        entries.push_back(VerifiedCredential {
            fingerprint,
            expires_at: now + VERIFICATION_TTL,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount() -> Share {
        Share {
            id: "mount".into(),
            storage_id: "primary".into(),
            name: "documents".into(),
            path: "docs".into(),
            username: Some("reader".into()),
            password_hash: Some("stored-hash".into()),
            webdav_enabled: true,
            readonly: false,
        }
    }

    #[tokio::test]
    async fn only_exact_current_credentials_and_mount_match() {
        let cache = CredentialCache::default();
        let original = mount();
        let fingerprint = cache.fingerprint(&original, "correct-password");
        assert!(!cache.contains(fingerprint).await);
        cache.remember(fingerprint).await;
        assert!(cache.contains(fingerprint).await);
        assert!(
            !cache
                .contains(cache.fingerprint(&original, "wrong-password"))
                .await
        );

        let mut variants = Vec::new();
        let mut changed = original.clone();
        changed.password_hash = Some("new-hash".into());
        variants.push(changed);
        let mut changed = original.clone();
        changed.username = Some("other-user".into());
        variants.push(changed);
        let mut changed = original.clone();
        changed.id = "replacement-mount".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.storage_id = "other-storage".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.path = "other-path".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.name = "other-name".into();
        variants.push(changed);
        let mut changed = original.clone();
        changed.readonly = true;
        variants.push(changed);
        let mut changed = original.clone();
        changed.webdav_enabled = false;
        variants.push(changed);
        for changed in variants {
            assert!(
                !cache
                    .contains(cache.fingerprint(&changed, "correct-password"))
                    .await
            );
        }
        // A new service lifetime cannot reuse old in-memory proofs.
        let restarted = CredentialCache::default();
        assert!(fingerprint != restarted.fingerprint(&original, "correct-password"));
        assert!(!restarted.contains(fingerprint).await);
    }

    #[tokio::test]
    async fn proofs_expire_without_sliding_and_evict_at_the_bound() {
        let cache = CredentialCache::default();
        let share = mount();
        let first = cache.fingerprint(&share, "first");
        cache.remember(first).await;
        let original_expiry = cache.entries.lock().await[0].expires_at;
        assert!(cache.contains(first).await);
        cache.remember(first).await;
        assert_eq!(cache.entries.lock().await[0].expires_at, original_expiry);
        cache.entries.lock().await[0].expires_at = Instant::now() - Duration::from_secs(1);
        assert!(!cache.contains(first).await);
        cache.remember(first).await;
        for index in 0..MAX_VERIFIED_CREDENTIALS {
            cache
                .remember(cache.fingerprint(&share, &format!("password-{index}")))
                .await;
        }
        assert_eq!(cache.entries.lock().await.len(), MAX_VERIFIED_CREDENTIALS);
        assert!(!cache.contains(first).await);
    }
}
