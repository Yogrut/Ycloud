use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use uuid::Uuid;

use super::commit::{self, ConfigCommit};
use super::migration::migrate_config;
use super::secret_store::SecretStore;
use super::{
    default_admin_username, default_storage_id, hash_password, uuid_v4, ConfigFile,
    InitialCredentials, Share, StorageBackendConfig, StorageInstanceConfig, CONFIG_SCHEMA_VERSION,
    DEFAULT_ADMIN_LOGIN_FAILURES, DEFAULT_LOGIN_BLOCK_SECONDS, DEFAULT_MAX_ARCHIVE_BYTES,
    DEFAULT_MAX_ARCHIVE_ENTRIES, DEFAULT_MAX_UPLOAD_BATCH_BYTES, DEFAULT_MAX_UPLOAD_BATCH_ENTRIES,
    DEFAULT_MAX_UPLOAD_BYTES, DEFAULT_SECURITY_LOG_MAX_ENTRIES,
    DEFAULT_SECURITY_LOG_RETENTION_DAYS, DEFAULT_WEB_LOGIN_FAILURES, INITIAL_CREDENTIALS_FILE,
};

pub async fn load_config(path: &Path) -> anyhow::Result<ConfigFile> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create configuration directory")?;
        secure_directory_permissions(parent).await?;
    }
    let backup_path = config_backup_path(path);
    if tokio::fs::try_exists(path).await? || tokio::fs::try_exists(&backup_path).await? {
        let (content, recovering) = match tokio::fs::read_to_string(path).await {
            Ok(content) if serde_json::from_str::<serde_json::Value>(&content).is_ok() => {
                (content, false)
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error)
                    .context("Failed to read primary configuration; refusing automatic rollback");
            }
            Ok(_) | Err(_) => {
                let backup = tokio::fs::read_to_string(&backup_path)
                    .await
                    .context("Primary config is unavailable and backup recovery failed")?;
                (backup, true)
            }
        };
        // Primary semantic/decryption failures never silently downgrade to an
        // older security policy. A fallback is fully validated before writing.
        let candidate = decode_candidate(path, &content).await?;
        let config = candidate.config;
        if candidate.migrated {
            preserve_migration_backup(path, &content).await?;
        }
        if recovering || candidate.migrated || candidate.credentials_migrated {
            persist_candidate(path, &config, !recovering, candidate.credentials_migrated)
                .await
                .context("Failed to publish validated configuration")?;
        }
        if recovering {
            tracing::warn!("restored fully validated configuration backup");
        }
        warn_if_initial_credentials_remain(path).await?;
        Ok(config)
    } else {
        let credentials_path = initial_credentials_path(path);
        let credentials = load_or_create_initial_credentials(&credentials_path).await?;
        let config = ConfigFile {
            traffic: crate::traffic::TrafficSettings::default(),
            schema_version: CONFIG_SCHEMA_VERSION,
            domain_binding: None,
            storage_instances: vec![StorageInstanceConfig::primary(
                StorageBackendConfig::default(),
            )],
            default_storage_id: default_storage_id(),
            pending_storage_instance: None,
            admin_username: credentials.admin_username.clone(),
            admin_password_hash: hash_password(&credentials.admin_password),
            admin_totp_secret: None,
            admin_recovery_code_hashes: Vec::new(),
            user_accounts: Vec::new(),
            global_web_password_hash: Some(hash_password(&credentials.web_access_password)),
            folder_locks: Vec::new(),
            shares: vec![Share {
                id: uuid_v4(),
                storage_id: default_storage_id(),
                name: "Default".into(),
                path: String::new(),
                username: Some("admin".into()),
                webdav_enabled: false,
                password_hash: None,
                readonly: false,
            }],
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_upload_batch_bytes: DEFAULT_MAX_UPLOAD_BATCH_BYTES,
            max_upload_batch_entries: DEFAULT_MAX_UPLOAD_BATCH_ENTRIES,
            max_archive_bytes: DEFAULT_MAX_ARCHIVE_BYTES,
            max_archive_entries: DEFAULT_MAX_ARCHIVE_ENTRIES,
            admin_login_failures: DEFAULT_ADMIN_LOGIN_FAILURES,
            web_login_failures: DEFAULT_WEB_LOGIN_FAILURES,
            admin_login_block_seconds: DEFAULT_LOGIN_BLOCK_SECONDS,
            web_login_block_seconds: DEFAULT_LOGIN_BLOCK_SECONDS,
            upload_rate_bytes_per_sec: 0,
            download_rate_bytes_per_sec: 0,
            security_log_retention_days: DEFAULT_SECURITY_LOG_RETENTION_DAYS,
            security_log_max_entries: DEFAULT_SECURITY_LOG_MAX_ENTRIES,
        };
        save_config(path, &config).await?;
        tracing::warn!(
            path = %credentials_path.display(),
            "initial administrator and web access credentials were generated; read this file and change both passwords after signing in"
        );
        Ok(config)
    }
}

