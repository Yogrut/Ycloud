//! Credential snapshots used before password work and rechecked before issuing
//! a session. These values contain secrets and intentionally do not implement Debug.
use crate::{
    config::ConfigFile,
    login_security::{LoginEntry, LoginPolicy},
};

use super::{AdminSecondFactorProof, SessionPrincipal};

#[derive(Clone, Copy)]
pub(super) enum AccountLoginKind {
    Administrator,
    User,
}

impl AccountLoginKind {
    pub(super) fn entry(self) -> LoginEntry {
        match self {
            Self::Administrator => LoginEntry::Admin,
            Self::User => LoginEntry::Account,
        }
    }
}

pub(super) struct SecondFactorSnapshot {
    pub secret: String,
    pub recovery_hashes: Vec<String>,
}

pub(super) struct AccountLoginSnapshot {
    username: String,
    pub password_hash: String,
    pub principal: Option<SessionPrincipal>,
    pub second_factor: Option<SecondFactorSnapshot>,
    pub policy: LoginPolicy,
}

impl AccountLoginSnapshot {
    pub(super) fn capture(config: &ConfigFile, username: &str, kind: AccountLoginKind) -> Self {
        match kind {
            AccountLoginKind::Administrator => Self {
                username: config.admin_username.clone(),
                password_hash: config.admin_password_hash.clone(),
                principal: (username == config.admin_username)
                    .then_some(SessionPrincipal::Administrator),
                second_factor: config.admin_totp_secret.as_ref().map(|secret| {
                    SecondFactorSnapshot {
                        secret: secret.clone(),
                        recovery_hashes: config.admin_recovery_code_hashes.clone(),
                    }
                }),
                policy: LoginPolicy {
                    maximum_failures: config.admin_login_failures,
                    block_seconds: config.admin_login_block_seconds as i64,
                },
            },
            AccountLoginKind::User => {
                let account = config
                    .user_accounts
                    .iter()
                    .find(|account| account.username == username);
                Self {
                    username: username.to_owned(),
                    // Unknown names still perform password work before rejection.
                    password_hash: account
                        .map(|account| account.password_hash.clone())
                        .unwrap_or_else(|| config.admin_password_hash.clone()),
                    principal: account
                        .filter(|account| account.enabled)
                        .map(|account| SessionPrincipal::User(account.id.clone())),
                    second_factor: None,
                    policy: LoginEntry::Account.fixed_policy(),
                }
            }
        }
    }

    /// The caller holds auth_transitions until session issuance. Checking only
    /// the password result would allow a credential changed during verification
    /// to create a session after its previous sessions had been revoked.
    pub(super) fn is_current(
        &self,
        config: &ConfigFile,
        proof: Option<&AdminSecondFactorProof>,
    ) -> bool {
        match &self.principal {
            Some(SessionPrincipal::Administrator) => {
                config.admin_username == self.username
                    && config.admin_password_hash == self.password_hash
                    && config.admin_totp_secret.as_deref()
                        == self
                            .second_factor
                            .as_ref()
                            .map(|factor| factor.secret.as_str())
                    && match proof {
                        Some(AdminSecondFactorProof::RecoveryHash(hash)) => {
                            config.admin_recovery_code_hashes.contains(hash)
                        }
                        _ => true,
                    }
            }
            Some(SessionPrincipal::User(id)) => config.user_accounts.iter().any(|account| {
                account.id == *id
                    && account.enabled
                    && account.username == self.username
                    && account.password_hash == self.password_hash
            }),
            None => false,
        }
    }
}

pub(super) struct GateLoginSnapshot {
    pub password_hash: Option<String>,
    pub policy: LoginPolicy,
}

impl GateLoginSnapshot {
    pub(super) fn capture(config: &ConfigFile) -> Self {
        Self {
            password_hash: config.global_web_password_hash.clone(),
            policy: LoginPolicy {
                maximum_failures: config.web_login_failures,
                block_seconds: config.web_login_block_seconds as i64,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserAccount;

    fn account_config() -> ConfigFile {
        ConfigFile {
            admin_password_hash: "admin-hash".into(),
            user_accounts: vec![UserAccount {
                id: "user-id".into(),
                username: "reader".into(),
                password_hash: "reader-hash".into(),
                enabled: true,
                permissions: vec![],
            }],
            ..ConfigFile::default()
        }
    }

    #[test]
    fn account_snapshot_rejects_wrong_role_unknown_names_and_disabled_users() {
        let mut config = account_config();
        for (name, kind) in [
            ("reader", AccountLoginKind::Administrator),
            ("admin", AccountLoginKind::User),
            ("unknown", AccountLoginKind::User),
        ] {
            let snapshot = AccountLoginSnapshot::capture(&config, name, kind);
            assert!(snapshot.principal.is_none());
            assert!(!snapshot.is_current(&config, None));
        }
        config.user_accounts[0].enabled = false;
        let disabled = AccountLoginSnapshot::capture(&config, "reader", AccountLoginKind::User);
        assert!(disabled.principal.is_none());
        assert_eq!(disabled.password_hash, "reader-hash");
        let unknown = AccountLoginSnapshot::capture(&config, "unknown", AccountLoginKind::User);
        assert_eq!(unknown.password_hash, "admin-hash");
    }

    #[test]
    fn user_snapshot_rejects_credentials_changed_during_verification() {
        let config = account_config();
        let snapshot = AccountLoginSnapshot::capture(&config, "reader", AccountLoginKind::User);
        assert!(snapshot.is_current(&config, None));
        for field in ["id", "username", "password", "enabled", "deleted"] {
            let mut changed = config.clone();
            match field {
                "id" => changed.user_accounts[0].id = "replacement-id".into(),
                "username" => changed.user_accounts[0].username = "renamed".into(),
                "password" => changed.user_accounts[0].password_hash = "replacement-hash".into(),
                "enabled" => changed.user_accounts[0].enabled = false,
                "deleted" => changed.user_accounts.clear(),
                _ => unreachable!(),
            }
            assert!(
                !snapshot.is_current(&changed, None),
                "accepted change: {field}"
            );
        }
    }

    #[test]
    fn administrator_snapshot_requires_current_credentials_and_unused_recovery_proof() {
        let config = ConfigFile {
            admin_totp_secret: Some("original-secret".into()),
            admin_recovery_code_hashes: vec!["used-recovery".into(), "other-recovery".into()],
            ..account_config()
        };
        let snapshot =
            AccountLoginSnapshot::capture(&config, "admin", AccountLoginKind::Administrator);
        let proof = AdminSecondFactorProof::RecoveryHash("used-recovery".into());
        assert!(snapshot.is_current(&config, Some(&proof)));
        let mut changed = config.clone();
        changed
            .admin_recovery_code_hashes
            .retain(|hash| hash != "other-recovery");
        assert!(snapshot.is_current(&changed, Some(&proof)));
        changed.admin_recovery_code_hashes.clear();
        assert!(!snapshot.is_current(&changed, Some(&proof)));
        for field in ["username", "password", "totp", "totp-disabled"] {
            let mut changed = config.clone();
            match field {
                "username" => changed.admin_username = "renamed".into(),
                "password" => changed.admin_password_hash = "replacement-hash".into(),
                "totp" => changed.admin_totp_secret = Some("replacement-secret".into()),
                "totp-disabled" => changed.admin_totp_secret = None,
                _ => unreachable!(),
            }
            assert!(
                !snapshot.is_current(&changed, Some(&proof)),
                "accepted change: {field}"
            );
        }
    }
}
