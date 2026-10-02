//! Configuration credential encoding, separate from file publication.
//! Ciphertext is bound to its storage identity, provider and field.
use std::path::Path;

use anyhow::Context;

use super::secret_store::SecretStore;

const LEGACY_ENCRYPTED_CREDENTIAL_PREFIX: &str = "enc:v1:";
const ADMIN_TOTP_CONTEXT: &str = "ycloud-config:v2:administrator:totp_secret";

/// Encode all sensitive fields with one configuration key lookup.
pub(super) async fn encrypt(path: &Path, raw: &mut serde_json::Value) -> anyhow::Result<()> {
    let has_s3 = has_s3_secret(raw)?;
    let totp_secret = raw
        .get("admin_totp_secret")
        .and_then(serde_json::Value::as_str)
        .filter(|secret| !SecretStore::is_current_encoding(secret))
        .map(str::to_owned);
    if !has_s3 && totp_secret.is_none() {
        return Ok(());
    }
    let store = SecretStore::for_write(path).await?;
    if has_s3 {
        encrypt_s3_credentials_with_store(&store, raw)?;
    }
    if let Some(secret) = totp_secret {
        raw["admin_totp_secret"] =
            serde_json::Value::String(store.seal(ADMIN_TOTP_CONTEXT, &secret)?);
    }
    Ok(())
}

/// Decode before model validation; plaintext candidates never create a key.
/// Returns whether saving in the current format is required.
pub(super) async fn decrypt(path: &Path, raw: &mut serde_json::Value) -> anyhow::Result<bool> {
    let mut has_encrypted = false;
    let mut has_plaintext_secret = false;
    visit_s3_settings_mut(raw, |_, _, settings| {
        for field in ["access_key_id", "secret_access_key"] {
            let Some(encoded) = settings.get(field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let encrypted = SecretStore::is_current_encoding(encoded)
                || encoded.starts_with(LEGACY_ENCRYPTED_CREDENTIAL_PREFIX);
            has_encrypted |= encrypted;
            has_plaintext_secret |= field == "secret_access_key" && !encrypted;
        }
        Ok(())
    })?;
    let totp_secret = raw
        .get("admin_totp_secret")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let totp_encrypted = totp_secret
        .as_deref()
        .is_some_and(SecretStore::is_current_encoding);
    let totp_needs_migration = totp_secret.is_some() && !totp_encrypted;
    if !has_encrypted && !totp_encrypted {
        return Ok(has_plaintext_secret || totp_needs_migration);
    }
    let store = SecretStore::for_read(path).await?;
    let s3_changed = decrypt_s3_credentials_with_store(&store, raw, has_plaintext_secret)?;
    if let Some(encoded) = totp_secret.as_deref().filter(|_| totp_encrypted) {
        raw["admin_totp_secret"] = serde_json::Value::String(
            store
                .open(ADMIN_TOTP_CONTEXT, encoded)
                .context("Failed to decrypt administrator TOTP secret")?,
        );
    }
    Ok(s3_changed || totp_needs_migration)
}

fn encrypt_s3_credentials_with_store(
    store: &SecretStore,
    raw: &mut serde_json::Value,
) -> anyhow::Result<()> {
    visit_s3_settings_mut(raw, |storage_id, provider, settings| {
        let value = settings
            .get_mut("secret_access_key")
            .ok_or_else(|| anyhow::anyhow!("S3 secret_access_key is missing"))?;
        let plaintext = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("S3 secret_access_key must be a string"))?;
        if SecretStore::is_current_encoding(plaintext) {
            return Ok(());
        }
        let context = secret_context(storage_id, provider, "secret_access_key");
        *value = serde_json::Value::String(store.seal(&context, plaintext)?);
        Ok(())
    })
}

fn decrypt_s3_credentials_with_store(
    store: &SecretStore,
    raw: &mut serde_json::Value,
    plaintext_secret_found: bool,
) -> anyhow::Result<bool> {
    let mut migration_required = plaintext_secret_found;
    visit_s3_settings_mut(raw, |storage_id, provider, settings| {
        for field in ["access_key_id", "secret_access_key"] {
            let Some(value) = settings.get_mut(field) else {
                continue;
            };
            let encoded = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("S3 credential field {field} must be a string"))?;
            let plaintext =
                if encoded.starts_with(LEGACY_ENCRYPTED_CREDENTIAL_PREFIX) {
                    migration_required = true;
                    Some(store.open_legacy(field, encoded).with_context(|| {
                        format!("Failed to decrypt legacy S3 credential field {field}")
                    })?)
                } else if SecretStore::is_current_encoding(encoded) {
                    let context = secret_context(storage_id, provider, field);
                    Some(store.open(&context, encoded).with_context(|| {
                        format!("Failed to decrypt S3 credential field {field}")
                    })?)
                } else {
                    None
                };
            if let Some(plaintext) = plaintext {
                *value = serde_json::Value::String(plaintext);
            }
        }
        Ok(())
    })?;
    Ok(migration_required)
}

fn has_s3_secret(raw: &mut serde_json::Value) -> anyhow::Result<bool> {
    let mut found = false;
    visit_s3_settings_mut(raw, |_, _, settings| {
        found |= settings
            .get("secret_access_key")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty());
        Ok(())
    })?;
    Ok(found)
}

fn secret_context(storage_id: &str, provider: &str, field: &str) -> String {
    format!("ycloud-config:v2:{storage_id}:{provider}:{field}")
}

