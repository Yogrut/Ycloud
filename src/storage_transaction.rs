use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::storage::is_link_or_reparse_point;

pub const SYSTEM_DIR: &str = ".ycloud-system";
const MARKER: &str = "Ycloud storage metadata v1\n";

#[derive(Clone)]
pub struct TransactionPaths {
    pub uploads: PathBuf,
    pub copies: PathBuf,
    pub backups: PathBuf,
    pub trash: PathBuf,
    pub journals: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReplaceJournal {
    id: String,
    destination: String,
}

impl TransactionPaths {
    pub async fn initialize(root: &Path) -> AppResult<Self> {
        let system = root.join(SYSTEM_DIR);
        let marker = system.join("marker");
        match fs::symlink_metadata(&system).await {
            Ok(metadata) => {
                if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
                    return Err(AppError::Conflict(
                        "Reserved .ycloud-system path must be a private local directory".into(),
                    ));
                }
                secure_directory_permissions(&system).await?;
                let marker_metadata = fs::symlink_metadata(&marker).await.map_err(|_| {
                    AppError::Conflict(
                        "Reserved .ycloud-system directory is not owned by Ycloud".into(),
                    )
                })?;
                if !marker_metadata.is_file() || is_link_or_reparse_point(&marker_metadata) {
                    return Err(AppError::Conflict(
                        "Reserved .ycloud-system marker must be a regular file".into(),
                    ));
                }
                secure_file_permissions(&marker).await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&system)
                    .await
                    .map_err(|e| AppError::with_source("failed to create storage metadata", e))?;
                secure_directory_permissions(&system).await?;
                let mut file = private_file_options()
                    .open(&marker)
                    .await
                    .map_err(|e| AppError::with_source("failed to create storage marker", e))?;
                file.write_all(MARKER.as_bytes())
                    .await
                    .map_err(|e| AppError::with_source("failed to write storage marker", e))?;
                file.sync_all()
                    .await
                    .map_err(|e| AppError::with_source("failed to flush storage marker", e))?;
            }
            Err(error) => {
                return Err(AppError::with_source(
                    "failed to inspect storage metadata",
                    error,
                ))
            }
        }
        if fs::try_exists(&marker)
            .await
            .map_err(|e| AppError::with_source("failed to inspect storage marker", e))?
        {
            let existing = fs::read_to_string(&marker).await.map_err(|_| {
                AppError::Conflict(
                    "Reserved .ycloud-system directory is not owned by Ycloud".into(),
                )
            })?;
            if existing != MARKER {
                return Err(AppError::Conflict(
                    "Reserved .ycloud-system directory has an invalid marker".into(),
                ));
            }
        } else {
            return Err(AppError::Conflict(
                "Reserved .ycloud-system directory has no ownership marker".into(),
            ));
        }
        let paths = Self {
            uploads: system.join("uploads"),
            copies: system.join("copies"),
            backups: system.join("backups"),
            trash: system.join("trash"),
            journals: system.join("transactions"),
        };
        for directory in [
            &paths.uploads,
            &paths.copies,
            &paths.backups,
            &paths.trash,
            &paths.journals,
        ] {
            ensure_private_directory(directory).await?;
        }
        paths.recover(root).await?;
        Ok(paths)
    }

    async fn recover(&self, root: &Path) -> AppResult<()> {
        let mut entries = fs::read_dir(&self.journals)
            .await
            .map_err(|e| AppError::with_source("failed to read transaction journals", e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| AppError::with_source("failed to read transaction journal", e))?
        {
            if entry.path().extension().and_then(|value| value.to_str()) == Some("tmp") {
                remove_any(&entry.path()).await?;
                continue;
            }
            let bytes = fs::read(entry.path())
                .await
                .map_err(|e| AppError::with_source("failed to read transaction journal", e))?;
            let journal: ReplaceJournal = serde_json::from_slice(&bytes)
                .map_err(|e| AppError::with_source("invalid transaction journal", e))?;
            let relative =
                crate::storage::StorageService::normalize_relative(&journal.destination)?;
            let destination = root.join(&relative);
            let backup = self.backups.join(&journal.id);
            let upload = self.uploads.join(&journal.id);
            let destination_exists = fs::try_exists(&destination).await.unwrap_or(false);
            let backup_exists = fs::try_exists(&backup).await.unwrap_or(false);
            if !destination_exists && backup_exists {
                fs::rename(&backup, &destination).await.map_err(|e| {
                    AppError::with_source("failed to restore interrupted replacement", e)
                })?;
            } else if destination_exists && backup_exists {
                remove_any(&backup).await?;
            }
            let _ = remove_any(&upload).await;
            fs::remove_file(entry.path())
                .await
                .map_err(|e| AppError::with_source("failed to clear transaction journal", e))?;
        }
        cleanup_directory(&self.copies).await?;
        cleanup_directory(&self.uploads).await?;
        cleanup_directory(&self.trash).await?;
        Ok(())
    }

    pub fn upload_path(&self, id: &str) -> PathBuf {
        self.uploads.join(id)
    }

    pub fn copy_path(&self, id: &str) -> PathBuf {
        self.copies.join(id)
    }

    pub async fn commit_file(
        &self,
        relative: &str,
        temporary: &Path,
        destination: &Path,
    ) -> AppResult<u64> {
        let metadata = match fs::symlink_metadata(destination).await {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(AppError::with_source(
                    "failed to inspect destination",
                    error,
                ))
            }
        };
        if let Some(metadata) = metadata.as_ref() {
            if !metadata.file_type().is_file() {
                return Err(AppError::Conflict(
                    "Only a regular file can be replaced by an upload".into(),
                ));
            }
        }
        let id = temporary
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| AppError::internal("invalid transaction identifier"))?;
        let journal_path = self.journals.join(format!("{id}.json"));
        let journal = ReplaceJournal {
            id: id.to_string(),
            destination: relative.to_string(),
        };
        write_json_atomic(&journal_path, &journal).await?;
        let backup = self.backups.join(id);
        if metadata.is_some() {
            fs::rename(destination, &backup)
                .await
                .map_err(|e| AppError::with_source("failed to preserve replaced file", e))?;
        }
        if let Err(error) = fs::rename(temporary, destination).await {
            if fs::try_exists(&backup).await.unwrap_or(false) {
                let _ = fs::rename(&backup, destination).await;
            }
            return Err(AppError::with_source(
                "failed to commit uploaded file",
                error,
            ));
        }
        sync_parent(destination).await?;
        if fs::try_exists(&backup).await.unwrap_or(false) {
            remove_any(&backup).await?;
        }
        fs::remove_file(&journal_path)
            .await
            .map_err(|e| AppError::with_source("failed to finalize replacement journal", e))?;
        sync_parent(&journal_path).await?;
        Ok(metadata.as_ref().map_or(0, std::fs::Metadata::len))
    }

    pub async fn stage_delete(&self, source: &Path) -> AppResult<PathBuf> {
        let target = self.trash.join(Uuid::new_v4().to_string());
        fs::rename(source, &target)
            .await
            .map_err(|e| AppError::with_source("failed to stage deletion", e))?;
        sync_parent(source).await?;
        Ok(target)
    }
}

