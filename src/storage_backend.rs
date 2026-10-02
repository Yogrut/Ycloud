use axum::{body::Body, http::HeaderMap, response::Response};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Weak,
};

use tokio::{sync::Notify, sync::RwLock};

use crate::{
    capacity::{load_capacity_ledger, CapacityStatus, CapacityTracker},
    directory_listing::{
        page_from_snapshot, prepare_snapshot, DirectoryListRequest, DirectoryPage,
    },
    directory_snapshot::{
        DirectorySnapshotCandidate, DirectorySnapshotCandidateResult, DirectorySnapshotKey,
        DirectorySnapshotStore, SnapshotClaim,
    },
    error::{AppError, AppResult},
    s3_backend::S3Backend,
    storage::{FileResponseMode, LocalDirectoryEntry, StorageService},
};

pub use crate::directory_listing::BackendEntry;

mod lifecycle;
pub use lifecycle::StorageEditGuard;
use lifecycle::{interruptible_body, AdmissionLease, MAX_MUTATION_OWNERS};
#[cfg(test)]
mod copy_tests;
mod maintenance;
pub(crate) use maintenance::spawn_s3_recovery_reconciler;
use maintenance::{
    reconcile_local_capacity, schedule_s3_capacity_reconcile, spawn_local_capacity_reconciler,
    RETRY_MIN,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendMetadata {
    pub is_dir: bool,
    pub size: u64,
    pub modified_unix: Option<i64>,
    pub content_type: Option<String>,
    pub version_tag: Option<String>,
}

/// One live storage boundary shared by every HTTP, WebDAV and archive entry
/// point. Its identity is immutable after registration.
#[derive(Clone)]
pub struct StorageBackend {
    active: Arc<ActiveStorage>,
    lease: Option<Arc<AdmissionLease>>,
    transfer: Option<tokio_util::sync::CancellationToken>,
}

/// Routes every storage operation through an immutable storage identity.
///
/// Persisted configuration and live instances are separate: replacing a
/// registration does not rebind handles retained by existing operations.
#[derive(Clone, Default)]
pub struct StorageRegistry {
    entries: Arc<RwLock<HashMap<String, RegisteredStorage>>>,
}

#[derive(Clone)]
enum RegisteredStorage {
    Ready(StorageBackend),
    Disabled(StorageBackend),
    Unavailable,
}

impl RegisteredStorage {
    fn cached_backend(&self) -> Option<&StorageBackend> {
        match self {
            Self::Ready(backend) | Self::Disabled(backend) => Some(backend),
            Self::Unavailable => None,
        }
    }
}

impl StorageRegistry {
    pub(crate) async fn cached_backends(&self) -> Vec<(String, StorageBackend)> {
        self.entries
            .read()
            .await
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .cached_backend()
                    .map(|backend| (id.clone(), backend.clone()))
            })
            .collect()
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn single(storage_id: impl Into<String>, backend: StorageBackend) -> Self {
        let mut entries = HashMap::new();
        entries.insert(storage_id.into(), RegisteredStorage::Ready(backend));
        Self {
            entries: Arc::new(RwLock::new(entries)),
        }
    }

    pub async fn insert_ready(&self, storage_id: impl Into<String>, backend: StorageBackend) {
        self.entries
            .write()
            .await
            .insert(storage_id.into(), RegisteredStorage::Ready(backend));
    }

    pub async fn insert_unavailable(&self, storage_id: impl Into<String>) {
        self.entries
            .write()
            .await
            .insert(storage_id.into(), RegisteredStorage::Unavailable);
    }

    pub async fn remove(&self, storage_id: &str) -> bool {
        self.entries.write().await.remove(storage_id).is_some()
    }

    pub async fn get(&self, storage_id: &str) -> AppResult<StorageBackend> {
        match self.entries.read().await.get(storage_id) {
            Some(RegisteredStorage::Ready(backend)) => backend.admitted(),
            Some(RegisteredStorage::Disabled(_)) => Err(AppError::Forbidden),
            Some(RegisteredStorage::Unavailable) => Err(AppError::ServiceUnavailable(
                "存储实例当前不可用，请检查连接后重试".into(),
            )),
            None => Err(AppError::NotFound),
        }
    }

    pub async fn cached(&self, storage_id: &str) -> Option<StorageBackend> {
        self.entries
            .read()
            .await
            .get(storage_id)
            .and_then(RegisteredStorage::cached_backend)
            .cloned()
    }

    pub async fn set_enabled(&self, storage_id: &str, enabled: bool) {
        let mut entries = self.entries.write().await;
        if let Some(backend) = entries
            .get(storage_id)
            .and_then(RegisteredStorage::cached_backend)
            .cloned()
        {
            entries.insert(
                storage_id.to_owned(),
                if enabled {
                    RegisteredStorage::Ready(backend)
                } else {
                    RegisteredStorage::Disabled(backend)
                },
            );
        }
    }

    pub async fn contains(&self, storage_id: &str) -> bool {
        self.entries.read().await.contains_key(storage_id)
    }

    pub async fn is_ready(&self, storage_id: &str) -> bool {
        matches!(
            self.entries.read().await.get(storage_id),
            Some(RegisteredStorage::Ready(backend))
                if !backend.interruption_pending() && !backend.active.retired.load(Ordering::Acquire)
        )
    }

    pub async fn set_local_max_upload_bytes(&self, max_upload_bytes: u64) {
        let backends = self
            .entries
            .read()
            .await
            .values()
            .filter_map(RegisteredStorage::cached_backend)
            .cloned()
            .collect::<Vec<_>>();
        for backend in backends {
            backend.set_local_max_upload_bytes(max_upload_bytes);
        }
    }

    pub async fn reconcile_capacities(&self) {
        let backends = self
            .entries
            .read()
            .await
            .values()
            .filter_map(RegisteredStorage::cached_backend)
            .cloned()
            .collect::<Vec<_>>();
        for backend in backends {
            backend.reconcile_capacity().await;
        }
    }
}

