use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

mod metadata;
mod receipts;
mod recovery;
pub use metadata::TransactionId;
use metadata::{
    resource_id, DeletionJournal, ReplaceJournal, MAX_JOURNAL_BYTES, MAX_RECOVERY_ENTRIES,
};

use crate::error::{AppError, AppResult};
use crate::storage::is_link_or_reparse_point;
#[cfg(target_os = "linux")]
use crate::storage::linux_root::LinuxRoot;

pub const SYSTEM_DIR: &str = ".ycloud-system";
const MARKER: &str = "Ycloud storage metadata v1\n";
pub(crate) const DELETION_NODES_PER_PASS: usize = 256;

/// Only the current owner may remove an upload. Recovery owns it before the
/// first journal I/O starts, including when that I/O outlives its waiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UploadOwnership {
    Writer,
    Recovery,
}

/// Synchronous notifications at the mutation boundary; publication accounting
/// must not wait until fallible directory sync or backup cleanup has finished.
pub(crate) trait ReplacementObserver {
    fn check_destination(&self, _metadata: Option<&std::fs::Metadata>) -> AppResult<()> {
        Ok(())
    }
    fn must_create_new(&self) -> bool {
        false
    }
    fn prepare(&mut self, _previous_size: u64) -> AppResult<()> {
        Ok(())
    }
    fn recovery_owned(&mut self) {}
    fn published(&mut self, _previous_size: u64) {}
}

#[cfg(test)]
impl ReplacementObserver for () {}

pub(crate) trait DeletionObserver {
    fn publication_started(&mut self) {}
    fn published(&mut self, _staged: &Path) {}
}

#[cfg(test)]
impl DeletionObserver for () {}

#[derive(Clone)]
pub struct TransactionPaths {
    root: PathBuf,
    _ownership_lock: std::sync::Arc<std::fs::File>,
    pending_publication: std::sync::Arc<std::sync::Mutex<Option<ReplaceJournal>>>,
    receipt_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(target_os = "linux")]
    linux_root: LinuxRoot,
    pub uploads: PathBuf,
    pub copies: PathBuf,
    pub backups: PathBuf,
    pub trash: PathBuf,
    pub journals: PathBuf,
    pub deletions: PathBuf,
    pub receipts: PathBuf,
    pub anchors: PathBuf,
}

#[derive(Clone)]
pub(crate) struct StagedDeletion {
    pub path: PathBuf,
    pub bytes_upper_bound: Option<u64>,
    record: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemovalProgress {
    pub removed_nodes: usize,
    pub complete: bool,
}

impl TransactionPaths {
    pub async fn initialize(root: &Path) -> AppResult<Self> {
        #[cfg(target_os = "linux")]
        {
            Self::initialize_linux(root).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::initialize_portable(root).await
        }
    }

    #[cfg(target_os = "linux")]
    async fn initialize_linux(root: &Path) -> AppResult<Self> {
        let linux_root = LinuxRoot::open(root)?;
        Self::initialize_linux_with_root(root, linux_root).await
    }

    #[cfg(target_os = "linux")]
    async fn initialize_linux_with_root(root: &Path, linux_root: LinuxRoot) -> AppResult<Self> {
        let system = root.join(SYSTEM_DIR);
        let system_created = match linux_root.metadata(SYSTEM_DIR).await {
            Ok(metadata) if metadata.is_dir() => false,
            Ok(_) | Err(AppError::Forbidden) => {
                return Err(AppError::Conflict(
                    "Reserved .ycloud-system path must be a private local directory".into(),
                ))
            }
            Err(AppError::NotFound) => {
                linux_root.ensure_private_directory(SYSTEM_DIR).await?;
                true
            }
            Err(error) => return Err(error),
        };
        let marker_relative = format!("{SYSTEM_DIR}/marker");
        if system_created {
            let file = linux_root.create_file_new(&marker_relative, 0o600).await?;
            let mut file = fs::File::from_std(file);
            file.write_all(MARKER.as_bytes())
                .await
                .map_err(|error| AppError::with_source("failed to write storage marker", error))?;
            file.sync_all()
                .await
                .map_err(|error| AppError::with_source("failed to flush storage marker", error))?;
            drop(file);
            linux_root.sync_parent(&marker_relative).await?;
        }
        let marker_metadata = linux_root.metadata(&marker_relative).await.map_err(|_| {
            AppError::Conflict("Reserved .ycloud-system directory is not owned by Ycloud".into())
        })?;
        if !marker_metadata.is_file() || marker_metadata.len() != MARKER.len() as u64 {
            return Err(AppError::Conflict(
                "Reserved .ycloud-system marker must be a regular file".into(),
            ));
        }
        let file = linux_root
            .open_file_for_read(&marker_relative)
            .await
            .map_err(|_| {
                AppError::Conflict(
                    "Reserved .ycloud-system directory is not owned by Ycloud".into(),
                )
            })?;
        let mut bytes = Vec::new();
        file.take(MARKER.len() as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| AppError::with_source("failed to read storage marker", error))?;
        if bytes != MARKER.as_bytes() {
            return Err(AppError::Conflict(
                "Reserved .ycloud-system directory has an invalid marker".into(),
            ));
        }
        linux_root
            .set_private_file_permissions(&marker_relative)
            .await?;
        linux_root.ensure_private_directory(SYSTEM_DIR).await?;
        let lock_relative = format!("{SYSTEM_DIR}/instance.lock");
        let lock = match linux_root.metadata(&lock_relative).await {
            Ok(metadata) if metadata.is_file() => {
                linux_root
                    .open_file_for_read(&lock_relative)
                    .await?
                    .into_std()
                    .await
            }
            Err(AppError::NotFound) => linux_root.create_file_new(&lock_relative, 0o600).await?,
            _ => return Err(AppError::Conflict("本地存储实例锁必须是普通文件".into())),
        };
        lock.try_lock()
            .map_err(|_| AppError::Conflict("本地存储正在由另一个实例使用".into()))?;
        let paths = Self {
            root: root.to_path_buf(),
            _ownership_lock: std::sync::Arc::new(lock),
            pending_publication: Default::default(),
            receipt_count: Default::default(),
            linux_root,
            uploads: system.join("uploads"),
            copies: system.join("copies"),
            backups: system.join("backups"),
            trash: system.join("trash"),
            journals: system.join("transactions"),
            deletions: system.join("deletions"),
            receipts: system.join("upload-receipts"),
            anchors: system.join("upload-anchors"),
        };
        for directory in [
            "uploads",
            "copies",
            "backups",
            "trash",
            "transactions",
            "deletions",
            "upload-receipts",
            "upload-anchors",
        ] {
            paths
                .linux_root
                .ensure_private_directory(&format!("{SYSTEM_DIR}/{directory}"))
                .await?;
        }
        paths.load_receipt_count().await?;
        paths.recover(root).await?;
        Ok(paths)
    }