fn visit_s3_settings_mut(
    raw: &mut serde_json::Value,
    mut visitor: impl FnMut(
        &str,
        &str,
        &mut serde_json::Map<String, serde_json::Value>,
    ) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut visit_instance = |instance: &mut serde_json::Value| -> anyhow::Result<()> {
        let storage_id = instance
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("S3 storage instance ID is missing"))?
            .to_owned();
        let Some(backend) = instance.get_mut("backend") else {
            return Ok(());
        };
        if backend.get("type").and_then(serde_json::Value::as_str) != Some("s3") {
            return Ok(());
        }
        let settings = backend
            .get_mut("settings")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| anyhow::anyhow!("S3 backend settings are missing"))?;
        let provider = settings
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("S3 provider is missing"))?
            .to_owned();
        visitor(&storage_id, &provider, settings)
    };

    if let Some(instances) = raw
        .get_mut("storage_instances")
        .and_then(serde_json::Value::as_array_mut)
    {
        for instance in instances {
            visit_instance(instance)?;
        }
    }
    if let Some(pending) = raw.get_mut("pending_storage_instance") {
        if !pending.is_null() {
            visit_instance(pending)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    fn mixed_credentials() -> serde_json::Value {
        serde_json::json!({
            "admin_totp_secret": "JBSWY3DPEHPK3PXP",
            "storage_instances": [{
                "id": "active-s3",
                "backend": {
                    "type": "s3",
                    "settings": {
                        "provider": "minio",
                        "access_key_id": "active-id",
                        "secret_access_key": "active-secret"
                    }
                }
            }],
            "pending_storage_instance": {
                "id": "pending-s3",
                "backend": {
                    "type": "s3",
                    "settings": {
                        "provider": "minio",
                        "access_key_id": "pending-id",
                        "secret_access_key": "pending-secret"
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn active_pending_and_totp_credentials_round_trip_together() {
        let directory = TestDirectory::new("mixed-credentials");
        let path = directory.path().join("config.json");
        let original = mixed_credentials();
        let mut raw = original.clone();
        encrypt(&path, &mut raw).await.unwrap();
        let encoded = serde_json::to_string(&raw).unwrap();
        for secret in ["active-secret", "pending-secret", "JBSWY3DPEHPK3PXP"] {
            assert!(!encoded.contains(secret));
        }
        assert!(!decrypt(&path, &mut raw).await.unwrap());
        assert_eq!(raw, original);
    }

    #[tokio::test]
    async fn ciphertext_cannot_be_moved_to_another_storage_identity() {
        let directory = TestDirectory::new("credential-identity");
        let path = directory.path().join("config.json");
        let mut raw = mixed_credentials();
        encrypt(&path, &mut raw).await.unwrap();
        raw["pending_storage_instance"]["backend"]["settings"]["secret_access_key"] =
            raw["storage_instances"][0]["backend"]["settings"]["secret_access_key"].clone();
        assert!(decrypt(&path, &mut raw).await.is_err());
    }

    #[tokio::test]
    async fn decoding_plaintext_candidate_does_not_create_a_configuration_key() {
        let directory = TestDirectory::new("plaintext-candidate");
        let path = directory.path().join("config.json");
        let mut raw = mixed_credentials();
        let original = raw.clone();
        assert!(decrypt(&path, &mut raw).await.unwrap());
        assert_eq!(raw, original);
        assert!(!super::super::secret_store::managed_key_path(&path).exists());
    }

    #[tokio::test]
    async fn legacy_encrypted_fields_and_plaintext_secrets_request_resaving() {
        let directory = TestDirectory::new("legacy-credentials");
        let path = directory.path().join("config.json");
        let store = SecretStore::for_write(&path).await.unwrap();
        let original = mixed_credentials();
        let mut raw = original.clone();
        let settings = &mut raw["storage_instances"][0]["backend"]["settings"];
        for field in ["access_key_id", "secret_access_key"] {
            let encoded = store
                .seal(field, settings[field].as_str().unwrap())
                .unwrap();
            settings[field] = serde_json::Value::String(encoded.replacen("enc:v2:", "enc:v1:", 1));
        }
        assert!(decrypt(&path, &mut raw).await.unwrap());
        assert_eq!(raw, original);
        encrypt(&path, &mut raw).await.unwrap();
        assert!(!serde_json::to_string(&raw).unwrap().contains("enc:v1:"));
        assert!(!decrypt(&path, &mut raw).await.unwrap());
        assert_eq!(raw, original);
    }

    #[test]
    fn s3_credentials_are_encrypted_and_round_trip() {
        let store = SecretStore::for_test();
        let mut raw = serde_json::json!({
            "storage_instances": [{
                "id": "cos-primary",
                "backend": {
                    "type": "s3",
                    "settings": {
                        "provider": "tencent_cos",
                        "access_key_id": "access-id",
                        "secret_access_key": "secret-value"
                    }
                }
            }],
            "pending_storage_instance": null
        });

        encrypt_s3_credentials_with_store(&store, &mut raw).unwrap();
        let serialized = serde_json::to_string(&raw).unwrap();
        assert!(serialized.contains("access-id"));
        assert!(!serialized.contains("secret-value"));
        assert!(serialized.contains("enc:v2:"));

        assert!(!decrypt_s3_credentials_with_store(&store, &mut raw, false).unwrap());
        let settings = &raw["storage_instances"][0]["backend"]["settings"];
        assert_eq!(settings["access_key_id"], "access-id");
        assert_eq!(settings["secret_access_key"], "secret-value");
    }
}
