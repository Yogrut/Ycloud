use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use uuid::Uuid;

use super::commit::{self, config_backup_path, ConfigCommit};
use super::credentials;
use super::migration::migrate_config;
use super::secret_store::SecretStore;
use super::{
    default_admin_username, hash_password, ConfigFile, InitialCredentials, INITIAL_CREDENTIALS_FILE,
};

pub async fn load_config(path: &Path) -> anyhow::Result<ConfigFile> {
    load_config_inner(path, None).await
}

pub async fn load_config_for_runtime(runtime: &super::Config) -> anyhow::Result<ConfigFile> {
    load_config_inner(&runtime.config_path, Some(runtime)).await
}

async fn load_config_inner(
    path: &Path,
    runtime: Option<&super::Config>,
) -> anyhow::Result<ConfigFile> {
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
        let mut config = ConfigFile::with_credentials(
            credentials.admin_username.clone(),
            hash_password(&credentials.admin_password),
            Some(hash_password(&credentials.web_access_password)),
        );
        if let Some(runtime) = runtime {
            config.max_upload_bytes = config.max_upload_bytes.min(runtime.max_upload_bytes);
            config.max_upload_batch_bytes = config
                .max_upload_batch_bytes
                .min(runtime.max_upload_batch_bytes);
            config.max_upload_batch_entries = config
                .max_upload_batch_entries
                .min(runtime.max_upload_batch_entries);
            config.max_archive_bytes = config.max_archive_bytes.min(runtime.max_archive_bytes);
            config.max_archive_entries =
                config.max_archive_entries.min(runtime.max_archive_entries);
        }
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
    let credentials_migrated = credentials::decrypt(path, &mut raw).await?;
    let config: ConfigFile =
        serde_json::from_value(raw).context("Failed to decode configuration candidate")?;
    config
        .validate()
        .map_err(anyhow::Error::new)
        .context("Invalid configuration candidate; original files were not replaced")?;
    Ok(ConfigCandidate {
        config,
        migrated,
        credentials_migrated,
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
    credentials::encrypt(path, &mut raw).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CONFIG_SCHEMA_VERSION;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn migration_keeps_an_encrypted_exact_recovery_copy() {
        let directory = TestDirectory::new("migration-recovery");
        let path = directory.path().join("config.json");
        let mut raw = serde_json::to_value(ConfigFile::with_test_storage()).unwrap();
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
        let config = ConfigFile::with_test_storage();
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
        let mut candidate = serde_json::to_value(ConfigFile::with_test_storage()).unwrap();
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
        save_config_refresh_backup(&path, &ConfigFile::with_test_storage())
            .await
            .unwrap();
        let backup = tokio::fs::read(config_backup_path(&path)).await.unwrap();
        let mut candidate = serde_json::to_value(ConfigFile::with_test_storage()).unwrap();
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
}