    #[cfg(not(target_os = "linux"))]
    async fn initialize_portable(root: &Path) -> AppResult<Self> {
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
                if !marker_metadata.is_file()
                    || is_link_or_reparse_point(&marker_metadata)
                    || marker_metadata.len() != MARKER.len() as u64
                {
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
            root: root.to_path_buf(),
            pending_publication: Default::default(),
            receipt_count: Default::default(),
            _ownership_lock: {
                let file =
                    crate::instance_lock::InstanceLock::acquire_file(&system.join("instance.lock"))
                        .map_err(|_| AppError::Conflict("本地存储正在由另一个实例使用".into()))?;
                std::sync::Arc::new(file)
            },
            uploads: system.join("uploads"),
            copies: system.join("copies"),
            backups: system.join("backups"),
            trash: system.join("trash"),
            journals: system.join("transactions"),
            deletions: system.join("deletions"),
            receipts: system.join("upload-receipts"),
            anchors: system.join("upload-anchors"),
        };
        for directory in [
            &paths.uploads,
            &paths.copies,
            &paths.backups,
            &paths.trash,
            &paths.journals,
            &paths.deletions,
            &paths.receipts,
            &paths.anchors,
        ] {
            ensure_private_directory(directory).await?;
        }
        paths.load_receipt_count().await?;
        paths.recover(root).await?;
        Ok(paths)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn linux_root(&self) -> &LinuxRoot {
        &self.linux_root
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn rooted_relative(&self, path: &Path) -> AppResult<String> {
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            AppError::internal("transaction path is outside the local storage root")
        })?;
        relative
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| AppError::Conflict("Transaction path is not valid UTF-8".into()))
    }