#[derive(Clone)]
enum StorageBackendKind {
    Local(StorageService),
    S3(S3Backend),
}

struct ActiveStorage {
    kind: StorageBackendKind,
    capacity: CapacityTracker,
    local_reconcile_wake: Option<Arc<Notify>>,
    directory_snapshots: DirectorySnapshotStore,
    lifecycle: Arc<RwLock<()>>,
    retired: AtomicBool,
    mutation_owners: Arc<tokio::sync::Semaphore>,
    editing: AtomicBool,
    transfer: std::sync::Mutex<tokio_util::sync::CancellationToken>,
    leases: std::sync::Mutex<Vec<Weak<AdmissionLease>>>,
    health: std::sync::Mutex<(bool, i64)>,
}

impl ActiveStorage {
    fn new(
        kind: StorageBackendKind,
        capacity: CapacityTracker,
        local_reconcile_wake: Option<Arc<Notify>>,
    ) -> Self {
        Self {
            kind,
            capacity,
            local_reconcile_wake,
            directory_snapshots: DirectorySnapshotStore::new(),
            lifecycle: Arc::new(RwLock::new(())),
            retired: AtomicBool::new(false),
            mutation_owners: Arc::new(tokio::sync::Semaphore::new(MAX_MUTATION_OWNERS)),
            editing: AtomicBool::new(false),
            transfer: std::sync::Mutex::new(tokio_util::sync::CancellationToken::new()),
            leases: std::sync::Mutex::new(Vec::new()),
            health: std::sync::Mutex::new((true, chrono::Utc::now().timestamp())),
        }
    }
}

/// Cancellation or a task panic must not leave a possibly published S3
/// mutation advertised as accurately accounted for.
struct S3CapacityAccounting {
    capacity: CapacityTracker,
    storage: S3Backend,
    settled: bool,
}

impl S3CapacityAccounting {
    fn new(capacity: &CapacityTracker, storage: &S3Backend) -> Self {
        Self {
            capacity: capacity.clone(),
            storage: storage.clone(),
            settled: false,
        }
    }
}

impl Drop for S3CapacityAccounting {
    fn drop(&mut self) {
        if !self.settled {
            schedule_s3_capacity_reconcile(self.capacity.clone(), self.storage.clone());
        }
    }
}

impl Drop for ActiveStorage {
    fn drop(&mut self) {
        if let Some(wake) = &self.local_reconcile_wake {
            // `notify_one` stores a permit when the worker has not reached its
            // wait yet, so a backend created and dropped in one scheduler turn
            // cannot leave a detached task asleep forever.
            wake.notify_one();
        }
        if let StorageBackendKind::S3(storage) = &self.kind {
            storage.stop_recovery_worker();
        }
    }
}

impl StorageBackend {
    pub fn health_status(&self) -> (bool, i64) {
        *self
            .active
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn instance_key(&self) -> usize {
        Arc::as_ptr(&self.active) as usize
    }

    fn observe_result<T>(&self, result: &AppResult<T>) {
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|error| error.status().is_server_error())
        {
            *self
                .active
                .health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                (result.is_ok(), chrono::Utc::now().timestamp());
        }
    }

