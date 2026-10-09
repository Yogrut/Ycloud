use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

mod item;
mod persistence;
use item::{classify_upload_result, UploadItem};
pub use item::{UploadBegin, UploadStatus};
use persistence::BatchPersistence;

use crate::{
    auth::{self, RequestSubject},
    error::{AppError, AppResult, CleanupState, CommitState, OperationOutcome},
    file_access::{
        check_folder_locks, ensure_non_root, ensure_storage_action, resolve_share,
        share_storage_path, FileQuery, StorageAction,
    },
    state::AppState,
    storage::StorageService,
};

const MAX_ACTIVE_BATCHES: usize = 32;
const MAX_ACTIVE_BATCHES_PER_ACCOUNT: usize = 4;
const MAX_ACTIVE_BATCH_ITEMS: usize = 20_000;
const MAX_RETAINED_BATCHES: usize = 128;
const MAX_RETAINED_BATCH_ITEMS: usize = 40_000;
const PENDING_UPLOAD_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct UploadBatchStore {
    batches: Arc<Mutex<HashMap<String, UploadBatch>>>,
    ttl: Duration,
    result_ttl: Duration,
    prepare_gate: Arc<Mutex<()>>,
    // Fences directory planning across administrator storage edits without
    // making those edits wait for a slow local or S3 directory listing.
    invalidation_epoch: Arc<AtomicU64>,
    persistence: Option<Arc<BatchPersistence>>,
    receipt_registry: Arc<std::sync::OnceLock<crate::storage_backend::StorageRegistry>>,
}

struct UploadBatch {
    // Serialize durable transitions within this ticket, not across accounts.
    transition: Arc<Mutex<()>>,
    recovery_backend: Option<crate::storage_backend::StorageBackend>,
    subject: Option<RequestSubject>,
    account: String,
    storage_id: String,
    namespace_id: Option<String>,
    expires_at: Instant,
    expires_unix: i64,
    items: HashMap<String, UploadItem>,
}

/// Request-local counts derived while the batch map is locked. They are never
/// stored beside the map, so later cancellation or completion cannot stale them.
#[derive(Default, Debug, PartialEq, Eq)]
struct BatchUsage {
    retained_batches: usize,
    retained_items: usize,
    active_batches: usize,
    account_batches: usize,
    active_items: usize,
}

impl BatchUsage {
    fn from_batches(batches: &HashMap<String, UploadBatch>, account: &str) -> Self {
        let mut usage = Self {
            retained_batches: batches.len(),
            ..Self::default()
        };
        for batch in batches.values() {
            let active_items = batch
                .items
                .values()
                .filter(|item| !item.status.is_terminal())
                .count();
            usage.retained_items = usage.retained_items.saturating_add(batch.items.len());
            usage.active_items = usage.active_items.saturating_add(active_items);
            if active_items > 0 {
                usage.active_batches += 1;
                if batch.account == account {
                    usage.account_batches += 1;
                }
            }
        }
        usage
    }

    fn requires_retirement(&self, incoming_items: usize) -> bool {
        self.retained_batches >= MAX_RETAINED_BATCHES
            || self.retained_items.saturating_add(incoming_items) > MAX_RETAINED_BATCH_ITEMS
    }

    fn can_admit(&self, incoming_items: usize) -> bool {
        !self.requires_retirement(incoming_items)
            && self.active_batches < MAX_ACTIVE_BATCHES
            && self.account_batches < MAX_ACTIVE_BATCHES_PER_ACCOUNT
            && self.active_items.saturating_add(incoming_items) <= MAX_ACTIVE_BATCH_ITEMS
    }
}

struct UploadPlanningFence {
    epoch: u64,
    guard: tokio::sync::OwnedMutexGuard<()>,
}

/// The execution owner, not the HTTP waiter, settles the ticket. If that
/// owner is cancelled or panics, retain the reservation for verification.
pub struct UploadExecutionGuard {
    store: UploadBatchStore,
    token: String,
    path: String,
    settled: bool,
}

impl UploadExecutionGuard {
    pub fn new(store: UploadBatchStore, token: String, path: String) -> Self {
        Self {
            store,
            token,
            path,
            settled: false,
        }
    }

    pub async fn finish<T>(&mut self, result: &AppResult<T>) {
        self.store
            .finish_result(&self.token, &self.path, result)
            .await;
        self.settled = true;
    }
}

impl Drop for UploadExecutionGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let (store, token, path) = (self.store.clone(), self.token.clone(), self.path.clone());
        tokio::spawn(async move {
            let Some(_transition) = store.lock_transition(&token).await else {
                return;
            };
            let mut batches = store.batches.lock().await;
            if let Some(item) = batches
                .get_mut(&token)
                .and_then(|batch| batch.items.get_mut(&path))
            {
                if item.status == UploadStatus::InProgress {
                    let outcome = AppError::internal("upload execution owner interrupted")
                        .with_operation(CommitState::Unknown, CleanupState::Unknown)
                        .operation();
                    drop(batches);
                    if let Some(persistence) = &store.persistence {
                        if let Err(error) = persistence
                            .update(
                                &token,
                                &path,
                                UploadStatus::Unknown,
                                outcome,
                                batch_expires_unix(store.ttl),
                            )
                            .await
                        {
                            tracing::error!(%error, "failed to persist interrupted upload result");
                        }
                    }
                    let mut batches = store.batches.lock().await;
                    if let Some(item) = batches
                        .get_mut(&token)
                        .and_then(|batch| batch.items.get_mut(&path))
                    {
                        item.status = UploadStatus::Unknown;
                        item.operation = outcome;
                    }
                }
            }
        });
    }
}

#[derive(Serialize)]
pub struct UploadBatchStatusResponse {
    pub ticket: String,
    pub items: Vec<UploadItemStatus>,
}

#[derive(Serialize)]
pub struct UploadItemStatus {
    pub path: String,
    pub size: u64,
    pub status: UploadStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationOutcome>,
}