    async fn rooted_metadata(&self, path: &Path) -> AppResult<Option<std::fs::Metadata>> {
        #[cfg(target_os = "linux")]
        {
            match self.linux_root.metadata(&self.rooted_relative(path)?).await {
                Ok(metadata) => Ok(Some(metadata)),
                Err(AppError::NotFound) => Ok(None),
                Err(error) => Err(error),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            match fs::symlink_metadata(path).await {
                Ok(metadata) => Ok(Some(metadata)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(AppError::with_source(
                    "failed to inspect transaction resource",
                    error,
                )),
            }
        }
    }

    async fn rooted_exists(&self, path: &Path) -> AppResult<bool> {
        self.rooted_metadata(path)
            .await
            .map(|value| value.is_some())
    }

    async fn validate_destination(&self, relative: &str) -> AppResult<()> {
        if relative.is_empty()
            || relative.len() > 4096
            || crate::storage::StorageService::normalize_relative(relative)? != relative
        {
            return Err(AppError::Conflict(
                "Invalid transaction destination; recovery stopped".into(),
            ));
        }
        #[cfg(target_os = "linux")]
        {
            let mut current = String::new();
            let mut parts = relative.split('/').peekable();
            while let Some(part) = parts.next() {
                if !current.is_empty() {
                    current.push('/');
                }
                current.push_str(part);
                let is_leaf = parts.peek().is_none();
                match self.linux_root.metadata(&current).await {
                    Ok(metadata)
                        if if is_leaf {
                            metadata.is_file()
                        } else {
                            metadata.is_dir()
                        } => {}
                    Err(AppError::NotFound) if is_leaf => {}
                    Err(error) => return Err(error),
                    Ok(_) => {
                        return Err(AppError::Conflict(
                            "Unexpected transaction destination type; recovery stopped".into(),
                        ))
                    }
                }
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            validate_destination(&self.root, relative).await
        }
    }

    async fn rooted_rename_noreplace(&self, source: &Path, destination: &Path) -> AppResult<()> {
        #[cfg(target_os = "linux")]
        {
            self.linux_root
                .rename_noreplace(
                    &self.rooted_relative(source)?,
                    &self.rooted_relative(destination)?,
                )
                .await
        }
        #[cfg(not(target_os = "linux"))]
        {
            if fs::try_exists(destination).await.map_err(|error| {
                AppError::with_source("failed to inspect transaction destination", error)
            })? {
                return Err(AppError::Conflict("Destination already exists".into()));
            }
            fs::rename(source, destination)
                .await
                .map_err(|error| AppError::with_source("failed to rename transaction path", error))
        }
    }

    pub(crate) async fn publish_noreplace(
        &self,
        source: &Path,
        destination: &Path,
    ) -> AppResult<()> {
        self.rooted_rename_noreplace(source, destination).await?;
        self.rooted_sync_parent(source).await?;
        self.rooted_sync_parent(destination).await
    }

    pub(crate) async fn sync_parent_of(&self, path: &Path) -> AppResult<()> {
        self.rooted_sync_parent(path).await
    }

    async fn rooted_remove_file(&self, path: &Path) -> AppResult<()> {
        #[cfg(target_os = "linux")]
        {
            self.linux_root
                .remove_file(&self.rooted_relative(path)?)
                .await
        }
        #[cfg(not(target_os = "linux"))]
        {
            fs::remove_file(path)
                .await
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => AppError::NotFound,
                    _ => AppError::with_source("failed to remove transaction file", error),
                })
        }
    }

    async fn rooted_remove_file_idempotent(&self, path: &Path) -> AppResult<()> {
        match self.rooted_remove_file(path).await {
            Ok(()) | Err(AppError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn remove_staging_file(&self, path: &Path) -> AppResult<RemovalProgress> {
        let existed = self.rooted_exists(path).await?;
        self.rooted_remove_file_idempotent(path).await?;
        self.rooted_sync_parent(path).await?;
        Ok(RemovalProgress {
            removed_nodes: usize::from(existed),
            complete: true,
        })
    }

    pub(crate) async fn remove_bounded(
        &self,
        path: &Path,
        max_removed_nodes: usize,
    ) -> AppResult<RemovalProgress> {
        #[cfg(target_os = "linux")]
        {
            let (removed_nodes, complete) = self
                .linux_root
                .remove_any_bounded(&self.rooted_relative(path)?, max_removed_nodes)
                .await?;
            Ok(RemovalProgress {
                removed_nodes,
                complete,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            remove_any_bounded(path, max_removed_nodes).await
        }
    }

    async fn rooted_sync_parent(&self, path: &Path) -> AppResult<()> {
        #[cfg(target_os = "linux")]
        {
            self.linux_root
                .sync_parent(&self.rooted_relative(path)?)
                .await
        }
        #[cfg(not(target_os = "linux"))]
        {
            sync_parent(path).await
        }
    }

    async fn rooted_create_private_file(&self, path: &Path) -> AppResult<fs::File> {
        #[cfg(target_os = "linux")]
        {
            self.linux_root
                .create_file_new(&self.rooted_relative(path)?, 0o600)
                .await
                .map(fs::File::from_std)
        }
        #[cfg(not(target_os = "linux"))]
        {
            private_file_options().open(path).await.map_err(|error| {
                AppError::with_source("failed to create private transaction file", error)
            })
        }
    }

    #[cfg(all(target_os = "linux", not(test)))]
    pub(crate) async fn create_upload_file(&self, path: &Path) -> AppResult<std::fs::File> {
        if path.parent() != Some(self.uploads.as_path()) {
            return Err(AppError::internal(
                "upload path is outside the transaction upload directory",
            ));
        }
        resource_id(path)?;
        Ok(self
            .rooted_create_private_file(path)
            .await?
            .into_std()
            .await)
    }

    async fn write_json_atomic(&self, path: &Path, value: &impl Serialize) -> AppResult<()> {
        self.write_json_atomic_inner(path, value, false).await
    }

    async fn write_json_atomic_inner(
        &self,
        path: &Path,
        value: &impl Serialize,
        replace_existing: bool,
    ) -> AppResult<()> {
        let temporary = path.with_extension("tmp");
        if replace_existing {
            if path.parent() != Some(self.journals.as_path()) {
                return Err(AppError::Forbidden);
            }
            self.rooted_remove_file_idempotent(&temporary).await?;
        }
        let bytes = serde_json::to_vec(value)
            .map_err(|e| AppError::with_source("failed to encode transaction journal", e))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(AppError::Conflict(
                "Transaction journal exceeds its size limit".into(),
            ));
        }
        let mut file = self.rooted_create_private_file(&temporary).await?;
        file.write_all(&bytes)
            .await
            .map_err(|e| AppError::with_source("failed to write transaction journal", e))?;
        file.sync_all()
            .await
            .map_err(|e| AppError::with_source("failed to flush transaction journal", e))?;
        drop(file);
        if replace_existing {
            #[cfg(target_os = "linux")]
            self.linux_root
                .replace_internal_journal(
                    &self.rooted_relative(&temporary)?,
                    &self.rooted_relative(path)?,
                )
                .await?;
            #[cfg(not(target_os = "linux"))]
            fs::rename(&temporary, path).await.map_err(|error| {
                AppError::with_source("failed to replace publication journal", error)
            })?;
        } else {
            self.rooted_rename_noreplace(&temporary, path).await?;
        }
        self.rooted_sync_parent(path).await
    }

    async fn inventory(&self, directory: &Path, kind: InventoryKind) -> AppResult<Vec<PathBuf>> {
        let (entries, more) = self
            .inventory_page(directory, kind, MAX_RECOVERY_ENTRIES)
            .await?;
        if more {
            return Err(AppError::Conflict(
                "Recovery inventory exceeds its budget; resources retained".into(),
            ));
        }
        Ok(entries)
    }

    async fn inventory_page(
        &self,
        directory: &Path,
        kind: InventoryKind,
        limit: usize,
    ) -> AppResult<(Vec<PathBuf>, bool)> {
        #[cfg(target_os = "linux")]
        {
            let (mut entries, task) = self
                .linux_root
                .scan_directory_with_policy(&self.rooted_relative(directory)?, true);
            let mut result = Vec::new();
            while let Some(entry) = entries.recv().await {
                if result.len() >= limit {
                    drop(entries);
                    task.await.map_err(|error| {
                        AppError::with_source("local recovery listing task failed", error)
                    })??;
                    return Ok((result, true));
                }
                let path = directory.join(entry.name);
                validate_inventory_entry(&path, &entry.metadata, kind)?;
                result.push(path);
            }
            task.await
                .map_err(|error| {
                    AppError::with_source("local recovery listing task failed", error)
                })?
                .map_err(|error| match error {
                    AppError::Forbidden => {
                        AppError::Conflict("Unexpected internal link; recovery stopped".into())
                    }
                    error => error,
                })?;
            Ok((result, false))
        }
        #[cfg(not(target_os = "linux"))]
        {
            inventory_page(directory, kind, limit).await
        }
    }

    async fn read_deletion_journal(&self, path: &Path) -> AppResult<DeletionJournal> {
        #[cfg(target_os = "linux")]
        let file = self
            .linux_root
            .open_file_for_read(&self.rooted_relative(path)?)
            .await?;
        #[cfg(not(target_os = "linux"))]
        let file = fs::File::open(path)
            .await
            .map_err(|error| AppError::with_source("failed to open deletion debt record", error))?;
        decode_deletion_journal(path, file).await
    }

    pub fn upload_path(&self, id: &TransactionId) -> PathBuf {
        self.uploads.join(id)
    }

    pub fn copy_path(&self, id: &TransactionId) -> PathBuf {
        self.copies.join(id)
    }

    /// Only staged deletions are eligible for online cleanup. Uploads, copies
    /// and replacement backups may still have live owners or need recovery.
    pub(crate) async fn staged_deletions(&self) -> AppResult<(Vec<StagedDeletion>, bool)> {
        let record_files = self
            .inventory(&self.deletions, InventoryKind::Journal)
            .await?;
        let mut records = std::collections::HashMap::new();
        let mut incomplete_records = std::collections::HashMap::new();
        for path in record_files {
            if path.extension().and_then(|ext| ext.to_str()) == Some("tmp") {
                let id = resource_id(&path.with_extension(""))?;
                incomplete_records.insert(id, path);
                continue;
            }
            let record = self.read_deletion_journal(&path).await?;
            records.insert(record.id.clone(), (path, record));
        }
        let (trash, trash_more) = self
            .inventory_page(&self.trash, InventoryKind::Tree, 128)
            .await?;
        let mut result = Vec::with_capacity(trash.len());
        for path in trash {
            let id = resource_id(&path)?;
            let record = records.remove(&id);
            let incomplete_record = if record.is_none() {
                incomplete_records.remove(&id)
            } else {
                None
            };
            result.push(StagedDeletion {
                path,
                bytes_upper_bound: record.as_ref().map(|(_, record)| record.bytes_upper_bound),
                record: record.map(|(path, _)| path).or(incomplete_record),
            });
        }
        if trash_more {
            return Ok((result, true));
        }
        let remaining_records = records
            .into_values()
            .map(|(record_path, record)| StagedDeletion {
                path: self.trash.join(&record.id),
                bytes_upper_bound: Some(record.bytes_upper_bound),
                record: Some(record_path),
            })
            .chain(
                incomplete_records
                    .into_iter()
                    .map(|(id, record_path)| StagedDeletion {
                        path: self.trash.join(id),
                        bytes_upper_bound: None,
                        record: Some(record_path),
                    }),
            );
        for item in remaining_records {
            if result.len() >= 128 {
                return Ok((result, true));
            }
            result.push(item);
        }
        Ok((result, false))
    }

    pub(crate) async fn finish_staged_deletion(&self, item: &StagedDeletion) -> AppResult<()> {
        if let Some(record) = &item.record {
            self.rooted_remove_file_idempotent(record).await?;
            self.rooted_sync_parent(record).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn commit_file(
        &self,
        relative: &str,
        temporary: &Path,
        destination: &Path,
        ownership: &mut UploadOwnership,
        observer: &mut impl ReplacementObserver,
    ) -> AppResult<u64> {
        self.commit_file_with_operation(relative, temporary, destination, ownership, observer, None)
            .await
    }

    pub(crate) async fn commit_file_with_operation(
        &self,
        relative: &str,
        temporary: &Path,
        destination: &Path,
        ownership: &mut UploadOwnership,
        observer: &mut impl ReplacementObserver,
        operation: Option<(&str, u64)>,
    ) -> AppResult<u64> {
        self.settle_publication().await?;
        if destination != self.root.join(relative) {
            return Err(AppError::internal(
                "destination does not belong to this transaction namespace",
            ));
        }
        self.validate_destination(relative).await?;
        let metadata = self.rooted_metadata(destination).await?;
        observer.check_destination(metadata.as_ref())?;
        if observer.must_create_new() && metadata.is_some() {
            return Err(AppError::Conflict(
                "上传目标已被占用，不能覆盖已有文件".into(),
            ));
        }
        if let Some(metadata) = metadata.as_ref() {
            if !metadata.file_type().is_file() {
                return Err(AppError::Conflict(
                    "Only a regular file can be replaced by an upload".into(),
                ));
            }
        }
        let id = resource_id(temporary)?;
        if temporary != self.upload_path(&id) {
            return Err(AppError::internal(
                "upload does not belong to this transaction namespace",
            ));
        }
        let journal_path = self.journals.join(format!("{id}.json"));
        let mut journal = ReplaceJournal::new(id.clone(), relative.to_string())?;
        if let Some((operation_id, size)) = operation {
            journal.operation_id = Some(operation_id.to_owned());
            journal.operation_size = Some(size);
            journal.validate()?;
            self.check_receipt_capacity(operation_id).await?;
        }
        let previous_size = metadata.as_ref().map_or(0, std::fs::Metadata::len);
        observer.prepare(previous_size)?;
        // No await between the ownership transfer and the journal operation.
        // From here an error/cancellation must preserve the recovery set, not
        // let the writer's Drop race a pending journal write or rename.
        *ownership = UploadOwnership::Recovery;
        observer.recovery_owned();
        #[cfg(target_os = "linux")]
        if operation.is_some() {
            let anchor = self.anchors.join(&id);
            if self.rooted_exists(&anchor).await? {
                return Err(AppError::Conflict("Upload anchor already exists".into()));
            }
            journal.anchored = self
                .linux_root
                .hard_link_noreplace(
                    &self.rooted_relative(temporary)?,
                    &self.rooted_relative(&anchor)?,
                )
                .await?;
            if journal.anchored {
                self.rooted_sync_parent(&anchor).await?;
            } else {
                tracing::warn!(transaction_id = %id, "local filesystem does not support upload publication anchors; retaining conservative crash recovery");
            }
        }
        self.write_json_atomic(&journal_path, &journal).await?;
        let backup = self.backups.join(&id);
        if metadata.is_some() {
            self.rooted_rename_noreplace(destination, &backup).await?;
        }
        if let Err(error) = self.rooted_rename_noreplace(temporary, destination).await {
            if self.rooted_exists(&backup).await? {
                if let Err(restore_error) = self.rooted_rename_noreplace(&backup, destination).await
                {
                    tracing::warn!(transaction_id = %id, error = %restore_error, "replacement rollback pending; journal retained");
                }
            }
            return Err(AppError::with_source(
                "failed to commit uploaded file",
                error,
            ));
        }
        observer.published(previous_size);
        *self
            .pending_publication
            .lock()
            .expect("publication mutex poisoned") = Some(journal.clone());
        self.settle_publication().await?;
        if let (Some(operation_id), Some(size)) = (&journal.operation_id, journal.operation_size) {
            self.ensure_receipt(operation_id, &journal.destination, size, true)
                .await?;
        }
        if self.rooted_exists(&backup).await? {
            self.rooted_remove_file(&backup).await?;
            self.rooted_sync_parent(&backup).await?;
        }
        if journal.anchored {
            let anchor = self.anchors.join(&id);
            self.rooted_remove_file_idempotent(&anchor).await?;
            self.rooted_sync_parent(&anchor).await?;
        }
        self.rooted_remove_file(&journal_path).await?;
        self.rooted_sync_parent(&journal_path).await?;
        Ok(previous_size)
    }

    /// Called under the storage mutation gate. Later mutations wait only for
    /// publication durability, never for old backup cleanup. Keep the in-memory
    /// owner on failure so a successful delete/move cannot outrun its receipt.
    pub(crate) async fn settle_publication(&self) -> AppResult<()> {
        let pending = self
            .pending_publication
            .lock()
            .expect("publication mutex poisoned")
            .clone();
        let Some(mut journal) = pending else {
            return Ok(());
        };
        self.rooted_sync_parent(&self.root.join(&journal.destination))
            .await?;
        self.rooted_sync_parent(&self.upload_path(&journal.id))
            .await?;
        journal.published = true;
        self.write_json_atomic_inner(
            &self.journals.join(format!("{}.json", journal.id)),
            &journal,
            true,
        )
        .await?;
        *self
            .pending_publication
            .lock()
            .expect("publication mutex poisoned") = None;
        Ok(())
    }

    pub async fn stage_delete(
        &self,
        source: &Path,
        bytes_upper_bound: u64,
        observer: &mut impl DeletionObserver,
    ) -> AppResult<PathBuf> {
        self.settle_publication().await?;
        let id = TransactionId::new();
        let target = self.trash.join(&id);
        let record_path = self.deletions.join(format!("{id}.json"));
        let record = DeletionJournal::new(id, bytes_upper_bound)?;
        self.write_json_atomic(&record_path, &record)
            .await
            .map_err(|error| {
                error.with_operation(
                    crate::error::CommitState::NotCommitted,
                    crate::error::CleanupState::Pending,
                )
            })?;
        // No await between marking the uncertain publication window and the
        // rename operation that may or may not reach the filesystem.
        observer.publication_started();
        self.rooted_rename_noreplace(source, &target)
            .await
            .map_err(|e| {
                e.with_operation(
                    crate::error::CommitState::Unknown,
                    crate::error::CleanupState::Unknown,
                )
            })?;
        observer.published(&target);
        let synced = async {
            self.rooted_sync_parent(source).await?;
            self.rooted_sync_parent(&target).await
        }
        .await;
        synced.map_err(|error| {
            error.with_operation(
                crate::error::CommitState::Committed,
                crate::error::CleanupState::Pending,
            )
        })?;
        Ok(target)
    }
}

#[cfg(not(target_os = "linux"))]
fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    options
}

#[cfg(not(target_os = "linux"))]
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

#[cfg(all(unix, not(target_os = "linux")))]
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

#[cfg(all(unix, not(target_os = "linux")))]
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

#[cfg(any(not(target_os = "linux"), test))]
pub(crate) async fn remove_any_bounded(
    path: &Path,
    max_removed_nodes: usize,
) -> AppResult<RemovalProgress> {
    if max_removed_nodes == 0 {
        return Ok(RemovalProgress {
            removed_nodes: 0,
            complete: false,
        });
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || remove_any_bounded_blocking(&path, max_removed_nodes))
        .await
        .map_err(|error| AppError::with_source("bounded cleanup task failed", error))?
}

#[cfg(any(not(target_os = "linux"), test))]
fn remove_any_bounded_blocking(
    path: &Path,
    max_removed_nodes: usize,
) -> AppResult<RemovalProgress> {
    const MAX_WALK_DEPTH: usize = 256;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RemovalProgress {
                removed_nodes: 0,
                complete: true,
            })
        }
        Err(error) => {
            return Err(AppError::with_source(
                "failed to inspect bounded cleanup path",
                error,
            ))
        }
    };
    if is_link_or_reparse_point(&metadata) {
        return Err(AppError::Conflict(
            "Unexpected internal link; bounded cleanup stopped".into(),
        ));
    }
    if metadata.is_file() {
        std::fs::remove_file(path)
            .map_err(|error| AppError::with_source("failed to remove cleanup file", error))?;
        return Ok(RemovalProgress {
            removed_nodes: 1,
            complete: true,
        });
    }
    if !metadata.is_dir() {
        return Err(AppError::Conflict(
            "Unexpected internal resource type; bounded cleanup stopped".into(),
        ));
    }

    let mut stack = vec![(
        path.to_path_buf(),
        std::fs::read_dir(path)
            .map_err(|error| AppError::with_source("failed to read cleanup directory", error))?,
    )];
    let mut removed_nodes = 0;
    while removed_nodes < max_removed_nodes {
        let next = stack
            .last_mut()
            .expect("bounded cleanup stack cannot be empty")
            .1
            .next();
        match next {
            Some(Ok(entry)) => {
                let child = entry.path();
                let metadata = std::fs::symlink_metadata(&child).map_err(|error| {
                    AppError::with_source("failed to inspect cleanup directory entry", error)
                })?;
                if is_link_or_reparse_point(&metadata) {
                    return Err(AppError::Conflict(
                        "Unexpected internal link; bounded cleanup stopped".into(),
                    ));
                }
                if metadata.is_dir() {
                    if stack.len() >= MAX_WALK_DEPTH {
                        return Err(AppError::Conflict(
                            "Cleanup directory depth exceeds its budget".into(),
                        ));
                    }
                    let entries = std::fs::read_dir(&child).map_err(|error| {
                        AppError::with_source("failed to read cleanup directory", error)
                    })?;
                    stack.push((child, entries));
                } else if metadata.is_file() {
                    std::fs::remove_file(&child).map_err(|error| {
                        AppError::with_source("failed to remove cleanup file", error)
                    })?;
                    removed_nodes += 1;
                } else {
                    return Err(AppError::Conflict(
                        "Unexpected internal resource type; bounded cleanup stopped".into(),
                    ));
                }
            }
            Some(Err(error)) => {
                return Err(AppError::with_source(
                    "failed to read cleanup directory entry",
                    error,
                ))
            }
            None => {
                let (directory, entries) =
                    stack.pop().expect("bounded cleanup stack cannot be empty");
                drop(entries);
                std::fs::remove_dir(&directory).map_err(|error| {
                    AppError::with_source("failed to remove cleanup directory", error)
                })?;
                removed_nodes += 1;
                if stack.is_empty() {
                    return Ok(RemovalProgress {
                        removed_nodes,
                        complete: true,
                    });
                }
            }
        }
    }
    Ok(RemovalProgress {
        removed_nodes,
        complete: false,
    })
}

#[derive(Clone, Copy)]
enum InventoryKind {
    Journal,
    Receipt,
    File,
    Tree,
}

#[cfg(not(target_os = "linux"))]
async fn inventory_page(
    directory: &Path,
    kind: InventoryKind,
    limit: usize,
) -> AppResult<(Vec<PathBuf>, bool)> {
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|error| AppError::with_source("failed to inspect transaction directory", error))?;
    let mut result = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| AppError::with_source("failed to inspect transaction entry", error))?
    {
        if result.len() >= limit {
            return Ok((result, true));
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|error| AppError::with_source("failed to inspect transaction entry", error))?;
        validate_inventory_entry(&path, &metadata, kind)?;
        result.push(path);
    }
    Ok((result, false))
}

fn validate_inventory_entry(
    path: &Path,
    metadata: &std::fs::Metadata,
    kind: InventoryKind,
) -> AppResult<()> {
    if is_link_or_reparse_point(metadata)
        || !(metadata.is_file() || matches!(kind, InventoryKind::Tree) && metadata.is_dir())
    {
        return Err(AppError::Conflict(
            "Unexpected transaction resource type; recovery stopped".into(),
        ));
    }
    if matches!(kind, InventoryKind::Journal | InventoryKind::Receipt) {
        if !matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("json" | "tmp")
        ) {
            return Err(AppError::Conflict(
                "Unrecognized transaction record; recovery stopped".into(),
            ));
        }
        let identity = path.with_extension("");
        if matches!(kind, InventoryKind::Receipt) {
            let valid = identity
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(metadata::valid_operation_id);
            if !valid {
                return Err(AppError::Conflict("Invalid upload receipt identity".into()));
            }
        } else {
            resource_id(&identity)?;
        }
    } else {
        resource_id(path)?;
    }
    Ok(())
}