    pub async fn check_health(&self) {
        if self.active.editing.load(Ordering::Acquire)
            || self.active.retired.load(Ordering::Acquire)
        {
            return;
        }
        let cancellation = self.transfer_token();
        let check = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            match &self.active.kind {
                StorageBackendKind::Local(storage) => {
                    let path = storage.resolve_existing("").await?;
                    storage.metadata(&path).await.map(|_| ())
                }
                StorageBackendKind::S3(storage) => storage.check_health().await,
            }
        });
        let result = tokio::select! {
            _ = cancellation.cancelled() => return,
            result = check => result.unwrap_or_else(|_| Err(AppError::ServiceUnavailable("存储连接检查超时".into()))),
        };
        *self
            .active
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            (result.is_ok(), chrono::Utc::now().timestamp());
    }
    pub fn set_capacity_limit(&self, limit: Option<u64>) {
        self.active.capacity.set_limit(limit);
    }

    /// Hold exclusive admission while settling abandoned uploads.
    /// No live upload may be adopted as abandoned recovery work.
    pub(crate) async fn recover_abandoned_uploads_before(
        &self,
        deadline: Option<tokio::time::Instant>,
        stop: Option<tokio_util::sync::CancellationToken>,
    ) -> AppResult<StorageEditGuard> {
        let guard = self.edit_guard().await?;
        match &self.active.kind {
            StorageBackendKind::S3(storage) => {
                storage
                    .scoped_work(deadline, stop)
                    .recover_quiesced_uploads()
                    .await?;
                self.active.capacity.mark_uncertain();
            }
            StorageBackendKind::Local(storage) => {
                storage.recover_quiesced_uploads().await?;
                self.active.capacity.mark_uncertain();
            }
        }
        Ok(guard)
    }

    pub(crate) async fn recovered_upload_committed(
        &self,
        path: &str,
        size: u64,
        operation_id: &str,
    ) -> AppResult<bool> {
        match &self.active.kind {
            StorageBackendKind::S3(storage) => {
                storage.upload_committed(path, size, operation_id).await
            }
            StorageBackendKind::Local(storage) => {
                storage.upload_committed(path, size, operation_id).await
            }
        }
    }
    pub(crate) async fn prune_upload_receipts(
        &self,
        retained: &std::collections::HashSet<String>,
    ) -> AppResult<()> {
        if let StorageBackendKind::Local(storage) = &self.active.kind {
            storage.prune_upload_receipts(retained).await?;
        }
        Ok(())
    }
    pub(crate) async fn prune_upload_receipt(&self, operation_id: &str) -> AppResult<()> {
        if let StorageBackendKind::Local(storage) = &self.active.kind {
            storage.prune_upload_receipt(operation_id).await?;
        }
        Ok(())
    }
    pub(crate) fn set_local_max_upload_bytes(&self, max_upload_bytes: u64) {
        let active = &self.active;
        if let StorageBackendKind::Local(storage) = &active.kind {
            storage.set_max_upload_bytes(max_upload_bytes);
        }
    }

    pub fn local(storage: StorageService) -> Self {
        Self {
            lease: None,
            transfer: None,
            active: Arc::new(ActiveStorage::new(
                StorageBackendKind::Local(storage),
                CapacityTracker::new(None, 0),
                None,
            )),
        }
    }

    pub async fn local_configured(
        storage: StorageService,
        capacity_limit: Option<u64>,
        ledger_path: PathBuf,
    ) -> AppResult<Self> {
        let used = storage.user_data_size().await?;
        let reconcile_wake = Arc::new(Notify::new());
        let capacity =
            CapacityTracker::new_with_ledger(capacity_limit, used, true, Some(ledger_path));
        capacity.set_reconcile_wake(&reconcile_wake);
        let backend = Self {
            lease: None,
            transfer: None,
            active: Arc::new(ActiveStorage::new(
                StorageBackendKind::Local(storage),
                capacity,
                Some(reconcile_wake.clone()),
            )),
        };
        let _worker = spawn_local_capacity_reconciler(
            Arc::downgrade(&backend.active),
            reconcile_wake,
            RETRY_MIN,
        );
        persist_capacity(&backend.active.capacity).await;
        Ok(backend)
    }

    pub async fn s3_configured(
        storage: S3Backend,
        capacity_limit: Option<u64>,
        ledger_path: PathBuf,
    ) -> AppResult<Self> {
        let persisted = load_capacity_ledger(&ledger_path).await?;
        // A persisted S3 ledger is only a last-known value. Objects may have
        // changed outside Ycloud while it was stopped, so never advertise it
        // as current before an online reconciliation finishes.
        let used = persisted.unwrap_or(0);
        let backend = Self {
            lease: None,
            transfer: None,
            active: Arc::new(ActiveStorage::new(
                StorageBackendKind::S3(storage),
                CapacityTracker::new_with_ledger(capacity_limit, used, false, Some(ledger_path)),
                None,
            )),
        };
        let active = &backend.active;
        if let StorageBackendKind::S3(storage) = &active.kind {
            schedule_s3_capacity_reconcile(active.capacity.clone(), storage.clone());
            spawn_s3_recovery_reconciler(storage.clone(), active.capacity.clone());
        }
        Ok(backend)
    }

    pub async fn metadata(&self, relative: &str) -> AppResult<BackendMetadata> {
        let result = self.metadata_inner(relative).await;
        self.observe_result(&result);
        result
    }

    async fn metadata_inner(&self, relative: &str) -> AppResult<BackendMetadata> {
        let active = &self.active;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                let metadata = storage.metadata(&path).await?;
                Ok(BackendMetadata {
                    is_dir: metadata.is_dir(),
                    size: metadata.len(),
                    modified_unix: metadata.modified().ok().map(|value| {
                        let value: chrono::DateTime<chrono::Utc> = value.into();
                        value.timestamp()
                    }),
                    content_type: None,
                    version_tag: None,
                })
            }
            StorageBackendKind::S3(storage) => {
                let metadata = storage.metadata(relative).await?;
                Ok(BackendMetadata {
                    is_dir: metadata.is_dir,
                    size: metadata.size,
                    modified_unix: metadata.last_modified,
                    content_type: metadata.content_type,
                    version_tag: metadata.etag,
                })
            }
        }
    }

    pub async fn list_directory(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<(Vec<BackendEntry>, bool)> {
        let result = self.list_directory_inner(relative, max_entries).await;
        self.observe_result(&result);
        result
    }

    async fn list_directory_inner(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<(Vec<BackendEntry>, bool)> {
        let active = &self.active;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                list_local_directory(storage, relative, max_entries).await
            }
            StorageBackendKind::S3(storage) => {
                let result = storage.list_directory(relative, max_entries).await?;
                Ok((
                    result
                        .entries
                        .into_iter()
                        .map(|entry| BackendEntry {
                            name: entry.name,
                            relative: entry.relative,
                            is_dir: entry.is_dir,
                            size: entry.size,
                            modified_unix: entry.last_modified,
                        })
                        .collect(),
                    result.truncated,
                ))
            }
        }
    }

    pub(crate) async fn directory_size(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<u64> {
        match &self.active.kind {
            StorageBackendKind::Local(storage) => {
                storage.directory_size(relative, max_entries).await
            }
            StorageBackendKind::S3(storage) => storage.directory_size(relative, max_entries).await,
        }
    }

    pub async fn list_directory_page(
        &self,
        relative: &str,
        request: DirectoryListRequest,
    ) -> AppResult<DirectoryPage> {
        let result = self.list_directory_page_inner(relative, request).await;
        self.observe_result(&result);
        result
    }

    async fn list_directory_page_inner(
        &self,
        relative: &str,
        request: DirectoryListRequest,
    ) -> AppResult<DirectoryPage> {
        let active = &self.active;
        let key = DirectorySnapshotKey::new(relative, &request);
        loop {
            match active.directory_snapshots.claim(key.clone()) {
                SnapshotClaim::Ready(entries) => {
                    return Ok(page_from_snapshot(&entries, &request));
                }
                SnapshotClaim::Wait(mut completed) => {
                    let pending = !*completed.borrow();
                    if pending {
                        let _ = completed.changed().await;
                    }
                }
                SnapshotClaim::Build(build) => {
                    let _permit = active.directory_snapshots.acquire_build_permit().await;
                    let mut candidate = DirectorySnapshotCandidate::new(request.clone());
                    match &active.kind {
                        StorageBackendKind::Local(storage) => {
                            scan_local_directory(storage, relative, |entry| {
                                candidate.consider(entry)
                            })
                            .await?;
                        }
                        StorageBackendKind::S3(storage) => {
                            storage
                                .scan_directory_entries(relative, |entry| {
                                    candidate.consider(BackendEntry {
                                        name: entry.name,
                                        relative: entry.relative,
                                        is_dir: entry.is_dir,
                                        size: entry.size,
                                        modified_unix: entry.last_modified,
                                    });
                                })
                                .await?;
                        }
                    }
                    let entries = match candidate.finish() {
                        DirectorySnapshotCandidateResult::Snapshot(entries) => entries,
                        DirectorySnapshotCandidateResult::Page(page) => return Ok(page),
                    };
                    let entries =
                        Arc::new(prepare_snapshot(entries, None, key.sort(), key.direction()));
                    let page = page_from_snapshot(&entries, &request);
                    build.publish(entries);
                    return Ok(page);
                }
            }
        }
    }

    pub async fn stream_file(
        &self,
        relative: &str,
        headers: &HeaderMap,
        mode: FileResponseMode,
    ) -> AppResult<Response> {
        let active = &self.active;
        let response = match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                storage.stream_file(&path, headers, mode).await
            }
            StorageBackendKind::S3(storage) => storage.stream_file(relative, headers, mode).await,
        };
        self.observe_result(&response);
        let (parts, body) = response?.into_parts();
        Ok(Response::from_parts(
            parts,
            interruptible_body(body, self.transfer_token()),
        ))
    }

    pub async fn upload_file(
        &self,
        relative: &str,
        body: Body,
        expected_bytes: Option<u64>,
        max_upload_bytes: u64,
        content_type: Option<&str>,
    ) -> AppResult<u64> {
        self.upload_file_mode(
            relative,
            crate::s3_backend::UploadInput::relay(body),
            expected_bytes,
            max_upload_bytes,
            content_type,
            false,
        )
        .await
    }

    /// Browser uploads create a new file; a late name collision must not replace user data.
    pub async fn upload_new_file(
        &self,
        relative: &str,
        body: Body,
        expected_bytes: Option<u64>,
        max_upload_bytes: u64,
        content_type: Option<&str>,
    ) -> AppResult<u64> {
        self.upload_file_mode(
            relative,
            crate::s3_backend::UploadInput::relay(body),
            expected_bytes,
            max_upload_bytes,
            content_type,
            true,
        )
        .await
    }

    pub(crate) async fn upload_tracked_new_file(
        &self,
        relative: &str,
        input: crate::s3_backend::UploadInput,
        expected_bytes: Option<u64>,
        maximum: u64,
        content_type: Option<&str>,
    ) -> AppResult<u64> {
        self.upload_file_mode(relative, input, expected_bytes, maximum, content_type, true)
            .await
    }

    pub(crate) fn supports_direct_upload(&self) -> bool {
        matches!(&self.active.kind, StorageBackendKind::S3(_))
    }

    pub(crate) async fn upload_direct_file(
        &self,
        relative: &str,
        size: u64,
        maximum: u64,
        channel: crate::s3_backend::DirectChannel,
        operation_id: String,
    ) -> AppResult<u64> {
        self.upload_file_mode(
            relative,
            crate::s3_backend::UploadInput {
                body: Body::empty(),
                direct: Some(channel),
                operation_id: Some(operation_id),
                cancellation: None,
                commit_owner: None,
            },
            Some(size),
            maximum,
            Some("application/octet-stream"),
            true,
        )
        .await
    }

    async fn upload_file_mode(
        &self,
        relative: &str,
        mut input: crate::s3_backend::UploadInput,
        expected_bytes: Option<u64>,
        max_upload_bytes: u64,
        content_type: Option<&str>,
        create_only: bool,
    ) -> AppResult<u64> {
        let cancellation = self.transfer_token();
        input.body = interruptible_body(input.body, cancellation.clone());
        input.cancellation = Some(cancellation);
        let (relative, content_type) = (relative.to_owned(), content_type.map(str::to_owned));
        self.owned_mutation(move |backend| async move {
            backend
                .upload_file_mode_inner(
                    &relative,
                    input,
                    expected_bytes,
                    max_upload_bytes,
                    content_type.as_deref(),
                    create_only,
                )
                .await
        })
        .await
    }

    async fn upload_file_mode_inner(
        &self,
        relative: &str,
        mut input: crate::s3_backend::UploadInput,
        expected_bytes: Option<u64>,
        max_upload_bytes: u64,
        content_type: Option<&str>,
        create_only: bool,
    ) -> AppResult<u64> {
        let active = &self.active;
        let _snapshot_invalidation = active.directory_snapshots.invalidate_on_drop();
        let capacity = active.capacity.clone();
        if expected_bytes.is_some_and(|bytes| bytes > max_upload_bytes) {
            return Err(AppError::PayloadTooLarge);
        }
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let destination = storage.resolve_for_write(relative).await?;
                let old_size = match storage.metadata(&destination).await {
                    Ok(_) if create_only => {
                        return Err(AppError::Conflict(
                            "上传目标已被占用，请重新上传以分配新编号".into(),
                        ))
                    }
                    Ok(metadata) if metadata.is_file() => metadata.len(),
                    Ok(_) => return Err(AppError::Conflict("不能用文件覆盖目录".into())),
                    Err(AppError::NotFound) => 0,
                    Err(error) => return Err(error),
                };
                let mut capacity_reservation =
                    capacity.reserve_replacement(old_size, expected_bytes.unwrap_or(0))?;
                let mut writer = match expected_bytes {
                    Some(bytes) => {
                        storage
                            .begin_atomic_write_with_expected(relative, bytes)
                            .await?
                    }
                    None => storage.begin_atomic_write(relative).await?,
                };
                if let Some(operation_id) = &input.operation_id {
                    writer.set_operation_id(operation_id.clone());
                }
                let mut stream = input.body.into_data_stream();
                let mut received = 0_u64;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| {
                        if self.transfer_token().is_cancelled() {
                            AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                        } else {
                            AppError::ClientClosedRequest
                        }
                    })?;
                    received = received
                        .checked_add(chunk.len() as u64)
                        .ok_or(AppError::PayloadTooLarge)?;
                    capacity_reservation.ensure_new_size(received)?;
                    writer.write_chunk(&chunk).await?;
                }
                let committed = if create_only {
                    writer
                        .commit_new_with_capacity(capacity_reservation)
                        .await?
                } else {
                    writer.commit_with_capacity(capacity_reservation).await?
                };
                Ok(committed.size)
            }
            StorageBackendKind::S3(storage) => {
                let content_length = expected_bytes.ok_or_else(|| {
                    AppError::BadRequest(
                        "对象存储上传需要有效的 Content-Length，不能使用未知长度请求体".into(),
                    )
                })?;
                let old_size = match storage.metadata(relative).await {
                    Ok(_) if create_only => {
                        return Err(AppError::Conflict(
                            "上传目标已被占用，请重新上传以分配新编号".into(),
                        ))
                    }
                    Ok(metadata) if !metadata.is_dir => metadata.size,
                    Ok(_) => return Err(AppError::Conflict("不能用文件覆盖目录".into())),
                    Err(AppError::NotFound) => 0,
                    Err(error) => return Err(error),
                };
                let capacity_reservation =
                    capacity.reserve_replacement(old_size, content_length)?;
                let storage = storage.clone();
                let relative = relative.to_owned();
                let content_type = content_type.map(str::to_owned);
                let failure_capacity = capacity.clone();
                let failure_storage = storage.clone();
                let snapshots = active.directory_snapshots.clone();
                tokio::spawn(async move {
                    let _snapshot_invalidation = snapshots.invalidate_on_drop();
                    let commit_owner = if input.direct.is_some() {
                        Some(Arc::new(tokio::sync::Mutex::new(None)))
                    } else {
                        None
                    };
                    input.commit_owner = commit_owner.clone();
                    let _accounting = if commit_owner.is_none() {
                        Some(storage.acquire_capacity_mutation().await)
                    } else {
                        None
                    };
                    let mut accounting = S3CapacityAccounting::new(&capacity, &storage);
                    let result = storage
                        .upload_file_mode(
                            &relative,
                            input,
                            content_length,
                            max_upload_bytes,
                            content_type.as_deref(),
                            create_only,
                        )
                        .await;
                    match result {
                        Ok(result) => {
                            capacity_reservation.commit(result.previous_size, result.size);
                            persist_capacity(&capacity).await;
                            accounting.settled = true;
                            Ok(result.size)
                        }
                        Err(error) => {
                            drop(capacity_reservation);
                            schedule_s3_capacity_reconcile(capacity, storage);
                            if error.operation().is_none() {
                                Err(error.with_operation(
                                    crate::error::CommitState::Unknown,
                                    crate::error::CleanupState::Unknown,
                                ))
                            } else {
                                Err(error)
                            }
                        }
                    }
                })
                .await
                .map_err(|error| {
                    schedule_s3_capacity_reconcile(failure_capacity, failure_storage);
                    AppError::with_source("S3 upload execution task failed", error)
                })?
            }
        }
    }

    pub async fn create_directory(&self, relative: &str) -> AppResult<()> {
        let relative = relative.to_owned();
        self.owned_mutation(move |backend| async move {
            backend.create_directory_inner(&relative).await
        })
        .await
    }

    async fn create_directory_inner(&self, relative: &str) -> AppResult<()> {
        let active = &self.active;
        let _snapshot_invalidation = active.directory_snapshots.invalidate_on_drop();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_for_write(relative).await?;
                storage.create_directory(&path).await
            }
            StorageBackendKind::S3(storage) => storage.create_directory(relative).await,
        }
    }

    pub async fn remove(&self, relative: &str) -> AppResult<()> {
        let relative = relative.to_owned();
        self.owned_mutation(move |backend| async move { backend.remove_inner(&relative).await })
            .await
    }

    async fn remove_inner(&self, relative: &str) -> AppResult<()> {
        let active = &self.active;
        let _snapshot_invalidation = active.directory_snapshots.invalidate_on_drop();
        let capacity = active.capacity.clone();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let path = storage.resolve_existing(relative).await?;
                storage
                    .remove_with_capacity(&path, Some(capacity))
                    .await
                    .map(|_| ())
            }
            StorageBackendKind::S3(storage) => {
                let _accounting = storage.acquire_capacity_mutation().await;
                let mut accounting = S3CapacityAccounting::new(&capacity, storage);
                let metadata = storage.metadata(relative).await?;
                let removal = if metadata.is_dir {
                    storage.delete_directory(relative).await
                } else {
                    storage.delete_file(relative).await
                };
                let removed_size = match removal {
                    Ok(removed_size) => removed_size,
                    Err(error) => {
                        schedule_s3_capacity_reconcile(capacity.clone(), storage.clone());
                        return Err(error);
                    }
                };
                capacity.remove_used(removed_size);
                persist_capacity(&capacity).await;
                accounting.settled = true;
                Ok(())
            }
        }
    }

    pub async fn move_path(&self, source: &str, destination: &str) -> AppResult<()> {
        let (source, destination) = (source.to_owned(), destination.to_owned());
        self.owned_mutation(move |backend| async move {
            backend.move_path_inner(&source, &destination).await
        })
        .await
    }

    async fn move_path_inner(&self, source: &str, destination: &str) -> AppResult<()> {
        let active = &self.active;
        let _snapshot_invalidation = active.directory_snapshots.invalidate_on_drop();
        let capacity = active.capacity.clone();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let source = storage.resolve_existing(source).await?;
                let destination = storage.resolve_for_write(destination).await?;
                let result = storage.move_path(&source, &destination).await;
                if result.is_err() {
                    let _ = reconcile_local_capacity(&capacity, storage).await;
                }
                result
            }
            StorageBackendKind::S3(storage) => {
                let _accounting = storage.acquire_capacity_mutation().await;
                let mut accounting = S3CapacityAccounting::new(&capacity, storage);
                let result = if storage.metadata(source).await?.is_dir {
                    storage.move_directory(source, destination).await
                } else {
                    storage.move_file(source, destination).await
                };
                if result.is_err() {
                    schedule_s3_capacity_reconcile(capacity.clone(), storage.clone());
                } else {
                    accounting.settled = true;
                }
                result
            }
        }
    }

    pub async fn copy_path(&self, source: &str, destination: &str) -> AppResult<()> {
        let (source, destination) = (source.to_owned(), destination.to_owned());
        self.owned_mutation(move |backend| async move {
            backend.copy_path_inner(&source, &destination).await
        })
        .await
    }

    async fn copy_path_inner(&self, source: &str, destination: &str) -> AppResult<()> {
        let active = &self.active;
        let _snapshot_invalidation = active.directory_snapshots.invalidate_on_drop();
        let capacity = active.capacity.clone();
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                let source = storage.resolve_existing(source).await?;
                let destination = storage.resolve_for_write(destination).await?;
                let copied_size = storage.path_size(&source).await?;
                storage
                    .copy_path_with_capacity(&source, &destination, copied_size, capacity)
                    .await
            }
            StorageBackendKind::S3(storage) => {
                let copied_size = storage.path_size(source).await?;
                let reservation = capacity.reserve_replacement(0, copied_size)?;
                // `copy_path` already owns independent execution and snapshot
                // invalidation. Keep quota and remote I/O in that same owner;
                // the accounting guard reconciles errors or panic on exit.
                let _accounting = storage.acquire_capacity_mutation().await;
                let mut accounting = S3CapacityAccounting::new(&capacity, storage);
                if storage.metadata(source).await?.is_dir {
                    storage
                        .copy_directory_with_expected_size(source, destination, copied_size)
                        .await?;
                } else {
                    storage
                        .copy_file_with_expected_size(source, destination, copied_size)
                        .await?;
                }
                reservation.commit(0, copied_size);
                persist_capacity(&capacity).await;
                accounting.settled = true;
                Ok(())
            }
        }
    }

    pub fn capacity_status(&self) -> CapacityStatus {
        self.active.capacity.status()
    }

    pub fn local_cleanup_status(&self) -> Option<crate::storage::CleanupStatus> {
        match &self.active.kind {
            StorageBackendKind::Local(storage) => Some(storage.cleanup_status()),
            StorageBackendKind::S3(_) => None,
        }
    }

    pub fn local_staging_cleanup_status(&self) -> Option<crate::storage::UploadCleanupStatus> {
        match &self.active.kind {
            StorageBackendKind::Local(storage) => Some(storage.upload_cleanup_status()),
            StorageBackendKind::S3(_) => None,
        }
    }

    pub fn s3_recovery_status(&self) -> Option<crate::s3_backend::S3RecoveryStatus> {
        match &self.active.kind {
            StorageBackendKind::Local(_) => None,
            StorageBackendKind::S3(storage) => Some(storage.recovery_status()),
        }
    }

    pub async fn ready(&self) -> bool {
        let active = &self.active;
        match &active.kind {
            StorageBackendKind::Local(storage) => storage.ready().await,
            StorageBackendKind::S3(storage) => storage.probe().await.is_ok(),
        }
    }

    pub async fn reconcile_capacity(&self) {
        let Ok(_lease) = self.active.lifecycle.clone().try_read_owned() else {
            return;
        };
        if self.active.retired.load(Ordering::Acquire) {
            return;
        }
        let active = &self.active;
        match &active.kind {
            StorageBackendKind::Local(storage) => {
                active.capacity.mark_uncertain();
                let _ = reconcile_local_capacity(&active.capacity, storage).await;
            }
            StorageBackendKind::S3(storage) => {
                schedule_s3_capacity_reconcile(active.capacity.clone(), storage.clone());
            }
        }
    }
}