struct ConfigCandidate {
    config: ConfigFile,
    migrated: bool,
    credentials_migrated: bool,
}

async fn decode_candidate(path: &Path, content: &str) -> anyhow::Result<ConfigCandidate> {
    let mut raw: serde_json::Value =
        serde_json::from_str(content).context("Failed to parse configuration candidate")?;
    let migrated = migrate_config(&mut raw)?;
    let s3_changed = decrypt_s3_credentials(path, &mut raw).await?;
    let totp_changed = decrypt_admin_totp_secret(path, &mut raw).await?;
    let config: ConfigFile =
        serde_json::from_value(raw).context("Failed to decode configuration candidate")?;
    config
        .validate()
        .map_err(anyhow::Error::new)
        .context("Invalid configuration candidate; original files were not replaced")?;
    Ok(ConfigCandidate {
        config,
        migrated,
        credentials_migrated: s3_changed || totp_changed,
    })
}

const MIGRATION_BACKUP_CONTEXT: &str = "ycloud-config:pre-migration:v1";

async fn preserve_migration_backup(path: &Path, content: &str) -> anyhow::Result<()> {
    // One bounded recovery slot, sealed as a whole so even legacy plaintext
    // secrets are not copied into an extra plaintext recovery file.
    let store = SecretStore::for_write(path).await?;
    let sealed = store.seal(MIGRATION_BACKUP_CONTEXT, content)?;
    let outcome = publish_bytes(
        &path.with_extension("json.pre-migration"),
        sealed.into_bytes(),
        false,
        false,
    )
    .await?;
    if outcome.durability != super::ConfigDurability::Confirmed {
        anyhow::bail!("Migration recovery snapshot durability was not confirmed; configuration was not migrated");
    }
    Ok(())
}

/// Read-only recovery export. Never restores old security settings implicitly.
/// The original configuration key is required; callers must protect the output.
pub async fn read_migration_backup(path: &Path) -> anyhow::Result<String> {
    let sealed = tokio::fs::read_to_string(path.with_extension("json.pre-migration")).await?;
    SecretStore::for_read(path)
        .await?
        .open(MIGRATION_BACKUP_CONTEXT, &sealed)
}

async fn warn_if_initial_credentials_remain(config_path: &Path) -> anyhow::Result<()> {
    let path = initial_credentials_path(config_path);
    if tokio::fs::try_exists(&path).await? {
        tracing::warn!(
            path = %path.display(),
            "initial plaintext credentials still exist; change both passwords in the administrator page"
        );
    }
    Ok(())
}

pub(super) fn initial_credentials_path(config_path: &Path) -> PathBuf {
    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join(INITIAL_CREDENTIALS_FILE)
}

async fn load_or_create_initial_credentials(path: &Path) -> anyhow::Result<InitialCredentials> {
    if tokio::fs::try_exists(path).await? {
        let content = tokio::fs::read(path)
            .await
            .context("Failed to read initial credentials file")?;
        let credentials: InitialCredentials =
            serde_json::from_slice(&content).context("Initial credentials file is invalid")?;
        validate_initial_credentials(&credentials)?;
        secure_file_permissions(path).await?;
        return Ok(credentials);
    }

    let credentials = InitialCredentials {
        admin_username: default_admin_username(),
        admin_password: random_password(9),
        web_access_password: random_password(6),
    };
    let json = serde_json::to_vec_pretty(&credentials)?;
    let temporary = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .await
        .context("Failed to create temporary initial credentials file")?;
    let prepare_result = async {
        file.write_all(&json)
            .await
            .context("Failed to write initial credentials file")?;
        file.sync_all()
            .await
            .context("Failed to flush initial credentials file")
    }
    .await;
    drop(file);
    if let Err(error) = prepare_result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error).context("Failed to publish initial credentials file");
    }
    secure_file_permissions(path).await?;
    sync_parent_directory(path.parent().unwrap_or_else(|| Path::new("."))).await?;
    Ok(credentials)
}

fn validate_initial_credentials(credentials: &InitialCredentials) -> anyhow::Result<()> {
    if credentials.admin_username.trim().is_empty()
        || credentials.admin_username.chars().count() > 128
        || credentials.admin_password.chars().count() < 12
        || credentials.web_access_password.chars().count() < 8
        || credentials.admin_password.len() > 4_096
        || credentials.web_access_password.len() > 4_096
    {
        anyhow::bail!("Initial credentials file does not satisfy the password policy");
    }
    Ok(())
}