async fn write_json_atomic(path: &Path, value: &impl Serialize) -> AppResult<()> {
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value)
        .map_err(|e| AppError::with_source("failed to encode transaction journal", e))?;
    let mut file = private_file_options()
        .open(&temporary)
        .await
        .map_err(|e| AppError::with_source("failed to create transaction journal", e))?;
    file.write_all(&bytes)
        .await
        .map_err(|e| AppError::with_source("failed to write transaction journal", e))?;
    file.sync_all()
        .await
        .map_err(|e| AppError::with_source("failed to flush transaction journal", e))?;
    drop(file);
    fs::rename(&temporary, path)
        .await
        .map_err(|e| AppError::with_source("failed to publish transaction journal", e))?;
    sync_parent(path).await
}

fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

async fn ensure_private_directory(path: &Path) -> AppResult<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
                return Err(AppError::Conflict(
                    "Transaction path must be a private local directory".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).await.map_err(|error| {
                AppError::with_source("failed to create transaction directory", error)
            })?;
            let metadata = fs::symlink_metadata(path).await.map_err(|error| {
                AppError::with_source("failed to inspect transaction directory", error)
            })?;
            if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
                return Err(AppError::Conflict(
                    "Transaction path must be a private local directory".into(),
                ));
            }
        }
        Err(error) => {
            return Err(AppError::with_source(
                "failed to inspect transaction directory",
                error,
            ))
        }
    }
    secure_directory_permissions(path).await
}

#[cfg(unix)]
async fn secure_directory_permissions(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|error| {
            AppError::with_source(
                "failed to restrict transaction directory permissions",
                error,
            )
        })
}