async fn persist_capacity(capacity: &CapacityTracker) {
    if let Err(error) = capacity.persist().await {
        capacity.mark_uncertain();
        tracing::error!(%error, "failed to persist storage capacity ledger");
    }
}

async fn list_local_directory(
    storage: &StorageService,
    relative: &str,
    max_entries: usize,
) -> AppResult<(Vec<BackendEntry>, bool)> {
    let directory = storage.resolve_existing(relative).await?;
    if !storage.metadata(&directory).await?.is_dir() {
        return Err(AppError::NotFound);
    }
    let mut entries = Vec::new();
    storage
        .scan_directory(&directory, |entry| {
            if let Some(entry) = local_backend_entry(relative, entry) {
                entries.push(entry);
            }
            Ok(entries.len() <= max_entries)
        })
        .await?;
    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    Ok((entries, truncated))
}

async fn scan_local_directory<F>(
    storage: &StorageService,
    relative: &str,
    mut consume: F,
) -> AppResult<()>
where
    F: FnMut(BackendEntry),
{
    let directory = storage.resolve_existing(relative).await?;
    if !storage.metadata(&directory).await?.is_dir() {
        return Err(AppError::NotFound);
    }
    storage
        .scan_directory(&directory, |entry| {
            if let Some(entry) = local_backend_entry(relative, entry) {
                consume(entry);
            }
            Ok(true)
        })
        .await?;
    Ok(())
}