fn random_password(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

pub async fn remove_initial_credentials(config_path: &Path) -> anyhow::Result<bool> {
    let path = initial_credentials_path(config_path);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            sync_parent_directory(path.parent().unwrap_or_else(|| Path::new("."))).await?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to remove initial credentials file {}",
                path.display()
            )
        }),
    }
}

/// Removes the bootstrap plaintext file only after neither password in it is
/// still active. Keeping the file while one initial credential remains is
/// intentional: otherwise rotating only the browser gate password could make
/// the still-active administrator password unrecoverable.
pub async fn remove_initial_credentials_if_rotated(
    config_path: &Path,
    config: &ConfigFile,
) -> anyhow::Result<bool> {
    let path = initial_credentials_path(config_path);
    let content = match tokio::fs::read(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", path.display()))
        }
    };
    let credentials: InitialCredentials =
        serde_json::from_slice(&content).context("Initial credentials file is invalid")?;
    validate_initial_credentials(&credentials)?;

    let administrator_rotated =
        !super::verify_password(&config.admin_password_hash, &credentials.admin_password);
    let web_gate_rotated = config
        .global_web_password_hash
        .as_deref()
        .is_none_or(|hash| !super::verify_password(hash, &credentials.web_access_password));
    if administrator_rotated && web_gate_rotated {
        remove_initial_credentials(config_path).await
    } else {
        Ok(false)
    }
}

pub async fn save_config(path: &Path, config: &ConfigFile) -> anyhow::Result<ConfigCommit> {
    persist_candidate(path, config, true, false).await
}

/// Save once, then refresh the backup without creating a second logical commit.
pub async fn save_config_refresh_backup(
    path: &Path,
    config: &ConfigFile,
) -> anyhow::Result<ConfigCommit> {
    persist_candidate(path, config, true, true).await
}

async fn persist_candidate(
    path: &Path,
    config: &ConfigFile,
    rotate_previous: bool,
    refresh_backup: bool,
) -> anyhow::Result<ConfigCommit> {
    config
        .validate()
        .map_err(anyhow::Error::new)
        .context("Refusing to save invalid configuration")?;
    let mut raw = serde_json::to_value(config)?;
    encrypt_s3_credentials(path, &mut raw).await?;
    encrypt_admin_totp_secret(path, &mut raw).await?;
    publish_bytes(
        path,
        serde_json::to_vec_pretty(&raw)?,
        rotate_previous,
        refresh_backup,
    )
    .await
}

async fn publish_bytes(
    path: &Path,
    bytes: Vec<u8>,
    rotate_previous: bool,
    refresh_backup: bool,
) -> anyhow::Result<ConfigCommit> {
    let path = path.to_owned();
    // One blocking job owns every filesystem publication step. Dropping an
    // HTTP future cannot interrupt rotation halfway through a rename sequence.
    tokio::task::spawn_blocking(move || {
        commit::publish(&path, &bytes, rotate_previous, refresh_backup, |_| Ok(()))
    })
    .await
    .context("Configuration publication worker failed; commit outcome is unknown")?
}

const LEGACY_ENCRYPTED_CREDENTIAL_PREFIX: &str = "enc:v1:";
const ADMIN_TOTP_CONTEXT: &str = "ycloud-config:v2:administrator:totp_secret";

async fn encrypt_admin_totp_secret(path: &Path, raw: &mut serde_json::Value) -> anyhow::Result<()> {
    let Some(secret) = raw
        .get("admin_totp_secret")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    if SecretStore::is_current_encoding(secret) {
        return Ok(());
    }
    let store = SecretStore::for_write(path).await?;
    raw["admin_totp_secret"] = serde_json::Value::String(store.seal(ADMIN_TOTP_CONTEXT, secret)?);
    Ok(())
}

async fn decrypt_admin_totp_secret(
    path: &Path,
    raw: &mut serde_json::Value,
) -> anyhow::Result<bool> {
    let Some(encoded) = raw
        .get("admin_totp_secret")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(false);
    };
    if SecretStore::is_current_encoding(&encoded) {
        let store = SecretStore::for_read(path).await?;
        raw["admin_totp_secret"] = serde_json::Value::String(
            store
                .open(ADMIN_TOTP_CONTEXT, &encoded)
                .context("Failed to decrypt administrator TOTP secret")?,
        );
        return Ok(false);
    }
    // A plaintext value can only come from an older/manual configuration.
    // Re-saving immediately moves it into the shared encrypted secret layer.
    Ok(true)
}