#[cfg(not(unix))]
async fn secure_directory_permissions(_path: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
async fn secure_file_permissions(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            AppError::with_source("failed to restrict transaction file permissions", error)
        })
}

#[cfg(not(unix))]
async fn secure_file_permissions(_path: &Path) -> AppResult<()> {
    Ok(())
}

pub async fn remove_any(path: &Path) -> AppResult<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path).await,
        Ok(_) => fs::remove_file(path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(AppError::with_source(
                "failed to inspect internal path",
                error,
            ))
        }
    }
    .map_err(|e| AppError::with_source("failed to remove internal path", e))
}

async fn cleanup_directory(path: &Path) -> AppResult<()> {
    let mut entries = fs::read_dir(path)
        .await
        .map_err(|e| AppError::with_source("failed to inspect transaction directory", e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::with_source("failed to inspect transaction entry", e))?
    {
        remove_any(&entry.path()).await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn sync_parent(path: &Path) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal("path has no parent"))?;
    let file = fs::File::open(parent)
        .await
        .map_err(|e| AppError::with_source("failed to open parent directory", e))?;
    file.sync_all()
        .await
        .map_err(|e| AppError::with_source("failed to flush parent directory", e))
}

#[cfg(not(unix))]
async fn sync_parent(_path: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ReplaceJournal, TransactionPaths, SYSTEM_DIR};

    #[tokio::test]
    async fn startup_recovers_every_persisted_replacement_stage() {
        let root = std::env::temp_dir().join(format!(
            "ycloud-transaction-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let paths = TransactionPaths::initialize(&root).await.unwrap();

        // Upload completed before a journal was published: safe to discard.
        tokio::fs::write(paths.uploads.join("temporary-only"), b"partial")
            .await
            .unwrap();

        // Old destination was moved aside, but the new file was not published.
        let restore_id = "restore-old";
        tokio::fs::write(paths.backups.join(restore_id), b"old")
            .await
            .unwrap();
        tokio::fs::write(
            paths.journals.join(format!("{restore_id}.json")),
            serde_json::to_vec(&ReplaceJournal {
                id: restore_id.into(),
                destination: "restored.txt".into(),
            })
            .unwrap(),
        )
        .await
        .unwrap();

        // New destination was published, while old backup and journal remain.
        let committed_id = "committed-new";
        tokio::fs::write(root.join("committed.txt"), b"new")
            .await
            .unwrap();
        tokio::fs::write(paths.backups.join(committed_id), b"old")
            .await
            .unwrap();
        tokio::fs::write(
            paths.journals.join(format!("{committed_id}.json")),
            serde_json::to_vec(&ReplaceJournal {
                id: committed_id.into(),
                destination: "committed.txt".into(),
            })
            .unwrap(),
        )
        .await
        .unwrap();

        // A staged delete was acknowledged but physical cleanup was interrupted.
        tokio::fs::write(paths.trash.join("staged-delete"), b"deleted")
            .await
            .unwrap();

        TransactionPaths::initialize(&root).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.join("restored.txt")).await.unwrap(),
            b"old"
        );
        assert_eq!(
            tokio::fs::read(root.join("committed.txt")).await.unwrap(),
            b"new"
        );
        for child in ["uploads", "backups", "trash", "transactions"] {
            assert!(tokio::fs::read_dir(root.join(SYSTEM_DIR).join(child))
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none());
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn unmarked_reserved_directory_is_never_taken_over() {
        let root = std::env::temp_dir().join(format!(
            "ycloud-transaction-marker-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(root.join(SYSTEM_DIR))
            .await
            .unwrap();
        assert!(TransactionPaths::initialize(&root).await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reserved_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "ycloud-transaction-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        let outside = std::env::temp_dir().join(format!(
            "ycloud-transaction-outside-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("marker"), super::MARKER)
            .await
            .unwrap();
        symlink(&outside, root.join(SYSTEM_DIR)).unwrap();

        assert!(TransactionPaths::initialize(&root).await.is_err());
        assert!(!outside.join("uploads").exists());
        tokio::fs::remove_file(root.join(SYSTEM_DIR)).await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
        tokio::fs::remove_dir_all(outside).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reserved_metadata_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "ycloud-transaction-permissions-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        TransactionPaths::initialize(&root).await.unwrap();
        let system = root.join(SYSTEM_DIR);
        let directory_mode = tokio::fs::metadata(&system)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let marker_mode = tokio::fs::metadata(system.join("marker"))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
        assert_eq!(marker_mode, 0o600);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