async fn decode_deletion_journal(path: &Path, file: fs::File) -> AppResult<DeletionJournal> {
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| AppError::with_source("failed to read deletion debt record", error))?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(AppError::Conflict(
            "Deletion debt record exceeds its size limit; recovery stopped".into(),
        ));
    }
    let record: DeletionJournal = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::with_source("invalid deletion debt record; recovery stopped", error)
    })?;
    record.validate()?;
    if record.id != resource_id(&path.with_extension(""))? {
        return Err(AppError::Conflict(
            "Deletion debt filename and identifier disagree; recovery stopped".into(),
        ));
    }
    Ok(record)
}

#[cfg(not(target_os = "linux"))]
async fn validate_destination(root: &Path, relative: &str) -> AppResult<()> {
    if relative.is_empty()
        || relative.len() > 4096
        || crate::storage::StorageService::normalize_relative(relative)? != relative
    {
        return Err(AppError::Conflict(
            "Invalid transaction destination; recovery stopped".into(),
        ));
    }
    let mut path = root.to_path_buf();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let is_leaf = parts.peek().is_none();
        match fs::symlink_metadata(&path).await {
            Ok(metadata)
                if !is_link_or_reparse_point(&metadata)
                    && if is_leaf {
                        metadata.is_file()
                    } else {
                        metadata.is_dir()
                    } => {}
            Err(error) if is_leaf && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(AppError::with_source(
                    "failed to inspect transaction destination",
                    error,
                ))
            }
            Ok(_) => {
                return Err(AppError::Conflict(
                    "Unexpected transaction destination type; recovery stopped".into(),
                ))
            }
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
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
    use super::{
        remove_any_bounded, DeletionJournal, ReplaceJournal, TransactionId, TransactionPaths,
        UploadOwnership, SYSTEM_DIR,
    };
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn owned_upload_replaces_a_regular_file_and_clears_its_journal() {
        let root = TestDirectory::new("owned-replacement");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let destination = root.path().join("report.txt");
        tokio::fs::write(&destination, b"old").await.unwrap();
        let temporary = paths.upload_path(&TransactionId::new());
        tokio::fs::write(&temporary, b"new report").await.unwrap();
        assert_eq!(
            paths
                .commit_file(
                    "report.txt",
                    &temporary,
                    &destination,
                    &mut UploadOwnership::Writer,
                    &mut (),
                )
                .await
                .unwrap(),
            3
        );
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"new report");
        assert!(!temporary.exists());
        assert!(tokio::fs::read_dir(&paths.journals)
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none());
        drop(paths);
        TransactionPaths::initialize(root.path()).await.unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"new report");
    }

    #[tokio::test]
    async fn upload_receipt_proves_the_operation_not_a_same_sized_file() {
        let root = TestDirectory::new("upload-operation-receipt");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let destination = root.path().join("report.txt");
        tokio::fs::write(&destination, b"old").await.unwrap();
        let unrelated = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert!(!paths
            .receipt_matches("report.txt", 3, unrelated)
            .await
            .unwrap());
        let temporary = paths.upload_path(&TransactionId::new());
        tokio::fs::write(&temporary, b"new").await.unwrap();
        let operation = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        paths
            .commit_file_with_operation(
                "report.txt",
                &temporary,
                &destination,
                &mut UploadOwnership::Writer,
                &mut (),
                Some((operation, 3)),
            )
            .await
            .unwrap();
        assert!(paths
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
        tokio::fs::remove_file(&destination).await.unwrap();
        tokio::fs::write(&destination, b"xxx").await.unwrap();
        assert!(paths
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
        assert!(!paths
            .receipt_matches("report.txt", 4, operation)
            .await
            .unwrap());
        drop(paths);
        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert!(recovered
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
        recovered
            .prune_receipts(&std::collections::HashSet::from([operation.into()]))
            .await
            .unwrap();
        assert!(recovered
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
        recovered
            .prune_receipts(&std::collections::HashSet::new())
            .await
            .unwrap();
        assert!(!recovered
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn anchored_rename_without_publication_marker_recovers_confirmed_receipt() {
        let root = TestDirectory::new("anchored-upload-crash");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let operation = "abababababababababababababababab";
        let upload = paths.upload_path(&id);
        let anchor = paths.anchors.join(&id);
        let destination = root.path().join("report.txt");
        tokio::fs::write(&upload, b"new").await.unwrap();
        tokio::fs::write(&destination, b"old").await.unwrap();
        tokio::fs::hard_link(&upload, &anchor).await.unwrap();
        let mut journal = ReplaceJournal::new(id.clone(), "report.txt".into()).unwrap();
        journal.operation_id = Some(operation.into());
        journal.operation_size = Some(3);
        journal.anchored = true;
        tokio::fs::write(
            paths.journals.join(format!("{id}.json")),
            serde_json::to_vec(&journal).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::rename(&destination, paths.backups.join(&id))
            .await
            .unwrap();
        tokio::fs::rename(&upload, &destination).await.unwrap();
        drop(paths);

        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert!(recovered
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
        assert_eq!(tokio::fs::read(destination).await.unwrap(), b"new");
        assert!(!anchor.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn anchored_rename_does_not_accept_an_unrelated_same_size_file() {
        let root = TestDirectory::new("anchored-upload-mismatch");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let upload = paths.upload_path(&id);
        let anchor = paths.anchors.join(&id);
        let destination = root.path().join("report.txt");
        tokio::fs::write(&upload, b"new").await.unwrap();
        tokio::fs::write(&destination, b"old").await.unwrap();
        tokio::fs::hard_link(&upload, &anchor).await.unwrap();
        let mut journal = ReplaceJournal::new(id.clone(), "report.txt".into()).unwrap();
        journal.operation_id = Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd".into());
        journal.operation_size = Some(3);
        journal.anchored = true;
        let record = paths.journals.join(format!("{id}.json"));
        let backup = paths.backups.join(&id);
        tokio::fs::write(&record, serde_json::to_vec(&journal).unwrap())
            .await
            .unwrap();
        tokio::fs::rename(&destination, &backup).await.unwrap();
        tokio::fs::remove_file(&upload).await.unwrap();
        tokio::fs::write(&destination, b"new").await.unwrap();
        drop(paths);

        assert!(TransactionPaths::initialize(root.path()).await.is_err());
        assert!(anchor.exists());
        assert!(record.exists());
        assert_eq!(tokio::fs::read(backup).await.unwrap(), b"old");
    }

    #[tokio::test]
    async fn unclaimed_anchor_preserves_the_recovery_set() {
        let root = TestDirectory::new("recovery-unclaimed-anchor");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let journal = ReplaceJournal::new(id.clone(), "report.txt".into()).unwrap();
        let journal_path = paths.journals.join(format!("{id}.json"));
        let upload = paths.upload_path(&id);
        let backup = paths.backups.join(&id);
        let anchor = paths.anchors.join(&id);
        let copy = paths.copy_path(&TransactionId::new());
        let trash = paths.trash.join(TransactionId::new());
        let pending_journal = paths.journals.join(format!("{}.tmp", TransactionId::new()));
        for (path, bytes) in [
            (&upload, b"pending".as_slice()),
            (&backup, b"original".as_slice()),
            (&anchor, b"anchor".as_slice()),
            (&pending_journal, b"partial".as_slice()),
        ] {
            tokio::fs::write(path, bytes).await.unwrap();
        }
        tokio::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap())
            .await
            .unwrap();
        for path in [&copy, &trash] {
            tokio::fs::create_dir(path).await.unwrap();
            tokio::fs::write(path.join("child.txt"), b"staged")
                .await
                .unwrap();
        }
        drop(paths);

        let error = TransactionPaths::initialize(root.path())
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, crate::error::AppError::Conflict(ref message)
            if message == "Unclaimed upload anchor; recovery stopped")
        );
        for (path, bytes) in [
            (&upload, b"pending".as_slice()),
            (&backup, b"original".as_slice()),
            (&anchor, b"anchor".as_slice()),
            (&pending_journal, b"partial".as_slice()),
        ] {
            assert_eq!(tokio::fs::read(path).await.unwrap(), bytes);
        }
        assert!(journal_path.exists());
        for path in [&copy, &trash] {
            assert_eq!(
                tokio::fs::read(path.join("child.txt")).await.unwrap(),
                b"staged"
            );
        }
        assert!(!root.path().join("report.txt").exists());
    }

    #[tokio::test]
    async fn orphan_anchor_is_reclaimed_without_touching_formal_files() {
        let root = TestDirectory::new("recovery-orphan-anchor");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let anchor = paths.anchors.join(TransactionId::new());
        let destination = root.path().join("report.txt");
        tokio::fs::write(&anchor, b"staged").await.unwrap();
        tokio::fs::write(&destination, b"formal").await.unwrap();
        drop(paths);

        for _ in 0..2 {
            let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
            assert!(!anchor.exists());
            assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"formal");
            drop(recovered);
        }
    }

    #[tokio::test]
    async fn declared_anchor_does_not_turn_rollback_into_a_success_receipt() {
        let root = TestDirectory::new("recovery-anchored-rollback");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let operation = "abababababababababababababababab";
        let mut journal = ReplaceJournal::new(id.clone(), "report.txt".into()).unwrap();
        journal.anchored = true;
        journal.operation_id = Some(operation.into());
        journal.operation_size = Some(7);
        let journal_path = paths.journals.join(format!("{id}.json"));
        let upload = paths.upload_path(&id);
        let backup = paths.backups.join(&id);
        let anchor = paths.anchors.join(&id);
        tokio::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap())
            .await
            .unwrap();
        tokio::fs::write(&upload, b"pending").await.unwrap();
        tokio::fs::write(&backup, b"original").await.unwrap();
        tokio::fs::write(&anchor, b"pending").await.unwrap();
        drop(paths);

        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.path().join("report.txt"))
                .await
                .unwrap(),
            b"original"
        );
        for path in [&journal_path, &upload, &backup, &anchor] {
            assert!(!path.exists());
        }
        assert!(!recovered
            .receipt_matches("report.txt", 7, operation)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn recovery_restores_receipt_after_publication_was_recorded() {
        let root = TestDirectory::new("upload-published-receipt-recovery");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let operation = "cccccccccccccccccccccccccccccccc";
        let mut journal = ReplaceJournal::new(TransactionId::new(), "report.txt".into()).unwrap();
        journal.published = true;
        journal.operation_id = Some(operation.into());
        journal.operation_size = Some(3);
        tokio::fs::write(
            paths.journals.join(format!("{}.json", journal.id)),
            serde_json::to_vec(&journal).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(paths.receipts.join(format!("{operation}.tmp")), b"partial")
            .await
            .unwrap();
        // The file may already have been moved or deleted; the published
        // journal, not current path contents, proves the historical commit.
        drop(paths);
        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert!(recovered
            .receipt_matches("report.txt", 3, operation)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn recovery_preserves_backup_when_an_external_destination_appears() {
        let root = TestDirectory::new("upload-external-destination");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let upload = paths.upload_path(&id);
        let backup = paths.backups.join(&id);
        let destination = root.path().join("report.txt");
        let journal = ReplaceJournal::new(id.clone(), "report.txt".into()).unwrap();
        tokio::fs::write(&upload, b"pending").await.unwrap();
        tokio::fs::write(&backup, b"original").await.unwrap();
        tokio::fs::write(&destination, b"external").await.unwrap();
        tokio::fs::write(
            paths.journals.join(format!("{id}.json")),
            serde_json::to_vec(&journal).unwrap(),
        )
        .await
        .unwrap();
        drop(paths);

        assert!(TransactionPaths::initialize(root.path()).await.is_err());
        assert_eq!(tokio::fs::read(&backup).await.unwrap(), b"original");
        assert_eq!(tokio::fs::read(&upload).await.unwrap(), b"pending");
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"external");
    }

    #[tokio::test]
    async fn unmarked_publication_never_becomes_a_certain_success() {
        let root = TestDirectory::new("upload-unmarked-publication");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let operation = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let mut journal = ReplaceJournal::new(TransactionId::new(), "report.txt".into()).unwrap();
        journal.operation_id = Some(operation.into());
        journal.operation_size = Some(3);
        tokio::fs::write(root.path().join("report.txt"), b"new")
            .await
            .unwrap();
        tokio::fs::write(
            paths.journals.join(format!("{}.json", journal.id)),
            serde_json::to_vec(&journal).unwrap(),
        )
        .await
        .unwrap();
        drop(paths);
        let recovered = TransactionPaths::initialize(root.path()).await.unwrap();
        assert!(matches!(
            recovered.receipt_matches("report.txt", 3, operation).await,
            Err(crate::error::AppError::Conflict(_))
        ));
        assert_eq!(
            tokio::fs::read(root.path().join("report.txt"))
                .await
                .unwrap(),
            b"new"
        );
    }

    #[tokio::test]
    async fn legacy_two_field_journal_is_recovered_without_rewriting_user_bytes() {
        let root = TestDirectory::new("legacy-journal");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        tokio::fs::create_dir(root.path().join("documents"))
            .await
            .unwrap();
        tokio::fs::write(paths.backups.join(&id), b"original document")
            .await
            .unwrap();
        tokio::fs::write(paths.upload_path(&id), b"unpublished document")
            .await
            .unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "id": id.to_string(), "destination": "documents/report.txt"
        }))
        .unwrap();
        tokio::fs::write(paths.journals.join(format!("{id}.json")), bytes)
            .await
            .unwrap();
        drop(paths);
        TransactionPaths::initialize(root.path()).await.unwrap();
        TransactionPaths::initialize(root.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.path().join("documents/report.txt"))
                .await
                .unwrap(),
            b"original document"
        );
    }

    #[tokio::test]
    async fn future_format_preserves_the_entire_recovery_set() {
        let root = TestDirectory::new("future-journal");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let current_id = TransactionId::new();
        let current_record = paths.journals.join(format!("{current_id}.json"));
        let current_backup = paths.backups.join(&current_id);
        tokio::fs::write(&current_backup, b"previous")
            .await
            .unwrap();
        tokio::fs::write(root.path().join("current.txt"), b"current")
            .await
            .unwrap();
        let current_bytes =
            serde_json::to_vec(&ReplaceJournal::new(current_id, "current.txt".into()).unwrap())
                .unwrap();
        tokio::fs::write(&current_record, &current_bytes)
            .await
            .unwrap();
        let id = TransactionId::new();
        let pending = paths.upload_path(&id);
        tokio::fs::write(&pending, b"pending data").await.unwrap();
        let record = paths.journals.join(format!("{id}.json"));
        let bytes = serde_json::to_vec(&ReplaceJournal {
            version: 4,
            id,
            destination: "report.txt".into(),
            published: false,
            operation_id: None,
            operation_size: None,
            anchored: false,
        })
        .unwrap();
        tokio::fs::write(&record, &bytes).await.unwrap();
        drop(paths);
        assert!(TransactionPaths::initialize(root.path()).await.is_err());
        assert_eq!(tokio::fs::read(&record).await.unwrap(), bytes);
        assert_eq!(tokio::fs::read(&pending).await.unwrap(), b"pending data");
        assert_eq!(tokio::fs::read(&current_backup).await.unwrap(), b"previous");
        assert_eq!(
            tokio::fs::read(&current_record).await.unwrap(),
            current_bytes
        );
        assert!(!root.path().join("report.txt").exists());
    }

    #[tokio::test]
    async fn missing_destination_parent_retains_the_backup_and_journal() {
        let root = TestDirectory::new("unavailable-parent");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let backup = paths.backups.join(&id);
        tokio::fs::write(&backup, b"original").await.unwrap();
        let record = paths.journals.join(format!("{id}.json"));
        let bytes =
            serde_json::to_vec(&ReplaceJournal::new(id, "documents/report.txt".into()).unwrap())
                .unwrap();
        tokio::fs::write(&record, &bytes).await.unwrap();
        drop(paths);
        assert!(TransactionPaths::initialize(root.path()).await.is_err());
        assert_eq!(tokio::fs::read(&backup).await.unwrap(), b"original");
        assert_eq!(tokio::fs::read(&record).await.unwrap(), bytes);
        tokio::fs::create_dir(root.path().join("documents"))
            .await
            .unwrap();
        TransactionPaths::initialize(root.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.path().join("documents/report.txt"))
                .await
                .unwrap(),
            b"original"
        );
    }

    #[tokio::test]
    async fn completed_copy_and_delete_staging_is_reclaimed_idempotently() {
        let root = TestDirectory::new("owned-staging");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let copy = paths.copy_path(&TransactionId::new());
        tokio::fs::create_dir(&copy).await.unwrap();
        tokio::fs::write(copy.join("report.txt"), b"copy")
            .await
            .unwrap();
        let source = root.path().join("old.txt");
        tokio::fs::write(&source, b"old").await.unwrap();
        let trash = paths.stage_delete(&source, 3, &mut ()).await.unwrap();
        drop(paths);
        TransactionPaths::initialize(root.path()).await.unwrap();
        TransactionPaths::initialize(root.path()).await.unwrap();
        assert!(!copy.exists());
        assert!(!trash.exists());
        assert!(!source.exists());
    }

    #[tokio::test]
    async fn staged_delete_persists_its_byte_debt_until_cleanup_finishes() {
        let root = TestDirectory::new("persistent-delete-debt");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let source = root.path().join("large.bin");
        tokio::fs::write(&source, b"content").await.unwrap();

        let trash = paths
            .stage_delete(&source, 1_048_576, &mut ())
            .await
            .unwrap();
        let (staged, more) = paths.staged_deletions().await.unwrap();
        assert!(!more);
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].path, trash);
        assert_eq!(staged[0].bytes_upper_bound, Some(1_048_576));
        assert!(staged[0].record.as_ref().is_some_and(|path| path.exists()));

        assert!(remove_any_bounded(&trash, 1).await.unwrap().complete);
        paths.finish_staged_deletion(&staged[0]).await.unwrap();
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
        assert!(tokio::fs::read_dir(&paths.deletions)
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn unknown_deletion_debt_version_preserves_the_entire_recovery_set() {
        let root = TestDirectory::new("future-delete-debt");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let trash = paths.trash.join(&id);
        let upload = paths.upload_path(&TransactionId::new());
        tokio::fs::write(&trash, b"deleted data").await.unwrap();
        tokio::fs::write(&upload, b"pending upload").await.unwrap();
        let record = paths.deletions.join(format!("{id}.json"));
        let bytes = serde_json::to_vec(&DeletionJournal {
            version: 2,
            id,
            bytes_upper_bound: 12,
            created_unix: 1,
        })
        .unwrap();
        tokio::fs::write(&record, &bytes).await.unwrap();

        drop(paths);
        assert!(TransactionPaths::initialize(root.path()).await.is_err());
        assert_eq!(tokio::fs::read(&record).await.unwrap(), bytes);
        assert_eq!(tokio::fs::read(&trash).await.unwrap(), b"deleted data");
        assert_eq!(tokio::fs::read(&upload).await.unwrap(), b"pending upload");
    }

    #[tokio::test]
    async fn incomplete_deletion_record_is_online_cleanup_work_not_a_global_blocker() {
        let root = TestDirectory::new("incomplete-delete-debt");
        let paths = TransactionPaths::initialize(root.path()).await.unwrap();
        let id = TransactionId::new();
        let trash = paths.trash.join(&id);
        let record = paths.deletions.join(format!("{id}.tmp"));
        tokio::fs::write(&trash, b"deleted data").await.unwrap();
        tokio::fs::write(&record, b"incomplete").await.unwrap();

        let (staged, more) = paths.staged_deletions().await.unwrap();
        assert!(!more);
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].path, trash);
        assert_eq!(staged[0].bytes_upper_bound, None);
        assert_eq!(staged[0].record.as_ref(), Some(&record));
        assert!(remove_any_bounded(&trash, 1).await.unwrap().complete);
        paths.finish_staged_deletion(&staged[0]).await.unwrap();
        assert!(!record.exists());
        assert!(paths.staged_deletions().await.unwrap().0.is_empty());
    }

    #[tokio::test]
    async fn startup_recovers_every_persisted_replacement_stage() {
        let root = std::env::temp_dir().join(format!(
            "ycloud-transaction-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let paths = TransactionPaths::initialize(&root).await.unwrap();

        // Upload completed before a journal was published: safe to discard.
        tokio::fs::write(paths.uploads.join(TransactionId::new()), b"partial")
            .await
            .unwrap();

        // Old destination was moved aside, but the new file was not published.
        let restore_id = TransactionId::new();
        tokio::fs::write(paths.upload_path(&restore_id), b"unpublished")
            .await
            .unwrap();
        tokio::fs::write(paths.backups.join(&restore_id), b"old")
            .await
            .unwrap();
        tokio::fs::write(
            paths.journals.join(format!("{restore_id}.json")),
            serde_json::to_vec(&ReplaceJournal {
                version: 1,
                id: restore_id,
                destination: "restored.txt".into(),
                published: false,
                operation_id: None,
                operation_size: None,
                anchored: false,
            })
            .unwrap(),
        )
        .await
        .unwrap();

        // New destination was published, while old backup and journal remain.
        let committed_id = TransactionId::new();
        tokio::fs::write(root.join("committed.txt"), b"new")
            .await
            .unwrap();
        tokio::fs::write(paths.backups.join(&committed_id), b"old")
            .await
            .unwrap();
        tokio::fs::write(
            paths.journals.join(format!("{committed_id}.json")),
            serde_json::to_vec(&ReplaceJournal {
                version: 1,
                id: committed_id,
                destination: "committed.txt".into(),
                published: false,
                operation_id: None,
                operation_size: None,
                anchored: false,
            })
            .unwrap(),
        )
        .await
        .unwrap();

        // A staged delete was acknowledged but physical cleanup was interrupted.
        tokio::fs::write(paths.trash.join(TransactionId::new()), b"deleted")
            .await
            .unwrap();

        drop(paths);
        TransactionPaths::initialize(&root).await.unwrap();
        TransactionPaths::initialize(&root).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.join("restored.txt")).await.unwrap(),
            b"old"
        );
        assert_eq!(
            tokio::fs::read(root.join("committed.txt")).await.unwrap(),
            b"new"
        );
        for child in ["uploads", "backups", "trash", "transactions", "deletions"] {
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
        #[cfg(unix)]
        let mode_before = {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::metadata(root.join(SYSTEM_DIR))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert!(TransactionPaths::initialize(&root).await.is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode_after = tokio::fs::metadata(root.join(SYSTEM_DIR))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode_after, mode_before);
        }
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn initialization_remains_bound_when_root_path_is_replaced() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("storage");
        let moved_root = parent.path().join("storage-moved");
        tokio::fs::create_dir(&root).await.unwrap();
        let linux_root = super::LinuxRoot::open(&root).unwrap();

        tokio::fs::rename(&root, &moved_root).await.unwrap();
        tokio::fs::create_dir(&root).await.unwrap();
        let paths = TransactionPaths::initialize_linux_with_root(&root, linux_root)
            .await
            .unwrap();

        assert!(moved_root.join(SYSTEM_DIR).join("marker").is_file());
        assert!(!root.join(SYSTEM_DIR).exists());
        assert_eq!(
            tokio::fs::read_to_string(moved_root.join(SYSTEM_DIR).join("marker"))
                .await
                .unwrap(),
            super::MARKER
        );
        drop(paths);
    }
}