async fn encrypt_s3_credentials(path: &Path, raw: &mut serde_json::Value) -> anyhow::Result<()> {
    if !has_s3_secret(raw)? {
        return Ok(());
    }
    let store = SecretStore::for_write(path).await?;
    encrypt_s3_credentials_with_store(&store, raw)
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

async fn decrypt_s3_credentials(path: &Path, raw: &mut serde_json::Value) -> anyhow::Result<bool> {
    let mut has_encrypted = false;
    let mut has_plaintext_secret = false;
    visit_s3_settings_mut(raw, |_, _, settings| {
        for field in ["access_key_id", "secret_access_key"] {
            let Some(encoded) = settings.get(field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            has_encrypted |= SecretStore::is_current_encoding(encoded)
                || encoded.starts_with(LEGACY_ENCRYPTED_CREDENTIAL_PREFIX);
            if field == "secret_access_key"
                && !SecretStore::is_current_encoding(encoded)
                && !encoded.starts_with(LEGACY_ENCRYPTED_CREDENTIAL_PREFIX)
            {
                has_plaintext_secret = true;
            }
        }
        Ok(())
    })?;
    if !has_encrypted {
        // Validation of a plaintext candidate must not create a master key.
        return Ok(has_plaintext_secret);
    }
    let store = SecretStore::for_read(path).await?;
    decrypt_s3_credentials_with_store(&store, raw, has_plaintext_secret)
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

#[cfg(unix)]
async fn sync_parent_directory(path: &Path) -> anyhow::Result<()> {
    let directory = tokio::fs::File::open(path)
        .await
        .context("Failed to open configuration directory for synchronization")?;
    directory
        .sync_all()
        .await
        .context("Failed to synchronize configuration directory")
}

#[cfg(not(unix))]
async fn sync_parent_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn secure_directory_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .context("Failed to restrict configuration directory permissions")
}

#[cfg(not(unix))]
async fn secure_directory_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn secure_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .context("Failed to restrict configuration file permissions")
}

#[cfg(not(unix))]
async fn secure_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub(super) fn config_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn migration_keeps_an_encrypted_exact_recovery_copy() {
        let directory = TestDirectory::new("migration-recovery");
        let path = directory.path().join("config.json");
        let mut raw = serde_json::to_value(ConfigFile::default()).unwrap();
        raw["schema_version"] = serde_json::json!(11);
        let original = serde_json::to_string_pretty(&raw).unwrap();
        tokio::fs::write(&path, &original).await.unwrap();
        let migrated = load_config(&path).await.unwrap();
        assert_eq!(migrated.schema_version, CONFIG_SCHEMA_VERSION);
        assert_eq!(read_migration_backup(&path).await.unwrap(), original);
        let protected = tokio::fs::read_to_string(path.with_extension("json.pre-migration"))
            .await
            .unwrap();
        assert!(protected.starts_with("enc:v2:"));
        assert!(!protected.contains("admin_password_hash"));
        let protected_before = protected.clone();
        load_config(&path).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(path.with_extension("json.pre-migration"))
                .await
                .unwrap(),
            protected_before
        );
    }

    #[tokio::test]
    async fn missing_primary_restores_only_a_fully_validated_backup() {
        let directory = TestDirectory::new("validated-recovery");
        let path = directory.path().join("config.json");
        let config = ConfigFile::default();
        save_config_refresh_backup(&path, &config).await.unwrap();
        let backup_before = tokio::fs::read(config_backup_path(&path)).await.unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        let loaded = load_config(&path).await.unwrap();
        assert_eq!(loaded.admin_username, config.admin_username);
        assert_eq!(
            tokio::fs::read(config_backup_path(&path)).await.unwrap(),
            backup_before
        );
        assert!(path.is_file());
    }

    #[tokio::test]
    async fn semantically_invalid_backup_is_not_published() {
        let directory = TestDirectory::new("recovery-validation");
        let path = directory.path().join("config.json");
        let mut candidate = serde_json::to_value(ConfigFile::default()).unwrap();
        candidate["admin_username"] = serde_json::json!("");
        let original = serde_json::to_vec(&candidate).unwrap();
        tokio::fs::write(config_backup_path(&path), &original)
            .await
            .unwrap();
        assert!(load_config(&path).await.is_err());
        assert!(!path.exists());
        assert_eq!(
            tokio::fs::read(config_backup_path(&path)).await.unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn primary_validation_failure_does_not_roll_back_security_settings() {
        let directory = TestDirectory::new("primary-validation");
        let path = directory.path().join("config.json");
        save_config_refresh_backup(&path, &ConfigFile::default())
            .await
            .unwrap();
        let backup = tokio::fs::read(config_backup_path(&path)).await.unwrap();
        let mut candidate = serde_json::to_value(ConfigFile::default()).unwrap();
        candidate["admin_username"] = serde_json::json!("");
        let original = serde_json::to_vec(&candidate).unwrap();
        tokio::fs::write(&path, &original).await.unwrap();
        assert!(load_config(&path).await.is_err());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), original);
        assert_eq!(
            tokio::fs::read(config_backup_path(&path)).await.unwrap(),
            backup
        );
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
