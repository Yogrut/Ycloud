//! Filesystem publication boundary. No fallible step after publication is
//! reported as "not saved". Callers can publish the same candidate in memory.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigDurability {
    Confirmed,
    /// The new file is visible, but directory synchronization failed.
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigCommit {
    pub durability: ConfigDurability,
    /// Publication succeeded, but replacing the backup with the current
    /// (credential-scrubbed) configuration needs another attempt.
    pub backup_refresh_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommitStage {
    BeforePublish,
    SyncPublished,
    RefreshBackup,
}

pub(super) fn publish(
    path: &Path,
    bytes: &[u8],
    rotate_previous: bool,
    refresh_backup: bool,
    mut checkpoint: impl FnMut(CommitStage) -> std::io::Result<()>,
) -> anyhow::Result<ConfigCommit> {
    let pending = PendingFile::prepare(path, bytes)?;
    let backup = super::persistence::config_backup_path(path);
    if rotate_previous {
        match fs::read(path) {
            Ok(previous) => PendingFile::prepare(&backup, &previous)?.publish(&backup)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Failed to read previous configuration"),
        }
    }
    checkpoint(CommitStage::BeforePublish)?;
    // Rename replaces the destination without first removing the main file.
    // The private permissions already belong to the prepared inode/file.
    pending.publish(path)?;
    let durability = if checkpoint(CommitStage::SyncPublished)
        .and_then(|()| sync_parent(path))
        .is_ok()
    {
        ConfigDurability::Confirmed
    } else {
        ConfigDurability::Unconfirmed
    };
    let backup_refresh_pending = refresh_backup
        && (|| -> anyhow::Result<()> {
            checkpoint(CommitStage::RefreshBackup)?;
            PendingFile::prepare(&backup, bytes)?.publish(&backup)?;
            sync_parent(&backup)?;
            Ok(())
        })()
        .is_err();
    let outcome = ConfigCommit {
        durability,
        backup_refresh_pending,
    };
    if outcome.durability == ConfigDurability::Unconfirmed || outcome.backup_refresh_pending {
        tracing::warn!(
            ?outcome,
            "configuration published; durability or backup maintenance needs attention"
        );
    }
    Ok(outcome)
}

struct PendingFile(PathBuf);

impl PendingFile {
    fn prepare(path: &Path, bytes: &[u8]) -> anyhow::Result<Self> {
        let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .context("Failed to prepare private configuration file")?;
        // Cleanup ownership starts only after create_new succeeded.
        let pending = Self(temporary);
        let result = (|| -> anyhow::Result<()> {
            file.write_all(bytes)
                .context("Failed to write prepared configuration")?;
            file.sync_all()
                .context("Failed to flush prepared configuration")?;
            Ok(())
        })();
        drop(file);
        result?;
        Ok(pending)
    }

    fn publish(self, path: &Path) -> anyhow::Result<()> {
        fs::rename(&self.0, path).context("Failed to publish prepared configuration")
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        match fs::remove_file(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, "could not remove prepared configuration file"),
        }
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    fs::File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[test]
    fn publication_keeps_previous_and_current_versions() {
        let dir = TestDirectory::new("config-commit");
        let path = dir.path().join("config.json");
        publish(&path, b"first", true, false, |_| Ok(())).unwrap();
        let result = publish(&path, b"second", true, false, |_| Ok(())).unwrap();
        assert_eq!(result.durability, ConfigDurability::Confirmed);
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::read(super::super::persistence::config_backup_path(&path)).unwrap(),
            b"first"
        );
    }

    #[test]
    fn io_failure_before_publication_preserves_main_file() {
        let dir = TestDirectory::new("config-precommit");
        let path = dir.path().join("config.json");
        publish(&path, b"first", true, false, |_| Ok(())).unwrap();
        let result = publish(&path, b"second", true, false, |stage| {
            if stage == CommitStage::BeforePublish {
                Err(std::io::Error::other("simulated I/O failure"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|ext| ext != "tmp")));
    }

    #[test]
    fn post_publication_failure_does_not_claim_the_write_was_rejected() {
        let dir = TestDirectory::new("config-postcommit");
        let path = dir.path().join("config.json");
        let outcome = publish(&path, b"current", false, true, |stage| {
            if stage != CommitStage::BeforePublish {
                Err(std::io::Error::other("simulated maintenance failure"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"current");
        assert_eq!(outcome.durability, ConfigDurability::Unconfirmed);
        assert!(outcome.backup_refresh_pending);
    }

    #[test]
    fn refreshing_backup_does_not_republish_the_main_file() {
        let dir = TestDirectory::new("config-backup-refresh");
        let path = dir.path().join("config.json");
        let mut publications = 0;
        let outcome = publish(&path, b"current", true, true, |stage| {
            if stage == CommitStage::BeforePublish {
                publications += 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(publications, 1);
        assert!(!outcome.backup_refresh_pending);
        assert_eq!(
            fs::read(super::super::persistence::config_backup_path(&path)).unwrap(),
            b"current"
        );
    }
}