impl UploadBatchStore {
    async fn lock_transition(&self, token: &str) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let mut batches = self.batches.lock().await;
        self.prune_expired(&mut batches);
        let gate = batches.get(token)?.transition.clone();
        drop(batches);
        Some(gate.lock_owned().await)
    }

    fn retire_batch(&self, token: String, batch: UploadBatch) {
        if let Some(persistence) = &self.persistence {
            let persistence = persistence.clone();
            let registry = self.receipt_registry.get().cloned();
            tokio::spawn(async move {
                if let Err(error) = persistence.remove(&token, &batch).await {
                    tracing::error!(%error, "failed to retire expired upload result index");
                    return;
                }
                if let Some(registry) = registry {
                    if let Some(backend) = registry.cached(&batch.storage_id).await {
                        for path in batch.items.keys() {
                            let operation = operation_id(&token, path);
                            if let Err(error) = backend.prune_upload_receipt(&operation).await {
                                tracing::warn!(storage_id = %batch.storage_id, %error, "expired upload receipt remains for startup cleanup");
                                break;
                            }
                        }
                    }
                }
            });
        }
    }
    pub(crate) fn bind_receipt_registry(&self, registry: crate::storage_backend::StorageRegistry) {
        let _ = self.receipt_registry.set(registry);
    }
    pub(crate) async fn sweep_expired(&self) {
        let mut batches = self.batches.lock().await;
        self.prune_expired(&mut batches);
    }
    pub(crate) async fn reconcile_receipts(
        &self,
        backends: &crate::storage_backend::StorageRegistry,
    ) -> AppResult<()> {
        // Only called before the listener starts. No new ticket can appear while
        // the authoritative retained set is inspected.
        let batches = self.batches.lock().await;
        for (storage_id, backend) in backends.cached_backends().await {
            let retained = batches
                .iter()
                .filter(|(_, batch)| batch.storage_id == storage_id)
                .flat_map(|(ticket, batch)| {
                    batch
                        .items
                        .keys()
                        .map(move |path| operation_id(ticket, path))
                })
                .collect::<HashSet<_>>();
            backend.prune_upload_receipts(&retained).await?;
        }
        Ok(())
    }
    // Removing a registry entry only schedules durable index/receipt cleanup;
    // it neither waits for that cleanup nor reports it as completed.
    fn prune_expired(&self, batches: &mut HashMap<String, UploadBatch>) {
        let now = Instant::now();
        let expired = batches
            .iter()
            .filter(|(_, batch)| {
                !batch_is_retained(batch, now) && Arc::strong_count(&batch.transition) == 1
            })
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        for token in expired {
            if let Some(batch) = batches.remove(&token) {
                self.retire_batch(token, batch);
            }
        }
    }
    pub(crate) async fn unknown_items(&self) -> Vec<UnknownUpload> {
        let batches = self.batches.lock().await;
        batches
            .iter()
            .flat_map(|(ticket, batch)| {
                batch
                    .items
                    .iter()
                    .filter(|(_, item)| item.status == UploadStatus::Unknown)
                    .map(|(path, item)| UnknownUpload {
                        backend: batch.recovery_backend.clone(),
                        ticket: ticket.clone(),
                        storage_id: batch.storage_id.clone(),
                        namespace_id: batch.namespace_id.clone(),
                        path: path.clone(),
                        size: item.size,
                        operation: item.operation,
                    })
            })
            .collect()
    }

    pub(crate) async fn has_unsettled_namespace(&self, storage_id: &str, namespace: &str) -> bool {
        self.batches.lock().await.values().any(|batch| {
            batch.storage_id == storage_id
                && batch.namespace_id.as_deref() == Some(namespace)
                && batch_has_recovery_work(batch)
        })
    }

    pub(crate) async fn confirm_unknown_committed(&self, review: &UnknownUpload) -> AppResult<()> {
        let _transition = self
            .lock_transition(&review.ticket)
            .await
            .ok_or(AppError::NotFound)?;
        let mut batches = self.batches.lock().await;
        let batch = batches.get_mut(&review.ticket).ok_or(AppError::NotFound)?;
        if batch.storage_id != review.storage_id {
            return Err(AppError::Forbidden);
        }
        let item = batch
            .items
            .get_mut(&review.path)
            .ok_or(AppError::NotFound)?;
        if item.status != UploadStatus::Unknown {
            return Err(AppError::Conflict("任务状态已经变化，请刷新".into()));
        }
        let expires_unix = batch_expires_unix(self.result_ttl);
        drop(batches);
        if let Some(persistence) = &self.persistence {
            persistence
                .update(
                    &review.ticket,
                    &review.path,
                    UploadStatus::Complete,
                    None,
                    expires_unix,
                )
                .await?;
        }
        let mut batches = self.batches.lock().await;
        let batch = batches.get_mut(&review.ticket).ok_or(AppError::NotFound)?;
        let item = batch
            .items
            .get_mut(&review.path)
            .ok_or(AppError::NotFound)?;
        item.status = UploadStatus::Complete;
        item.operation = None;
        batch.expires_at = Instant::now() + self.result_ttl;
        batch.expires_unix = expires_unix;
        if !batch_has_recovery_work(batch) {
            batch.recovery_backend = None;
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn ensure_storage_idle(&self, storage_id: &str) -> AppResult<()> {
        if self
            .batches
            .lock()
            .await
            .values()
            .any(|batch| batch.storage_id == storage_id && batch_has_recovery_work(batch))
        {
            return Err(AppError::Conflict(
                "存储仍有正在执行或后台自动恢复的上传，请稍后重试".into(),
            ));
        }
        Ok(())
    }

    pub async fn invalidate_storage(
        &self,
        storage_id: &str,
        backend: Option<crate::storage_backend::StorageBackend>,
    ) {
        let mut batches = self.batches.lock().await;
        self.invalidation_epoch.fetch_add(1, Ordering::AcqRel);
        for batch in batches
            .values_mut()
            .filter(|batch| batch.storage_id == storage_id && batch.recovery_backend.is_none())
        {
            for item in batch.items.values_mut() {
                if item.status == UploadStatus::Pending {
                    item.status = UploadStatus::Cancelled;
                }
            }
            if batch_has_recovery_work(batch) {
                batch.recovery_backend = backend.clone();
            }
            batch.expires_at = Instant::now() + self.ttl;
        }
    }
    pub fn new(ttl: Duration) -> Self {
        Self {
            batches: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            result_ttl: ttl,
            prepare_gate: Arc::new(Mutex::new(())),
            invalidation_epoch: Arc::new(AtomicU64::new(0)),
            persistence: None,
            receipt_registry: Arc::new(std::sync::OnceLock::new()),
        }
    }

    pub async fn load_persistent(config_path: &std::path::Path, ttl: Duration) -> AppResult<Self> {
        let (persistence, batches) = BatchPersistence::load(config_path, ttl).await?;
        Ok(Self {
            batches: Arc::new(Mutex::new(batches)),
            ttl,
            result_ttl: Duration::from_secs(24 * 60 * 60),
            prepare_gate: Arc::new(Mutex::new(())),
            invalidation_epoch: Arc::new(AtomicU64::new(0)),
            persistence: Some(Arc::new(persistence)),
            receipt_registry: Arc::new(std::sync::OnceLock::new()),
        })
    }

    #[cfg(test)]
    pub async fn create(
        &self,
        subject: RequestSubject,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
    ) -> AppResult<String> {
        let account = match &subject {
            RequestSubject::Session(token) | RequestSubject::Gate(token) => token.clone(),
        };
        self.create_for_account(subject, account, storage_id, items)
            .await
    }

    #[cfg(test)]
    async fn create_for_account(
        &self,
        subject: RequestSubject,
        account: String,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
    ) -> AppResult<String> {
        self.create_for_account_with_namespace(subject, account, storage_id, items, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn create_for_account_with_namespace(
        &self,
        subject: RequestSubject,
        account: String,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
        namespace_id: Option<String>,
    ) -> AppResult<String> {
        let expected_epoch = self.invalidation_epoch.load(Ordering::Acquire);
        self.create_for_account_with_namespace_owned(
            subject,
            account,
            storage_id,
            items,
            namespace_id,
            expected_epoch,
        )
        .await
    }

    async fn create_for_account_with_namespace_planned(
        &self,
        subject: RequestSubject,
        account: String,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
        namespace_id: Option<String>,
        planning: UploadPlanningFence,
    ) -> AppResult<String> {
        // Once creation starts, its disk manifest and in-memory reservation
        // must settle together even if the HTTP waiter disconnects. Retain
        // the planning gate so another request cannot queue behind an orphan.
        let store = self.clone();
        tokio::spawn(async move {
            let _planning_guard = planning.guard;
            store
                .create_for_account_with_namespace_owned(
                    subject,
                    account,
                    storage_id,
                    items,
                    namespace_id,
                    planning.epoch,
                )
                .await
        })
        .await
        .map_err(|error| AppError::with_source("upload batch creation task failed", error))?
    }

    async fn create_for_account_with_namespace_owned(
        &self,
        subject: RequestSubject,
        account: String,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
        namespace_id: Option<String>,
        expected_epoch: u64,
    ) -> AppResult<String> {
        let mut batches = self.batches.lock().await;
        if self.invalidation_epoch.load(Ordering::Acquire) != expected_epoch {
            return Err(AppError::Conflict(
                "存储设置在上传准备期间已变化，请重新选择文件".into(),
            ));
        }
        let now = Instant::now();
        self.prune_expired(&mut batches);
        let mut usage = BatchUsage::from_batches(&batches, &account);
        if usage.requires_retirement(items.len()) {
            // Snapshot only when space is needed. All candidates are terminal,
            // so retiring them cannot change active/account admission counts.
            let mut completed = batches
                .iter()
                .filter(|(_, batch)| {
                    batch_is_terminal(batch) && Arc::strong_count(&batch.transition) == 1
                })
                .map(|(token, batch)| (token.clone(), batch.expires_at))
                .collect::<Vec<_>>();
            completed.sort_by_key(|(_, expires_at)| *expires_at);
            for (token, _) in completed {
                if !usage.requires_retirement(items.len()) {
                    break;
                }
                if let Some(batch) = batches.remove(&token) {
                    usage.retained_batches -= 1;
                    usage.retained_items -= batch.items.len();
                    self.retire_batch(token, batch);
                }
            }
        }
        if !usage.can_admit(items.len()) {
            return Err(AppError::TooManyRequests);
        }
        let token = Uuid::new_v4().to_string();
        let batch = UploadBatch {
            transition: Arc::new(Mutex::new(())),
            recovery_backend: None,
            subject: Some(subject),
            account,
            storage_id,
            namespace_id,
            expires_at: now + self.ttl.min(PENDING_UPLOAD_TTL),
            expires_unix: batch_expires_unix(self.ttl.min(PENDING_UPLOAD_TTL)),
            items: items
                .into_iter()
                .map(|(path, (request_path, size))| {
                    (
                        path,
                        UploadItem {
                            request_path,
                            size,
                            status: UploadStatus::Pending,
                            operation: None,
                        },
                    )
                })
                .collect(),
        };
        let prepared = self
            .persistence
            .as_ref()
            .map(|_| BatchPersistence::prepare(&token, &batch))
            .transpose()?;
        let _transition = batch.transition.clone().lock_owned().await;
        // Reserve capacity and names before releasing the registry, so another
        // creation cannot admit the same budget while this manifest is writing.
        batches.insert(token.clone(), batch);
        drop(batches);
        if let (Some(persistence), Some(prepared)) = (&self.persistence, prepared) {
            if let Err(error) = persistence.create(&token, prepared).await {
                self.batches.lock().await.remove(&token);
                return Err(error);
            }
        }
        let mut batches = self.batches.lock().await;
        if self.invalidation_epoch.load(Ordering::Acquire) != expected_epoch {
            if let Some(batch) = batches.remove(&token) {
                self.retire_batch(token, batch);
            }
            return Err(AppError::Conflict(
                "存储设置在上传准备期间已变化，请重新选择文件".into(),
            ));
        }
        Ok(token)
    }

    async fn reserved_paths(&self, storage_id: &str) -> HashSet<String> {
        let batches = self.batches.lock().await;
        batches
            .values()
            .filter(|batch| {
                batch.storage_id == storage_id && batch_is_retained(batch, Instant::now())
            })
            .flat_map(|batch| {
                batch
                    .items
                    .iter()
                    .filter(|(_, item)| !item.status.is_terminal())
                    .map(|(path, _)| path.clone())
            })
            .collect()
    }

    pub async fn begin(
        &self,
        token: &str,
        subject: &RequestSubject,
        storage_id: &str,
        path: &str,
        size: u64,
    ) -> AppResult<UploadBegin> {
        let _transition = self
            .lock_transition(token)
            .await
            .ok_or(AppError::UploadBatchExpired)?;
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        self.prune_expired(&mut batches);
        let batch = batches.get_mut(token).ok_or(AppError::UploadBatchExpired)?;
        if batch.subject.as_ref() != Some(subject) || batch.storage_id != storage_id {
            return Err(AppError::Forbidden);
        }
        let item = batch
            .items
            .get_mut(path)
            .ok_or_else(|| AppError::BadRequest("上传目标不属于该批次".into()))?;
        if item.size != size {
            return Err(AppError::BadRequest("上传文件大小与批次声明不一致".into()));
        }
        match item.begin_decision()? {
            UploadBegin::Start => {}
            completed @ UploadBegin::AlreadyComplete(_) => return Ok(completed),
        }
        let expires_unix = batch_expires_unix(self.ttl);
        drop(batches);
        if let Some(persistence) = &self.persistence {
            persistence
                .update(token, path, UploadStatus::InProgress, None, expires_unix)
                .await?;
        }
        let mut batches = self.batches.lock().await;
        let batch = batches.get_mut(token).ok_or(AppError::UploadBatchExpired)?;
        let item = batch.items.get_mut(path).ok_or(AppError::NotFound)?;
        if item.status == UploadStatus::Cancelled {
            // Administrator edits may invalidate pending admission while its
            // durable write is running. Never resurrect that old reservation.
            drop(batches);
            if let Some(persistence) = &self.persistence {
                persistence
                    .update(token, path, UploadStatus::Cancelled, None, expires_unix)
                    .await?;
            }
            return Err(AppError::Conflict("存储设置已变化，请重新准备上传".into()));
        }
        item.status = UploadStatus::InProgress;
        item.operation = None;
        batch.expires_at = now + self.ttl;
        batch.expires_unix = expires_unix;
        Ok(UploadBegin::Start)
    }

    pub async fn cancel(
        &self,
        token: &str,
        subject: &RequestSubject,
        storage_id: &str,
        paths: Option<&HashSet<String>>,
    ) -> AppResult<()> {
        let Some(_transition) = self.lock_transition(token).await else {
            return Ok(());
        };
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        self.prune_expired(&mut batches);
        let Some(batch) = batches.get_mut(token) else {
            return Ok(());
        };
        if batch.subject.as_ref() != Some(subject) || batch.storage_id != storage_id {
            return Err(AppError::Forbidden);
        }
        if let Some(paths) = paths {
            if paths.iter().any(|path| !batch.items.contains_key(path)) {
                return Err(AppError::BadRequest("取消目标不属于该批次".into()));
            }
        }
        let cancelled = batch
            .items
            .iter()
            .filter(|(path, item)| {
                paths.is_none_or(|paths| paths.contains(*path)) && item.status.can_cancel()
            })
            .map(|(path, _)| path.clone())
            .collect::<HashSet<_>>();
        let retention = batch_retention(
            self.ttl,
            self.result_ttl,
            batch.items.iter().map(|(path, item)| {
                if cancelled.contains(path) {
                    UploadStatus::Cancelled
                } else {
                    item.status
                }
            }),
        );
        drop(batches);
        for path in cancelled {
            if let Some(persistence) = &self.persistence {
                persistence
                    .update(
                        token,
                        &path,
                        UploadStatus::Cancelled,
                        None,
                        batch_expires_unix(retention),
                    )
                    .await?;
            }
            let mut batches = self.batches.lock().await;
            let batch = batches.get_mut(token).ok_or(AppError::UploadBatchExpired)?;
            let item = batch.items.get_mut(&path).ok_or(AppError::NotFound)?;
            item.status = UploadStatus::Cancelled;
            item.operation = None;
        }
        let mut batches = self.batches.lock().await;
        let batch = batches.get_mut(token).ok_or(AppError::UploadBatchExpired)?;
        batch.expires_at = now + retention;
        batch.expires_unix = batch_expires_unix(retention);
        Ok(())
    }

    pub async fn finish_result<T>(&self, token: &str, path: &str, result: &AppResult<T>) {
        let (status, operation) = classify_upload_result(result);
        self.finish_with_status(token, path, status, operation)
            .await;
    }

    #[cfg(test)]
    async fn finish(&self, token: &str, path: &str, succeeded: bool) {
        self.finish_with_status(
            token,
            path,
            if succeeded {
                UploadStatus::Complete
            } else {
                UploadStatus::Failed
            },
            None,
        )
        .await;
    }

    async fn finish_with_status(
        &self,
        token: &str,
        path: &str,
        status: UploadStatus,
        operation: Option<OperationOutcome>,
    ) {
        let Some(_transition) = self.lock_transition(token).await else {
            return;
        };
        let mut batches = self.batches.lock().await;
        let Some(batch) = batches.get_mut(token) else {
            return;
        };
        if !batch.items.contains_key(path) {
            return;
        }
        let retention = batch_retention(
            self.ttl,
            self.result_ttl,
            batch
                .items
                .iter()
                .map(|(key, item)| if key == path { status } else { item.status }),
        );
        let expires_unix = batch_expires_unix(retention);
        drop(batches);
        if let Some(persistence) = &self.persistence {
            if let Err(error) = persistence
                .update(token, path, status, operation, expires_unix)
                .await
            {
                tracing::error!(%error, "failed to persist upload result; recovery will verify operation");
            }
        }
        let mut batches = self.batches.lock().await;
        let Some(batch) = batches.get_mut(token) else {
            return;
        };
        let Some(item) = batch.items.get_mut(path) else {
            return;
        };
        item.status = status;
        item.operation = operation;
        batch.expires_at = Instant::now() + retention;
        batch.expires_unix = expires_unix;
        if !batch_has_recovery_work(batch) {
            batch.recovery_backend = None;
        }
    }

    pub async fn status(
        &self,
        token: &str,
        subject: &RequestSubject,
        storage_id: &str,
    ) -> AppResult<UploadBatchStatusResponse> {
        self.status_with_account(token, subject, None, storage_id)
            .await
    }

    pub async fn status_for_account(
        &self,
        token: &str,
        subject: &RequestSubject,
        account: &str,
        storage_id: &str,
    ) -> AppResult<UploadBatchStatusResponse> {
        self.status_with_account(token, subject, Some(account), storage_id)
            .await
    }

    async fn status_with_account(
        &self,
        token: &str,
        subject: &RequestSubject,
        account: Option<&str>,
        storage_id: &str,
    ) -> AppResult<UploadBatchStatusResponse> {
        let mut batches = self.batches.lock().await;
        self.prune_expired(&mut batches);
        let batch = batches.get(token).ok_or(AppError::UploadBatchExpired)?;
        if batch.storage_id != storage_id
            || (batch.subject.as_ref() != Some(subject) && account != Some(batch.account.as_str()))
        {
            return Err(AppError::Forbidden);
        }
        let mut items = batch
            .items
            .values()
            .map(|item| UploadItemStatus {
                path: item.request_path.clone(),
                size: item.size,
                status: item.status,
                operation: item.operation,
            })
            .collect::<Vec<_>>();
        items.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(UploadBatchStatusResponse {
            ticket: token.to_owned(),
            items,
        })
    }
}

fn batch_expires_unix(duration: Duration) -> i64 {
    chrono::Utc::now()
        .timestamp()
        .saturating_add(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[derive(Clone, Serialize)]
pub(crate) struct UnknownUpload {
    #[serde(skip)]
    pub backend: Option<crate::storage_backend::StorageBackend>,
    pub ticket: String,
    pub storage_id: String,
    pub namespace_id: Option<String>,
    pub path: String,
    pub size: u64,
    pub operation: Option<OperationOutcome>,
}

pub(crate) fn operation_id(ticket: &str, path: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("{ticket}:{path}").as_bytes());
    digest.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn namespace_id(
    config: &crate::config::Config,
    backend: &crate::config::StorageBackendConfig,
) -> AppResult<String> {
    let identity = match backend {
        crate::config::StorageBackendConfig::Local(settings) => {
            let mount = config
                .local_mounts
                .resolve(&settings.mount_id)
                .ok_or(AppError::NotFound)?;
            serde_json::json!(["local", mount.path])
        }
        crate::config::StorageBackendConfig::S3(settings) => {
            serde_json::json!([
                "s3",
                settings.provider,
                settings.endpoint,
                settings.bucket,
                settings.prefix
            ])
        }
    };
    let bytes = serde_json::to_vec(&identity)
        .map_err(|error| AppError::with_source("failed to identify upload storage", error))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    Ok(digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn batch_retention(
    upload_ttl: Duration,
    result_ttl: Duration,
    statuses: impl Iterator<Item = UploadStatus>,
) -> Duration {
    let mut terminal = true;
    for status in statuses {
        if status.needs_recovery() {
            return upload_ttl;
        }
        terminal &= status.is_terminal();
    }
    if terminal {
        result_ttl
    } else {
        upload_ttl.min(PENDING_UPLOAD_TTL)
    }
}

fn batch_is_retained(batch: &UploadBatch, now: Instant) -> bool {
    batch.expires_at > now || batch_has_recovery_work(batch)
}

fn batch_has_recovery_work(batch: &UploadBatch) -> bool {
    batch
        .items
        .values()
        .any(|item| item.status.needs_recovery())
}

fn batch_is_terminal(batch: &UploadBatch) -> bool {
    batch.items.values().all(|item| item.status.is_terminal())
}

#[derive(Deserialize)]
pub struct PrepareUploadBatchRequest {
    pub items: Vec<PrepareUploadItem>,
}

#[derive(Deserialize)]
pub struct PrepareUploadItem {
    pub path: String,
    pub size: u64,
}

#[derive(Serialize)]
pub struct PrepareUploadBatchResponse {
    pub ticket: String,
    pub items: Vec<PreparedUploadName>,
    pub upload_mode: &'static str,
}

#[derive(Serialize)]
pub struct PreparedUploadName {
    pub original_path: String,
    pub path: String,
}

fn numbered_upload_name(name: &str, names: &HashSet<String>) -> AppResult<String> {
    let (stem, extension) = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or((name, String::new()), |(stem, extension)| {
            (stem, format!(".{extension}"))
        });
    let (base, initial) = stem
        .strip_suffix(')')
        .and_then(|stem| stem.rsplit_once(" ("))
        .and_then(|(base, number)| {
            number
                .parse::<u64>()
                .ok()
                .filter(|number| *number > 0)
                .map(|number| (base, number))
        })
        .unwrap_or((stem, 0));
    let prefix = format!("{base} (");
    let suffix = format!("){extension}");
    let maximum = names
        .iter()
        .filter_map(|name| {
            name.strip_prefix(&prefix)?
                .strip_suffix(&suffix)?
                .parse::<u64>()
                .ok()
        })
        .fold(initial, u64::max);
    let next = maximum
        .checked_add(1)
        .ok_or_else(|| AppError::Conflict("文件重复编号超出范围".into()))?;
    Ok(format!("{base} ({next}){extension}"))
}

#[derive(Deserialize)]
pub struct CancelUploadBatchRequest {
    pub ticket: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

pub async fn prepare_upload_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<PrepareUploadBatchRequest>,
) -> AppResult<Json<PrepareUploadBatchResponse>> {
    let expected_epoch = state
        .upload_batches
        .invalidation_epoch
        .load(Ordering::Acquire);
    let share = crate::file_access::resolve_write_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Upload).await?;
    let subject = auth::current_request_subject(&state, &headers)
        .await
        .ok_or(AppError::Forbidden)?;
    let policy = state.config_file.read().await.clone();
    let upload_mode = if policy.storage_instances.iter().any(|instance| instance.id == share.storage_id
        && matches!(&instance.backend, crate::config::StorageBackendConfig::S3(settings) if !settings.relay_upload)) {
        "direct"
    } else { "relay" };
    if body.items.is_empty() || body.items.len() > policy.max_upload_batch_entries {
        return Err(AppError::PayloadTooLarge);
    }
    // Serialize only name planning, never the upload body. Pending names belong to tickets.
    let planning_guard = state.upload_batches.prepare_gate.clone().lock_owned().await;
    let backend = state.storage_backend(&share.storage_id).await?;
    let mut reserved = state.upload_batches.reserved_paths(&share.storage_id).await;
    let mut directory_names: HashMap<String, HashSet<String>> = HashMap::new();
    let mut prepared = Vec::with_capacity(body.items.len());
    let mut total = 0_u64;
    let mut validated = HashMap::with_capacity(body.items.len());
    for item in body.items {
        if item.size > policy.max_upload_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        total = total
            .checked_add(item.size)
            .ok_or(AppError::PayloadTooLarge)?;
        if total > policy.max_upload_batch_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        let mut request_path = StorageService::normalize_relative(&item.path)?;
        ensure_non_root(&request_path)?;
        let name = request_path.rsplit('/').next().unwrap_or_default();
        if name.trim() != name
            || name
                .chars()
                .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(AppError::BadRequest("上传目标文件名无效".into()));
        }
        let mut storage_path = share_storage_path(&share, &request_path);
        check_folder_locks(&state, &headers, &share.storage_id, &storage_path).await?;
        let original_path = request_path.clone();
        let exists = match backend.metadata(&storage_path).await {
            Ok(_) => true,
            Err(AppError::NotFound) => false,
            Err(error) => return Err(error),
        };
        if exists || reserved.contains(&storage_path) {
            let (parent, name) = request_path.rsplit_once('/').unwrap_or(("", &request_path));
            let storage_parent = share_storage_path(&share, parent);
            if !directory_names.contains_key(&storage_parent) {
                let (entries, truncated) =
                    match backend.list_directory(&storage_parent, 10_000).await {
                        Ok(result) => result,
                        Err(AppError::NotFound) => (Vec::new(), false),
                        Err(error) => return Err(error),
                    };
                if truncated {
                    return Err(AppError::Conflict(
                        "目录文件过多，无法完整确认重复编号，请换一个文件名".into(),
                    ));
                }
                directory_names.insert(
                    storage_parent.clone(),
                    entries.into_iter().map(|entry| entry.name).collect(),
                );
            }
            let names = directory_names
                .get_mut(&storage_parent)
                .expect("loaded directory names");
            for reserved_path in &reserved {
                let (reserved_parent, reserved_name) = reserved_path
                    .rsplit_once('/')
                    .unwrap_or(("", reserved_path));
                if reserved_parent == storage_parent {
                    names.insert(reserved_name.to_owned());
                }
            }
            let assigned = numbered_upload_name(name, names)?;
            names.insert(assigned.clone());
            request_path = if parent.is_empty() {
                assigned
            } else {
                format!("{parent}/{assigned}")
            };
            storage_path = share_storage_path(&share, &request_path);
            check_folder_locks(&state, &headers, &share.storage_id, &storage_path).await?;
        }
        reserved.insert(storage_path.clone());
        prepared.push(PreparedUploadName {
            original_path,
            path: request_path.clone(),
        });
        if validated
            .insert(storage_path, (request_path, item.size))
            .is_some()
        {
            return Err(AppError::Conflict("批量上传包含重复目标路径".into()));
        }
    }
    let ticket = state
        .upload_batches
        .create_for_account_with_namespace_planned(
            subject,
            crate::traffic::browser_subject(&state, &headers).await,
            share.storage_id.clone(),
            validated,
            Some(namespace_id(
                &state.config,
                &policy
                    .storage_instances
                    .iter()
                    .find(|instance| instance.id == share.storage_id)
                    .ok_or(AppError::NotFound)?
                    .backend,
            )?),
            UploadPlanningFence {
                epoch: expected_epoch,
                guard: planning_guard,
            },
        )
        .await?;
    Ok(Json(PrepareUploadBatchResponse {
        ticket,
        items: prepared,
        upload_mode,
    }))
}

pub async fn cancel_upload_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<CancelUploadBatchRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Upload).await?;
    let subject = auth::current_request_subject(&state, &headers)
        .await
        .ok_or(AppError::Forbidden)?;
    let paths = body
        .paths
        .iter()
        .map(|path| {
            let request_path = StorageService::normalize_relative(path)?;
            ensure_non_root(&request_path)?;
            Ok(share_storage_path(&share, &request_path))
        })
        .collect::<AppResult<HashSet<_>>>()?;
    state
        .upload_batches
        .cancel(
            &body.ticket,
            &subject,
            &share.storage_id,
            (!paths.is_empty()).then_some(&paths),
        )
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn upload_batch_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> AppResult<Json<UploadBatchStatusResponse>> {
    let share = resolve_share(&state, &headers, &query).await?;
    ensure_storage_action(&state, &headers, &share.storage_id, StorageAction::Upload).await?;
    let subject = auth::current_request_subject(&state, &headers)
        .await
        .ok_or(AppError::Forbidden)?;
    let ticket = query
        .batch
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("缺少上传批次票据".into()))?;
    Ok(Json(
        state
            .upload_batches
            .status_for_account(
                ticket,
                &subject,
                &crate::traffic::browser_subject(&state, &headers).await,
                &share.storage_id,
            )
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_names_expire_soon_but_running_and_unknown_results_keep_their_budget() {
        let upload = Duration::from_secs(6 * 60 * 60);
        let result = Duration::from_secs(24 * 60 * 60);
        let retention =
            |statuses: Vec<UploadStatus>| batch_retention(upload, result, statuses.into_iter());
        assert_eq!(retention(vec![UploadStatus::Pending]), PENDING_UPLOAD_TTL);
        assert_eq!(
            retention(vec![UploadStatus::Complete, UploadStatus::Pending]),
            PENDING_UPLOAD_TTL
        );
        assert_eq!(
            retention(vec![UploadStatus::Pending, UploadStatus::InProgress]),
            upload
        );
        assert_eq!(retention(vec![UploadStatus::Unknown]), upload);
        assert_eq!(
            retention(vec![UploadStatus::Complete, UploadStatus::Cancelled]),
            result
        );
    }

    #[tokio::test]
    async fn slow_ticket_write_does_not_block_another_batch_or_status_reads() {
        let directory = crate::test_support::TestDirectory::new("independent-upload-tickets");
        let store = UploadBatchStore::load_persistent(
            &directory.path().join("config.json"),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        let owner = subject("owner");
        let first = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        let second = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        let gate = Arc::new(Mutex::new(()));
        let pause = gate.lock().await;
        *store
            .persistence
            .as_ref()
            .unwrap()
            .update_pause
            .lock()
            .unwrap() = Some((first.clone(), gate.clone()));
        let starting = tokio::spawn({
            let (store, owner, first) = (store.clone(), owner.clone(), first.clone());
            async move {
                store
                    .begin(&first, &owner, "local", "folder/one.txt", 11)
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&gate) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            store.status(&first, &owner, "local"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                store.begin(&second, &owner, "local", "folder/one.txt", 11)
            )
            .await
            .unwrap()
            .unwrap(),
            UploadBegin::Start
        );
        store.finish(&second, "folder/one.txt", true).await;
        drop(pause);
        assert_eq!(starting.await.unwrap().unwrap(), UploadBegin::Start);
    }

    #[tokio::test]
    async fn storage_edit_during_admission_write_cannot_resurrect_old_ticket() {
        let directory = crate::test_support::TestDirectory::new("upload-admission-edit");
        let config_path = directory.path().join("config.json");
        let store = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        let gate = Arc::new(Mutex::new(()));
        let pause = gate.lock().await;
        *store
            .persistence
            .as_ref()
            .unwrap()
            .update_pause
            .lock()
            .unwrap() = Some((ticket.clone(), gate.clone()));
        let starting = tokio::spawn({
            let (store, owner, ticket) = (store.clone(), owner.clone(), ticket.clone());
            async move {
                store
                    .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&gate) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            store.invalidate_storage("local", None),
        )
        .await
        .unwrap();
        drop(pause);
        assert!(matches!(
            starting.await.unwrap(),
            Err(AppError::Conflict(_))
        ));
        assert!(store
            .status(&ticket, &owner, "local")
            .await
            .unwrap()
            .items
            .iter()
            .all(|item| item.status == UploadStatus::Cancelled));
        drop(store);
        let restored = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        assert!(restored
            .status_for_account(&ticket, &subject("new-session"), "owner", "local")
            .await
            .unwrap()
            .items
            .iter()
            .all(|item| item.status == UploadStatus::Cancelled));
    }

    fn admission_batch(
        account: &str,
        count: usize,
        status: UploadStatus,
        expires_at: Instant,
    ) -> UploadBatch {
        UploadBatch {
            transition: Arc::new(Mutex::new(())),
            recovery_backend: None,
            subject: Some(subject("owner")),
            account: account.into(),
            storage_id: "local".into(),
            namespace_id: None,
            expires_at,
            expires_unix: batch_expires_unix(Duration::from_secs(3600)),
            items: (0..count)
                .map(|index| {
                    let path = format!("{index}.txt");
                    (
                        path.clone(),
                        UploadItem {
                            request_path: path,
                            size: 4,
                            status,
                            operation: operation_for_status(status),
                        },
                    )
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn active_item_budget_excludes_ended_items_and_reopens_after_a_failure() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(
                owner.clone(),
                "local".into(),
                (0..MAX_ACTIVE_BATCH_ITEMS)
                    .map(|index| (format!("{index}.txt"), (format!("{index}.txt"), 4)))
                    .collect(),
            )
            .await
            .unwrap();
        let incoming = || HashMap::from([("next.txt".into(), ("next.txt".into(), 4))]);
        assert!(matches!(
            store
                .create(owner.clone(), "local".into(), incoming())
                .await,
            Err(AppError::TooManyRequests)
        ));
        store.finish(&ticket, "0.txt", false).await;
        store
            .create(owner, "local".into(), incoming())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn retained_item_pressure_retires_only_the_oldest_terminal_batch() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let now = Instant::now();
        let mut unknown = admission_batch(
            "other",
            10_000,
            UploadStatus::Complete,
            now + Duration::from_secs(100),
        );
        unknown.items.get_mut("0.txt").unwrap().status = UploadStatus::Unknown;
        let mut pending = admission_batch(
            "other",
            10_000,
            UploadStatus::Failed,
            now + Duration::from_secs(200),
        );
        pending.items.get_mut("0.txt").unwrap().status = UploadStatus::Pending;
        store.batches.lock().await.extend([
            ("unknown".into(), unknown),
            ("pending".into(), pending),
            (
                "oldest".into(),
                admission_batch(
                    "other",
                    10_000,
                    UploadStatus::Complete,
                    now + Duration::from_secs(300),
                ),
            ),
            (
                "newer".into(),
                admission_batch(
                    "other",
                    10_000,
                    UploadStatus::Cancelled,
                    now + Duration::from_secs(400),
                ),
            ),
        ]);
        let ticket = store
            .create(subject("owner"), "local".into(), items())
            .await
            .unwrap();
        let batches = store.batches.lock().await;
        assert!(!batches.contains_key("oldest"));
        for retained in ["unknown", "pending", "newer", &ticket] {
            assert!(batches.contains_key(retained));
        }
        assert_eq!(batches.len(), 4);
    }

    #[tokio::test]
    async fn retained_batch_pressure_keeps_live_work_and_retires_the_oldest_result() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let now = Instant::now();
        {
            let mut batches = store.batches.lock().await;
            batches.insert(
                "unknown".into(),
                admission_batch(
                    "other",
                    1,
                    UploadStatus::Unknown,
                    now + Duration::from_secs(100),
                ),
            );
            batches.insert(
                "pending".into(),
                admission_batch(
                    "other",
                    1,
                    UploadStatus::Pending,
                    now + Duration::from_secs(200),
                ),
            );
            for index in 0..MAX_RETAINED_BATCHES - 2 {
                batches.insert(
                    format!("result-{index}"),
                    admission_batch(
                        "other",
                        1,
                        UploadStatus::Complete,
                        now + Duration::from_secs(300 + index as u64),
                    ),
                );
            }
        }
        let ticket = store
            .create(subject("owner"), "local".into(), items())
            .await
            .unwrap();
        let batches = store.batches.lock().await;
        assert!(!batches.contains_key("result-0"));
        assert!(batches.contains_key("result-1"));
        assert!(batches.contains_key("unknown"));
        assert!(batches.contains_key("pending"));
        assert!(batches.contains_key(&ticket));
        assert_eq!(batches.len(), MAX_RETAINED_BATCHES);
    }

    #[tokio::test]
    async fn result_retirement_still_precedes_active_limit_rejection() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let now = Instant::now();
        let mut unknown = admission_batch(
            "other",
            MAX_ACTIVE_BATCH_ITEMS,
            UploadStatus::Complete,
            now + Duration::from_secs(100),
        );
        unknown.items.get_mut("0.txt").unwrap().status = UploadStatus::Unknown;
        store.batches.lock().await.extend([
            ("unknown".into(), unknown),
            (
                "oldest".into(),
                admission_batch(
                    "other",
                    10_000,
                    UploadStatus::Complete,
                    now + Duration::from_secs(200),
                ),
            ),
            (
                "newer".into(),
                admission_batch(
                    "other",
                    10_000,
                    UploadStatus::Cancelled,
                    now + Duration::from_secs(300),
                ),
            ),
        ]);
        let incoming = (0..MAX_ACTIVE_BATCH_ITEMS)
            .map(|index| (format!("new-{index}.txt"), (format!("new-{index}.txt"), 4)))
            .collect();
        assert!(matches!(
            store
                .create(subject("owner"), "local".into(), incoming)
                .await,
            Err(AppError::TooManyRequests)
        ));
        let batches = store.batches.lock().await;
        assert!(!batches.contains_key("oldest"));
        assert!(!batches.contains_key("newer"));
        assert!(batches.contains_key("unknown"));
        assert_eq!(batches.len(), 1);
    }

    fn operation_for_status(status: UploadStatus) -> Option<OperationOutcome> {
        let (commit, cleanup) = match status {
            UploadStatus::Complete => (CommitState::Committed, CleanupState::Pending),
            UploadStatus::Failed => (CommitState::NotCommitted, CleanupState::Complete),
            UploadStatus::Unknown => (CommitState::Unknown, CleanupState::Pending),
            _ => return None,
        };
        Some(OperationOutcome::new(commit, cleanup))
    }

    #[test]
    fn admission_counts_all_states_but_scopes_active_batches_to_the_account() {
        let expires_at = Instant::now() + Duration::from_secs(60);
        let mut mixed = admission_batch("first", 6, UploadStatus::Complete, expires_at);
        for (index, status) in [
            UploadStatus::Pending,
            UploadStatus::InProgress,
            UploadStatus::Complete,
            UploadStatus::Failed,
            UploadStatus::Unknown,
            UploadStatus::Cancelled,
        ]
        .into_iter()
        .enumerate()
        {
            mixed.items.get_mut(&format!("{index}.txt")).unwrap().status = status;
        }
        let batches = HashMap::from([
            ("mixed".into(), mixed),
            (
                "ended".into(),
                admission_batch("first", 3, UploadStatus::Failed, expires_at),
            ),
            (
                "other".into(),
                admission_batch("second", 1, UploadStatus::Unknown, expires_at),
            ),
            (
                "empty".into(),
                admission_batch("first", 0, UploadStatus::Pending, expires_at),
            ),
        ]);
        assert_eq!(
            BatchUsage::from_batches(&batches, "first"),
            BatchUsage {
                retained_batches: 4,
                retained_items: 10,
                active_batches: 2,
                account_batches: 1,
                active_items: 4,
            }
        );
        assert_eq!(
            BatchUsage::from_batches(&batches, "absent").account_batches,
            0
        );
    }

    #[test]
    fn admission_snapshot_is_recomputed_after_task_and_membership_changes() {
        let expires_at = Instant::now() + Duration::from_secs(60);
        let mut batches = HashMap::from([(
            "ticket".into(),
            admission_batch("first", 1, UploadStatus::Pending, expires_at),
        )]);
        let before = BatchUsage::from_batches(&batches, "first");
        batches
            .get_mut("ticket")
            .unwrap()
            .items
            .get_mut("0.txt")
            .unwrap()
            .status = UploadStatus::Complete;
        let after = BatchUsage::from_batches(&batches, "first");
        assert_eq!(before.active_items, 1);
        assert_eq!(
            after,
            BatchUsage {
                retained_batches: 1,
                retained_items: 1,
                ..BatchUsage::default()
            }
        );
        batches.clear();
        assert_eq!(
            BatchUsage::from_batches(&batches, "first"),
            BatchUsage::default()
        );
    }

    #[test]
    fn admission_keeps_each_batch_and_item_limit_at_its_existing_boundary() {
        let near = BatchUsage {
            retained_batches: MAX_RETAINED_BATCHES - 1,
            retained_items: MAX_RETAINED_BATCH_ITEMS - 1,
            active_batches: MAX_ACTIVE_BATCHES - 1,
            account_batches: MAX_ACTIVE_BATCHES_PER_ACCOUNT - 1,
            active_items: MAX_ACTIVE_BATCH_ITEMS - 1,
        };
        assert!(near.can_admit(1));
        for full in [
            BatchUsage {
                retained_batches: MAX_RETAINED_BATCHES,
                ..BatchUsage::default()
            },
            BatchUsage {
                retained_items: MAX_RETAINED_BATCH_ITEMS,
                ..BatchUsage::default()
            },
            BatchUsage {
                active_batches: MAX_ACTIVE_BATCHES,
                ..BatchUsage::default()
            },
            BatchUsage {
                account_batches: MAX_ACTIVE_BATCHES_PER_ACCOUNT,
                ..BatchUsage::default()
            },
            BatchUsage {
                active_items: MAX_ACTIVE_BATCH_ITEMS,
                ..BatchUsage::default()
            },
        ] {
            assert!(!full.can_admit(1), "{full:?}");
        }
        let exact_items = BatchUsage {
            retained_items: MAX_RETAINED_BATCH_ITEMS,
            active_items: MAX_ACTIVE_BATCH_ITEMS,
            ..BatchUsage::default()
        };
        assert!(exact_items.can_admit(0));
        assert!(BatchUsage::default().can_admit(MAX_ACTIVE_BATCH_ITEMS));
        assert!(!BatchUsage::default().can_admit(MAX_ACTIVE_BATCH_ITEMS + 1));
    }

    #[test]
    fn admission_item_totals_cannot_wrap_into_available_room() {
        assert!(BatchUsage::default().requires_retirement(usize::MAX));
        assert!(!BatchUsage::default().can_admit(usize::MAX));
        let retained = BatchUsage {
            retained_items: usize::MAX,
            ..BatchUsage::default()
        };
        assert!(retained.requires_retirement(1));
        let active = BatchUsage {
            active_items: usize::MAX,
            ..BatchUsage::default()
        };
        assert!(!active.can_admit(1));
    }

    #[tokio::test]
    async fn mixed_status_reservations_and_cancellation_keep_uncertain_results() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let statuses = [
            UploadStatus::Pending,
            UploadStatus::InProgress,
            UploadStatus::Complete,
            UploadStatus::Failed,
            UploadStatus::Unknown,
            UploadStatus::Cancelled,
        ];
        let ticket = store
            .create(
                owner.clone(),
                "local".into(),
                statuses
                    .iter()
                    .enumerate()
                    .map(|(index, _)| (format!("{index}.txt"), (format!("{index}.txt"), 4)))
                    .collect(),
            )
            .await
            .unwrap();
        for (index, status) in statuses.into_iter().enumerate() {
            store
                .finish_with_status(
                    &ticket,
                    &format!("{index}.txt"),
                    status,
                    operation_for_status(status),
                )
                .await;
        }
        assert_eq!(
            store.reserved_paths("local").await,
            HashSet::from(["0.txt".into(), "1.txt".into(), "4.txt".into()])
        );
        assert!(store.reserved_paths("other").await.is_empty());
        store.cancel(&ticket, &owner, "local", None).await.unwrap();
        let result = store.status(&ticket, &owner, "local").await.unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.status)
                .collect::<Vec<_>>(),
            vec![
                UploadStatus::Cancelled,
                UploadStatus::InProgress,
                UploadStatus::Complete,
                UploadStatus::Cancelled,
                UploadStatus::Unknown,
                UploadStatus::Cancelled,
            ]
        );
        for (item, before) in result.items.iter().zip(statuses) {
            assert_eq!(
                item.operation,
                if item.path == "0.txt" || item.path == "3.txt" {
                    None
                } else {
                    operation_for_status(before)
                }
            );
        }
        assert_eq!(
            store.reserved_paths("local").await,
            HashSet::from(["1.txt".into(), "4.txt".into()])
        );
    }

    #[tokio::test]
    async fn persistent_restart_preserves_all_status_evidence_without_reopening_uploads() {
        let directory = crate::test_support::TestDirectory::new("upload-status-matrix-restart");
        let config_path = directory.path().join("config.json");
        let store = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        let owner = subject("owner");
        let statuses = [
            UploadStatus::Pending,
            UploadStatus::InProgress,
            UploadStatus::Complete,
            UploadStatus::Failed,
            UploadStatus::Unknown,
            UploadStatus::Cancelled,
        ];
        let ticket = store
            .create_for_account(
                owner,
                "user:1".into(),
                "local".into(),
                statuses
                    .iter()
                    .enumerate()
                    .map(|(index, _)| (format!("{index}.txt"), (format!("{index}.txt"), 4)))
                    .collect(),
            )
            .await
            .unwrap();
        for (index, status) in statuses.into_iter().enumerate() {
            // Leave the first item without a sidecar, as an unstarted ticket.
            if index != 0 {
                store
                    .finish_with_status(
                        &ticket,
                        &format!("{index}.txt"),
                        status,
                        operation_for_status(status),
                    )
                    .await;
            }
        }
        drop(store);
        let restored = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        let result = restored
            .status_for_account(&ticket, &subject("new-session"), "user:1", "local")
            .await
            .unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.status)
                .collect::<Vec<_>>(),
            vec![
                UploadStatus::Cancelled,
                UploadStatus::Unknown,
                UploadStatus::Complete,
                UploadStatus::Failed,
                UploadStatus::Unknown,
                UploadStatus::Cancelled,
            ]
        );
        for (item, before) in result.items.iter().zip(statuses) {
            assert_eq!(item.operation, operation_for_status(before));
        }
        assert_eq!(
            restored.reserved_paths("local").await,
            HashSet::from(["1.txt".into(), "4.txt".into()])
        );
    }

    #[tokio::test]
    async fn storage_edit_invalidates_a_plan_before_it_creates_a_ticket() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let planning_epoch = store.invalidation_epoch.load(Ordering::Acquire);
        store.invalidate_storage("local", None).await;

        let result = store
            .create_for_account_with_namespace_planned(
                subject("session"),
                "user:1".into(),
                "local".into(),
                items(),
                None,
                UploadPlanningFence {
                    epoch: planning_epoch,
                    guard: store.prepare_gate.clone().lock_owned().await,
                },
            )
            .await;

        assert!(matches!(result, Err(AppError::Conflict(_))));
        assert!(store.batches.lock().await.is_empty());
    }

    #[tokio::test]
    async fn disconnected_waiter_does_not_orphan_a_persisted_ticket() {
        let directory = crate::test_support::TestDirectory::new("upload-ticket-owner");
        let store = UploadBatchStore::load_persistent(
            &directory.path().join("config.json"),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        let locked_batches = store.batches.lock().await;
        let planning_guard = store.prepare_gate.clone().lock_owned().await;
        let waiter_store = store.clone();
        let initial_owners = Arc::strong_count(&store.batches);
        let waiter = tokio::spawn(async move {
            waiter_store
                .create_for_account_with_namespace_planned(
                    subject("session"),
                    "user:1".into(),
                    "local".into(),
                    items(),
                    None,
                    UploadPlanningFence {
                        epoch: 0,
                        guard: planning_guard,
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&store.batches) == initial_owners {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        waiter.abort();
        assert!(store.prepare_gate.try_lock().is_err());
        drop(locked_batches);

        let ticket = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(ticket) = store.batches.lock().await.keys().next().cloned() {
                    break ticket;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // A reservation is visible before disk I/O; the owned creator releases
        // the planning fence only after the durable manifest has been published.
        let _planning = tokio::time::timeout(Duration::from_secs(2), store.prepare_gate.lock())
            .await
            .unwrap();
        assert!(directory
            .path()
            .join(".ycloud-system/upload-results")
            .join(ticket)
            .join("manifest.json")
            .exists());
    }

    #[tokio::test]
    async fn persistent_ticket_survives_restart_but_requires_same_account() {
        let directory = crate::test_support::TestDirectory::new("persistent-upload-ticket");
        let config_path = directory.path().join("config.json");
        let store = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        let ticket = store
            .create_for_account(
                subject("old-session"),
                "user:1".into(),
                "local".into(),
                items(),
            )
            .await
            .unwrap();
        store
            .begin(
                &ticket,
                &subject("old-session"),
                "local",
                "folder/one.txt",
                11,
            )
            .await
            .unwrap();
        store.finish(&ticket, "folder/one.txt", true).await;
        assert_eq!(
            store
                .status_for_account(&ticket, &subject("new-session"), "user:1", "local")
                .await
                .unwrap()
                .items[0]
                .status,
            UploadStatus::Complete
        );
        let interrupted = store
            .create_for_account(
                subject("old-session"),
                "user:1".into(),
                "local".into(),
                HashMap::from([(
                    "folder/interrupted.txt".into(),
                    ("folder/interrupted.txt".into(), 3),
                )]),
            )
            .await
            .unwrap();
        store
            .begin(
                &interrupted,
                &subject("old-session"),
                "local",
                "folder/interrupted.txt",
                3,
            )
            .await
            .unwrap();
        drop(store);

        let restored = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        let result = restored
            .status_for_account(&ticket, &subject("new-session"), "user:1", "local")
            .await
            .unwrap();
        assert_eq!(result.items[0].status, UploadStatus::Complete);
        assert_eq!(
            restored
                .status_for_account(&interrupted, &subject("new-session"), "user:1", "local")
                .await
                .unwrap()
                .items[0]
                .status,
            UploadStatus::Unknown
        );
        assert!(matches!(
            restored
                .status_for_account(&ticket, &subject("other-session"), "user:2", "local")
                .await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            restored
                .begin(
                    &ticket,
                    &subject("new-session"),
                    "local",
                    "folder/one.txt",
                    11
                )
                .await,
            Err(AppError::Forbidden)
        ));
    }

    #[tokio::test]
    async fn runtime_sweep_retires_expired_persistent_results() {
        let directory = crate::test_support::TestDirectory::new("upload-result-runtime-sweep");
        let config_path = directory.path().join("config.json");
        let store = UploadBatchStore::load_persistent(&config_path, Duration::from_secs(60))
            .await
            .unwrap();
        let ticket = store
            .create_for_account(subject("owner"), "user:1".into(), "local".into(), items())
            .await
            .unwrap();
        store
            .cancel(&ticket, &subject("owner"), "local", None)
            .await
            .unwrap();
        store
            .batches
            .lock()
            .await
            .get_mut(&ticket)
            .unwrap()
            .expires_at = Instant::now();
        store.sweep_expired().await;
        let persisted = directory
            .path()
            .join(".ycloud-system/upload-results")
            .join(&ticket);
        tokio::time::timeout(Duration::from_secs(2), async {
            while persisted.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!store.batches.lock().await.contains_key(&ticket));
    }

    #[tokio::test]
    async fn retired_namespace_stays_owned_while_an_upload_is_unsettled() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let namespace = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let ticket = store
            .create_for_account_with_namespace(
                subject("owner"),
                "user:1".into(),
                "old-s3".into(),
                items(),
                Some(namespace.into()),
            )
            .await
            .unwrap();
        store
            .begin(&ticket, &subject("owner"), "old-s3", "folder/one.txt", 11)
            .await
            .unwrap();
        assert!(store.has_unsettled_namespace("old-s3", namespace).await);
        assert!(!store.has_unsettled_namespace("new-s3", namespace).await);
        store.finish(&ticket, "folder/one.txt", true).await;
        assert!(!store.has_unsettled_namespace("old-s3", namespace).await);
    }

    #[tokio::test]
    async fn expiry_releases_unstarted_batches_but_retains_running_and_uncertain_results() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let pending = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        let running = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        store
            .begin(&running, &owner, "local", "folder/one.txt", 11)
            .await
            .unwrap();
        for batch in store.batches.lock().await.values_mut() {
            batch.expires_at = Instant::now() - Duration::from_secs(1);
        }
        assert!(matches!(
            store.status(&pending, &owner, "local").await,
            Err(AppError::UploadBatchExpired)
        ));
        assert_eq!(
            store.status(&running, &owner, "local").await.unwrap().items[0].status,
            UploadStatus::InProgress
        );
        let mut guard =
            UploadExecutionGuard::new(store.clone(), running.clone(), "folder/one.txt".into());
        let result: AppResult<()> =
            Err(AppError::RequestTimeout
                .with_operation(CommitState::Unknown, CleanupState::Unknown));
        guard.finish(&result).await;
        store
            .batches
            .lock()
            .await
            .get_mut(&running)
            .unwrap()
            .expires_at = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            store.status(&running, &owner, "local").await.unwrap().items[0].status,
            UploadStatus::Unknown
        );
        assert!(store
            .reserved_paths("local")
            .await
            .contains("folder/one.txt"));
        assert!(store.ensure_storage_idle("local").await.is_err());
    }

    #[tokio::test]
    async fn interrupted_execution_retains_unknown_but_never_overwrites_a_completed_result() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        for path in ["folder/one.txt", "folder/two.txt"] {
            let size = if path.ends_with("one.txt") { 11 } else { 22 };
            store
                .begin(&ticket, &owner, "local", path, size)
                .await
                .unwrap();
            let guard = UploadExecutionGuard::new(store.clone(), ticket.clone(), path.into());
            if path.ends_with("two.txt") {
                store.finish(&ticket, path, true).await;
            }
            drop(guard);
        }
        tokio::task::yield_now().await;
        let status = store.status(&ticket, &owner, "local").await.unwrap();
        assert_eq!(status.items[0].status, UploadStatus::Unknown);
        assert_eq!(status.items[1].status, UploadStatus::Complete);
    }

    #[test]
    fn duplicate_names_continue_after_highest_number_without_filling_gaps() {
        let names = HashSet::from(
            ["报告.txt", "报告 (2).txt", "报告 (9).txt", "报告 (99).zip"].map(str::to_owned),
        );
        assert_eq!(
            numbered_upload_name("报告.txt", &names).unwrap(),
            "报告 (10).txt"
        );
        assert_eq!(
            numbered_upload_name("报告 (2).txt", &names).unwrap(),
            "报告 (10).txt"
        );
        assert_eq!(
            numbered_upload_name(".gitkeep", &HashSet::from([".gitkeep (3)".into()])).unwrap(),
            ".gitkeep (4)"
        );
        assert_eq!(
            numbered_upload_name(
                "archive.tar.gz",
                &HashSet::from(["archive.tar (5).gz".into()])
            )
            .unwrap(),
            "archive.tar (6).gz"
        );
        assert!(numbered_upload_name(
            "file.txt",
            &HashSet::from([format!("file ({}).txt", u64::MAX)])
        )
        .is_err());
    }

    #[tokio::test]
    async fn concurrent_preparation_reserves_distinct_names_and_preserves_old_bytes() {
        use crate::test_support::{app_state, TestDirectory};
        use axum::http::{header, HeaderValue};
        let directory = TestDirectory::new("numbered-upload");
        let state = app_state(&directory, crate::config::ConfigFile::with_test_storage()).await;
        let root = state.config.storage_path.clone();
        tokio::fs::write(root.join("file.txt"), b"old")
            .await
            .unwrap();
        tokio::fs::write(root.join("file (5).txt"), b"five")
            .await
            .unwrap();
        let token = state.sessions.create().await;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("session={token}")).unwrap(),
        );
        let prepare = || {
            prepare_upload_batch(
                State(state.clone()),
                headers.clone(),
                Query(FileQuery::default()),
                Json(PrepareUploadBatchRequest {
                    items: vec![PrepareUploadItem {
                        path: "file.txt".into(),
                        size: 3,
                    }],
                }),
            )
        };
        let (first, second) = tokio::join!(prepare(), prepare());
        let first = first.unwrap().0;
        let second = second.unwrap().0;
        let names: HashSet<_> = [first.items[0].path.clone(), second.items[0].path.clone()]
            .into_iter()
            .collect();
        assert_eq!(
            names,
            HashSet::from(["file (6).txt".to_owned(), "file (7).txt".to_owned()])
        );
        let backend = state.storage_backend("primary").await.unwrap();
        backend
            .upload_new_file(
                &first.items[0].path,
                axum::body::Body::from("new"),
                Some(3),
                100,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(root.join("file.txt")).await.unwrap(),
            b"old"
        );
        assert_eq!(
            tokio::fs::read(root.join(&first.items[0].path))
                .await
                .unwrap(),
            b"new"
        );
        assert!(backend
            .upload_new_file(
                "file.txt",
                axum::body::Body::from("bad"),
                Some(3),
                100,
                None
            )
            .await
            .is_err());
        assert_eq!(
            tokio::fs::read(root.join("file.txt")).await.unwrap(),
            b"old"
        );
    }

    #[tokio::test]
    async fn upload_waiter_cancellation_still_registers_completion_and_metadata_edit_preserves_owner(
    ) {
        use axum::{body::Body, http::header};
        let directory = crate::test_support::TestDirectory::new("upload-owned-result");
        let state = crate::test_support::app_state(
            &directory,
            crate::config::ConfigFile::with_test_storage(),
        )
        .await;
        let token = state.sessions.create().await;
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, format!("session={token}").parse().unwrap());
        headers.insert(header::CONTENT_LENGTH, "3".parse().unwrap());
        let owner = auth::current_request_subject(&state, &headers)
            .await
            .unwrap();
        let prepared = prepare_upload_batch(
            State(state.clone()),
            headers.clone(),
            Query(FileQuery::default()),
            Json(PrepareUploadBatchRequest {
                items: vec![PrepareUploadItem {
                    path: "owned.txt".into(),
                    size: 3,
                }],
            }),
        )
        .await
        .unwrap()
        .0;
        let ticket = prepared.ticket;
        let (send, receive) = tokio::sync::oneshot::channel::<bytes::Bytes>();
        let body = Body::from_stream(futures_util::stream::once(async move {
            receive.await.map_err(std::io::Error::other)
        }));
        let waiter = tokio::spawn({
            let state = state.clone();
            let ticket = ticket.clone();
            async move {
                crate::api::upload_file(
                    State(state),
                    headers,
                    Query(FileQuery {
                        path: Some("owned.txt".into()),
                        storage_id: Some("primary".into()),
                        batch: Some(ticket),
                    }),
                    body,
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if state
                    .upload_batches
                    .status(&ticket, &owner, "primary")
                    .await
                    .unwrap()
                    .items[0]
                    .status
                    == UploadStatus::InProgress
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let path = state.config.storage_path.to_str().unwrap().to_owned();
        state
            .update_local_storage(
                "primary",
                crate::state::LocalStorageEdit {
                    name: "Rename during upload".into(),
                    path,
                    capacity_limit_bytes: None,
                    enabled: true,
                    guest_access: true.into(),
                    expected_revision: None,
                },
            )
            .await
            .unwrap();
        waiter.abort();
        waiter.await.unwrap_err();
        send.send(bytes::Bytes::from_static(b"new")).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if state
                    .upload_batches
                    .status(&ticket, &owner, "primary")
                    .await
                    .unwrap()
                    .items[0]
                    .status
                    == UploadStatus::Complete
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(state.config.storage_path.join("owned.txt"))
                .await
                .unwrap(),
            b"new"
        );
    }

    #[tokio::test]
    async fn twelve_users_uploading_the_same_name_keep_every_payload() {
        use crate::config::{StoragePermission, UserAccount};
        use crate::test_support::{app_state, TestDirectory};
        use axum::{body::Body, http::header};

        let directory = TestDirectory::new("multiuser-upload");
        let mut config = crate::config::ConfigFile::with_test_storage();
        let password_hash = crate::config::hash_password("isolated-test-password");
        for index in 0..12 {
            let id = format!("user-{index}");
            config.user_accounts.push(UserAccount {
                id: id.clone(),
                username: id,
                password_hash: password_hash.clone(),
                enabled: true,
                permissions: vec![StoragePermission {
                    storage_id: "primary".into(),
                    browse: true,
                    upload: true,
                    ..Default::default()
                }],
            });
        }
        let state = app_state(&directory, config).await;
        let root = state.config.storage_path.clone();
        tokio::fs::write(root.join("same.txt"), b"original")
            .await
            .unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(12));
        let mut uploads = tokio::task::JoinSet::new();
        for index in 0..12 {
            let state = state.clone();
            let barrier = barrier.clone();
            uploads.spawn(async move {
                let token = state.sessions.create_user(format!("user-{index}")).await;
                let payload = format!("payload-{index}");
                let mut headers = HeaderMap::new();
                headers.insert(header::COOKIE, format!("session={token}").parse().unwrap());
                headers.insert(
                    header::CONTENT_LENGTH,
                    payload.len().to_string().parse().unwrap(),
                );
                barrier.wait().await;
                let prepared = prepare_upload_batch(
                    State(state.clone()),
                    headers.clone(),
                    Query(FileQuery::default()),
                    Json(PrepareUploadBatchRequest {
                        items: vec![PrepareUploadItem {
                            path: "same.txt".into(),
                            size: payload.len() as u64,
                        }],
                    }),
                )
                .await
                .unwrap()
                .0;
                let path = prepared.items[0].path.clone();
                let _ = crate::api::upload_file(
                    State(state),
                    headers,
                    Query(FileQuery {
                        path: Some(path.clone()),
                        storage_id: Some("primary".into()),
                        batch: Some(prepared.ticket),
                    }),
                    Body::from(payload.clone()),
                )
                .await
                .unwrap();
                (path, payload)
            });
        }
        let results = tokio::time::timeout(Duration::from_secs(30), async {
            let mut names = HashSet::new();
            while let Some(result) = uploads.join_next().await {
                let (path, payload) = result.unwrap();
                assert!(
                    names.insert(path.clone()),
                    "two users received the same name"
                );
                assert_eq!(
                    tokio::fs::read(root.join(path)).await.unwrap(),
                    payload.as_bytes()
                );
            }
            names
        })
        .await
        .expect("concurrent uploads must settle");
        assert_eq!(results.len(), 12);
        assert_eq!(
            tokio::fs::read(root.join("same.txt")).await.unwrap(),
            b"original"
        );
    }

    fn subject(value: &str) -> RequestSubject {
        RequestSubject::Session(value.into())
    }

    fn items() -> HashMap<String, (String, u64)> {
        HashMap::from([
            (
                "folder/one.txt".to_string(),
                ("folder/one.txt".to_string(), 11),
            ),
            (
                "folder/two.txt".to_string(),
                ("folder/two.txt".to_string(), 22),
            ),
        ])
    }

    #[tokio::test]
    async fn ticket_is_bound_to_storage_path_and_size() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();

        assert!(matches!(
            store
                .begin(&ticket, &owner, "other", "folder/one.txt", 11)
                .await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            store
                .begin(&ticket, &subject("attacker"), "local", "folder/one.txt", 11,)
                .await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            store
                .begin(&ticket, &owner, "local", "folder/unknown.txt", 11)
                .await,
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 12)
                .await,
            Err(AppError::BadRequest(_))
        ));
        store
            .begin(&ticket, &owner, "local", "folder/one.txt", 11)
            .await
            .unwrap();
        assert!(matches!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await,
            Err(AppError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn failed_item_can_retry_and_completed_batch_is_idempotent() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();

        assert_eq!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await
                .unwrap(),
            UploadBegin::Start
        );
        store.finish(&ticket, "folder/one.txt", false).await;
        store
            .begin(&ticket, &owner, "local", "folder/one.txt", 11)
            .await
            .unwrap();
        store.finish(&ticket, "folder/one.txt", true).await;

        store
            .begin(&ticket, &owner, "local", "folder/two.txt", 22)
            .await
            .unwrap();
        store.finish(&ticket, "folder/two.txt", true).await;
        assert_eq!(
            store
                .begin(&ticket, &owner, "local", "folder/two.txt", 22)
                .await
                .unwrap(),
            UploadBegin::AlreadyComplete(None)
        );
        let status = store.status(&ticket, &owner, "local").await.unwrap();
        assert!(status
            .items
            .iter()
            .all(|item| item.status == UploadStatus::Complete));
    }

    #[tokio::test]
    async fn uncertain_or_committed_items_are_not_reopened_for_retry() {
        use crate::error::{CleanupState, CommitState};
        for commit in [CommitState::Unknown, CommitState::Committed] {
            let store = UploadBatchStore::new(Duration::from_secs(60));
            let owner = subject("owner");
            let ticket = store
                .create(owner.clone(), "local".into(), items())
                .await
                .unwrap();
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await
                .unwrap();
            let result: AppResult<()> =
                Err(AppError::internal("pending").with_operation(commit, CleanupState::Pending));
            store
                .finish_result(&ticket, "folder/one.txt", &result)
                .await;
            assert_eq!(
                store.unknown_items().await.len(),
                usize::from(commit == CommitState::Unknown)
            );
            let replay = store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await;
            if commit == CommitState::Unknown {
                assert!(matches!(replay, Err(AppError::Operation { .. })));
            } else {
                assert!(matches!(
                    replay,
                    Ok(UploadBegin::AlreadyComplete(Some(outcome)))
                        if outcome.commit == CommitState::Committed
                ));
            }
            // Other items in this batch remain independent.
            store
                .begin(&ticket, &owner, "local", "folder/two.txt", 22)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_is_storage_bound_and_idempotent() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();

        assert!(matches!(
            store.cancel(&ticket, &owner, "other", None).await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            store
                .cancel(&ticket, &subject("attacker"), "local", None)
                .await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            store.status(&ticket, &subject("attacker"), "local").await,
            Err(AppError::Forbidden)
        ));
        let selected = HashSet::from(["folder/one.txt".to_string()]);
        store
            .cancel(&ticket, &owner, "local", Some(&selected))
            .await
            .unwrap();
        store
            .cancel(&ticket, &owner, "local", Some(&selected))
            .await
            .unwrap();
        assert!(matches!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await,
            Err(AppError::Conflict(_))
        ));
        assert_eq!(
            store
                .begin(&ticket, &owner, "local", "folder/two.txt", 22)
                .await
                .unwrap(),
            UploadBegin::Start
        );
        let status = store.status(&ticket, &owner, "local").await.unwrap();
        assert_eq!(status.items[0].path, "folder/one.txt");
        assert_eq!(status.items[0].status, UploadStatus::Cancelled);
        assert_eq!(status.items[1].status, UploadStatus::InProgress);
    }

    #[tokio::test]
    async fn concurrent_confirmation_starts_an_upload_only_once() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(32));
        let mut attempts = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let (store, owner, ticket, barrier) = (
                store.clone(),
                owner.clone(),
                ticket.clone(),
                barrier.clone(),
            );
            attempts.spawn(async move {
                barrier.wait().await;
                store
                    .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                    .await
            });
        }
        let mut started = 0;
        let mut conflicts = 0;
        while let Some(result) = attempts.join_next().await {
            match result.unwrap() {
                Ok(UploadBegin::Start) => started += 1,
                Err(AppError::Conflict(_)) => conflicts += 1,
                other => panic!("unexpected upload admission: {other:?}"),
            }
        }
        assert_eq!(started, 1);
        assert_eq!(conflicts, 31);
        store.finish(&ticket, "folder/one.txt", true).await;
        assert_eq!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await
                .unwrap(),
            UploadBegin::AlreadyComplete(None)
        );
    }

    #[tokio::test]
    async fn a_full_batch_queue_rejects_admission_and_recovers_after_cancellation() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let mut tickets = Vec::new();
        for index in 0..MAX_ACTIVE_BATCHES {
            tickets.push(
                store
                    .create_for_account(
                        owner.clone(),
                        format!("account-{index}"),
                        "local".into(),
                        items(),
                    )
                    .await
                    .unwrap(),
            );
        }
        assert!(matches!(
            store.create(owner.clone(), "local".into(), items()).await,
            Err(AppError::TooManyRequests)
        ));
        store
            .cancel(&tickets[0], &owner, "local", None)
            .await
            .unwrap();
        store.create(owner, "local".into(), items()).await.unwrap();
    }

    #[tokio::test]
    async fn distinct_accounts_share_admission_and_cancel_releases_an_account_slot() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("first-session");
        let mut tickets = Vec::new();
        for _ in 0..4 {
            tickets.push(
                store
                    .create_for_account(owner.clone(), "account-a".into(), "local".into(), items())
                    .await
                    .unwrap(),
            );
        }
        let other = subject("other-session");
        store
            .create_for_account(other, "account-b".into(), "local".into(), items())
            .await
            .unwrap();
        store
            .cancel(&tickets[0], &owner, "local", None)
            .await
            .unwrap();
        store
            .create_for_account(owner, "account-a".into(), "local".into(), items())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn confirmed_upload_releases_the_old_reservation() {
        let store = UploadBatchStore::new(Duration::from_secs(60));
        let owner = subject("owner");
        let ticket = store
            .create(owner.clone(), "local".into(), items())
            .await
            .unwrap();
        store
            .begin(&ticket, &owner, "local", "folder/one.txt", 11)
            .await
            .unwrap();
        store
            .finish_result::<()>(
                &ticket,
                "folder/one.txt",
                &Err(AppError::internal("result unavailable")
                    .with_operation(CommitState::Unknown, CleanupState::Unknown)),
            )
            .await;
        let review = store.unknown_items().await.pop().unwrap();
        store.confirm_unknown_committed(&review).await.unwrap();
        let status = store.status(&ticket, &owner, "local").await.unwrap();
        let item = status
            .items
            .iter()
            .find(|item| item.path == "folder/one.txt")
            .unwrap();
        assert_eq!(item.status, UploadStatus::Complete);
        assert!(item.operation.is_none());
        store.ensure_storage_idle("local").await.unwrap();
    }
}
