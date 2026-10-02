//! Bounded password work and administrator second-factor verification.
use std::{sync::Arc, time::Duration};

use tokio::sync::Semaphore;

use crate::{
    config,
    error::{AppError, AppResult},
};

const MAX_LOGIN_PASSWORD_CHARACTERS: usize = 1_024;
const MAX_LOGIN_PASSWORD_BYTES: usize = 4 * MAX_LOGIN_PASSWORD_CHARACTERS;

pub(crate) fn valid_password_length(password: &str) -> bool {
    !password.is_empty()
        && password.len() <= MAX_LOGIN_PASSWORD_BYTES
        && password.chars().count() <= MAX_LOGIN_PASSWORD_CHARACTERS
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
        let Ok(permit) = self.gate.clone().acquire_owned().await else {
            return false;
        };
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            config::verify_password(&hash, &password)
        })
        .await
        .unwrap_or(false)
    }

    pub async fn verify_with_timeout(
        &self,
        hash: String,
        password: String,
        wait: Duration,
    ) -> AppResult<bool> {
        let permit = tokio::time::timeout(wait, self.gate.clone().acquire_owned())
            .await
            .map_err(|_| AppError::ServiceUnavailable("Authentication service is busy".into()))?
            .map_err(|_| AppError::ServiceUnavailable("Authentication is shutting down".into()))?;
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            config::verify_password(&hash, &password)
        })
        .await
        .map_err(|error| AppError::with_source("password verification task failed", error))?;
        Ok(result)
    }

    pub async fn hash(&self, password: String) -> AppResult<String> {
        let permit =
            self.gate.clone().acquire_owned().await.map_err(|_| {
                AppError::ServiceUnavailable("Authentication is shutting down".into())
            })?;
        let hash = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            config::hash_password(&password)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_password_limit_accepts_unicode_without_accepting_oversized_inputs() {
        assert!(!valid_password_length(""));
        assert!(valid_password_length(&"x".repeat(1_024)));
        assert!(valid_password_length(&"🔑".repeat(1_024)));
        assert!(!valid_password_length(&"x".repeat(1_025)));
        assert!(!valid_password_length(&"🔑".repeat(1_025)));
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
}