fn local_backend_entry(relative: &str, entry: LocalDirectoryEntry) -> Option<BackendEntry> {
    let name = entry.name.to_string_lossy().to_string();
    if name.eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR) {
        return None;
    }
    let entry_relative = if relative.is_empty() {
        name.clone()
    } else {
        format!("{}/{}", relative.trim_end_matches('/'), name)
    };
    Some(BackendEntry {
        name,
        relative: entry_relative,
        is_dir: entry.metadata.is_dir(),
        size: entry.metadata.len(),
        modified_unix: entry.metadata.modified().ok().map(|value| {
            let value: chrono::DateTime<chrono::Utc> = value.into();
            value.timestamp()
        }),
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

    use super::{StorageBackend, StorageRegistry};

    use crate::capacity::load_capacity_ledger;
    use crate::directory_listing::{
        DirectoryEntryFilter, DirectoryListRequest, DirectorySort, SortDirection,
    };
    use crate::storage::StorageService;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn fresh_instances_do_not_share_lifecycle_or_capacity_state() {
        let fixture = TestDirectory::new("backend-instance-state");
        let storage = StorageService::new(fixture.path().join("files"), 1024, 1, 100, 0)
            .await
            .unwrap();
        let first = StorageBackend::local(storage.clone());
        let second = StorageBackend::local(storage);
        assert_ne!(first.instance_key(), second.instance_key());
        assert!(!std::sync::Arc::ptr_eq(
            &first.active.lifecycle,
            &second.active.lifecycle,
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &first.active.mutation_owners,
            &second.active.mutation_owners,
        ));
        assert_eq!(
            second.active.mutation_owners.available_permits(),
            super::MAX_MUTATION_OWNERS
        );
        assert!(second.active.local_reconcile_wake.is_none());
        assert!(second.active.leases.lock().unwrap().is_empty());
        assert!(!second.transfer_token().is_cancelled());

        first.set_capacity_limit(Some(2));
        first.active.capacity.mark_uncertain();
        let edit = first.interrupt_for_edit().await.unwrap();
        edit.retire();
        drop(edit);
        assert!(first.admitted().is_err());
        assert!(second.active.capacity.status().accurate);
        assert!(second.active.capacity.status().limit.is_none());
        assert_eq!(second.active.capacity.status().used, 0);
        assert!(!second.transfer_token().is_cancelled());
        second
            .admitted()
            .unwrap()
            .owned_mutation(|_| async { Ok(()) })
            .await
            .unwrap();
    }

    fn directory_request(limit: usize) -> DirectoryListRequest {
        DirectoryListRequest {
            limit,
            search: None,
            sort: DirectorySort::Name,
            direction: SortDirection::Asc,
            filter: DirectoryEntryFilter::All,
            after: None,
        }
    }

    #[tokio::test]
    async fn directory_pages_reuse_a_snapshot_until_a_storage_mutation_invalidates_it() {
        let fixture = TestDirectory::new("directory-snapshot");
        let root = fixture.path().join("files");
        let storage = StorageService::new(root.clone(), 1024, 1, 100, 0)
            .await
            .unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            tokio::fs::write(root.join(name), name).await.unwrap();
        }
        let backend = StorageBackend::local(storage);
        let first = backend
            .list_directory_page("", directory_request(2))
            .await
            .unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["a.txt", "b.txt"]
        );

        tokio::fs::write(root.join("aa.txt"), b"external")
            .await
            .unwrap();
        let cached = backend
            .list_directory_page("", directory_request(2))
            .await
            .unwrap();
        assert_eq!(
            cached
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["a.txt", "b.txt"]
        );

        backend.create_directory("new-dir").await.unwrap();
        let refreshed = backend
            .list_directory_page("", directory_request(20))
            .await
            .unwrap();
        let names = refreshed
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"aa.txt"));
        assert!(names.contains(&"new-dir"));
    }

    #[tokio::test]
    async fn local_capacity_counts_growth_copy_and_delete() {
        let root =
            std::env::temp_dir().join(format!("ycloud-backend-capacity-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 1, 100, 0)
            .await
            .unwrap();
        tokio::fs::write(root.join("existing.bin"), b"1234")
            .await
            .unwrap();
        let backend =
            StorageBackend::local_configured(storage, Some(5), root.join("capacity-ledger.json"))
                .await
                .unwrap();
        let staging = backend.local_staging_cleanup_status().unwrap();
        assert_eq!(staging.pending_uploads, 0);
        assert_eq!(staging.pending_copies, 0);
        assert_eq!(staging.failed_attempts, 0);

        let rejected = backend
            .upload_file("new.bin", Body::from("12"), Some(2), 1024, None)
            .await
            .unwrap_err();
        assert_eq!(
            rejected.status(),
            axum::http::StatusCode::INSUFFICIENT_STORAGE
        );

        backend
            .upload_file("existing.bin", Body::from("12345"), Some(5), 1024, None)
            .await
            .unwrap();
        assert_eq!(backend.capacity_status().used, 5);

        backend.remove("existing.bin").await.unwrap();
        backend
            .upload_file("new.bin", Body::from("12"), Some(2), 1024, None)
            .await
            .unwrap();
        backend.copy_path("new.bin", "copy.bin").await.unwrap();
        assert_eq!(backend.capacity_status().used, 4);
        assert!(backend.copy_path("new.bin", "third.bin").await.is_err());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn local_capacity_uncertainty_is_reconciled_and_persisted_automatically() {
        let fixture = TestDirectory::new("local-capacity-reconcile");
        let root = fixture.path().join("files");
        let ledger_dir = fixture.path().join("ledger");
        let ledger = ledger_dir.join("state.json");
        let storage = StorageService::new(root.clone(), 1024, 1, 100, 0)
            .await
            .unwrap();
        tokio::fs::write(root.join("old.txt"), b"old")
            .await
            .unwrap();
        let backend = StorageBackend::local_configured(storage, Some(100), ledger.clone())
            .await
            .unwrap();

        tokio::fs::remove_file(&ledger).await.unwrap();
        tokio::fs::remove_dir(&ledger_dir).await.unwrap();
        tokio::fs::write(&ledger_dir, b"temporarily unavailable")
            .await
            .unwrap();
        let error = backend
            .upload_file("new.txt", Body::from("note"), Some(4), 1024, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "operation_committed_pending");
        assert!(!backend.capacity_status().accurate);

        tokio::fs::remove_file(&ledger_dir).await.unwrap();
        // The reconciliation worker owns recovery and may recreate this
        // directory immediately after the blocking file is removed.
        tokio::fs::create_dir_all(&ledger_dir).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while !backend.capacity_status().accurate {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("automatic local capacity reconciliation completed");
        assert_eq!(backend.capacity_status().used, 7);
        assert_eq!(load_capacity_ledger(&ledger).await.unwrap(), Some(7));
    }

    #[tokio::test]
    async fn registry_routes_only_registered_storage_ids() {
        let root =
            std::env::temp_dir().join(format!("ycloud-backend-registry-{}", uuid::Uuid::new_v4()));
        let storage = StorageService::new(root.clone(), 1024, 1, 100, 0)
            .await
            .unwrap();
        let backend = StorageBackend::local(storage);
        let registry = StorageRegistry::single("primary", backend).await;

        assert!(registry.contains("primary").await);
        assert!(!registry.contains("unregistered").await);
        registry.get("primary").await.unwrap();
        let error = match registry.get("unregistered").await {
            Ok(_) => panic!("unregistered storage unexpectedly resolved"),
            Err(error) => error,
        };
        assert_eq!(error.status(), axum::http::StatusCode::NOT_FOUND);

        registry.insert_unavailable("unavailable").await;
        assert!(registry.cached("unavailable").await.is_none());
        registry.set_enabled("primary", false).await;
        assert!(!registry.is_ready("primary").await);
        assert!(registry.cached("primary").await.is_some());
        assert_eq!(registry.cached_backends().await.len(), 1);
        assert_eq!(
            registry.get("primary").await.err().unwrap().status(),
            axum::http::StatusCode::FORBIDDEN
        );
        registry.set_enabled("primary", true).await;
        assert!(registry.is_ready("primary").await);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
