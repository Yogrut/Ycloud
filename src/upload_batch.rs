use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
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
const MAX_ACTIVE_BATCH_ITEMS: usize = 20_000;
const MAX_RETAINED_BATCHES: usize = 128;
const MAX_RETAINED_BATCH_ITEMS: usize = 40_000;

#[derive(Clone)]
pub struct UploadBatchStore {
    batches: Arc<Mutex<HashMap<String, UploadBatch>>>,
    ttl: Duration,
    prepare_gate: Arc<Mutex<()>>,
}

struct UploadBatch {
    subject: RequestSubject,
    account: String,
    storage_id: String,
    expires_at: Instant,
    items: HashMap<String, UploadItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Pending,
    InProgress,
    Complete,
    Failed,
    Unknown,
    Cancelled,
}

struct UploadItem {
    request_path: String,
    size: u64,
    status: UploadStatus,
    operation: Option<OperationOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadBegin {
    Start,
    AlreadyComplete(Option<OperationOutcome>),
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
            let mut batches = store.batches.lock().await;
            if let Some(item) = batches
                .get_mut(&token)
                .and_then(|batch| batch.items.get_mut(&path))
            {
                if item.status == UploadStatus::InProgress {
                    item.status = UploadStatus::Unknown;
                    item.operation = AppError::internal("upload execution owner interrupted")
                        .with_operation(CommitState::Unknown, CleanupState::Unknown)
                        .operation();
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
                        ticket: ticket.clone(),
                        storage_id: batch.storage_id.clone(),
                        path: path.clone(),
                        size: item.size,
                        operation: item.operation,
                    })
            })
            .collect()
    }

    pub(crate) async fn resolve_unknown(
        &self,
        review: &UnknownUpload,
        committed: bool,
    ) -> AppResult<()> {
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
        item.status = if committed {
            UploadStatus::Complete
        } else {
            UploadStatus::Failed
        };
        item.operation = if committed {
            None
        } else {
            AppError::ClientClosedRequest
                .with_operation(CommitState::NotCommitted, CleanupState::Complete)
                .operation()
        };
        batch.expires_at = Instant::now() + self.ttl;
        Ok(())
    }

    #[cfg(test)]
    pub async fn ensure_storage_idle(&self, storage_id: &str) -> AppResult<()> {
        if self.batches.lock().await.values().any(|batch| {
            batch.storage_id == storage_id
                && batch.items.values().any(|item| {
                    matches!(
                        item.status,
                        UploadStatus::InProgress | UploadStatus::Unknown
                    )
                })
        }) {
            return Err(AppError::Conflict(
                "存储仍有正在执行或后台自动恢复的上传，请稍后重试".into(),
            ));
        }
        Ok(())
    }

    pub async fn invalidate_storage(&self, storage_id: &str) {
        self.batches
            .lock()
            .await
            .retain(|_, batch| batch.storage_id != storage_id);
    }
    pub fn new(ttl: Duration) -> Self {
        Self {
            batches: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            prepare_gate: Arc::new(Mutex::new(())),
        }
    }

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

    async fn create_for_account(
        &self,
        subject: RequestSubject,
        account: String,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
    ) -> AppResult<String> {
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch_is_retained(batch, now));
        while batches.len() >= MAX_RETAINED_BATCHES
            || retained_item_count(&batches).saturating_add(items.len()) > MAX_RETAINED_BATCH_ITEMS
        {
            let Some(expired_first) = batches
                .iter()
                .filter(|(_, batch)| batch_is_terminal(batch))
                .min_by_key(|(_, batch)| batch.expires_at)
                .map(|(token, _)| token.clone())
            else {
                break;
            };
            batches.remove(&expired_first);
        }
        if batches.len() >= MAX_RETAINED_BATCHES
            || retained_item_count(&batches).saturating_add(items.len()) > MAX_RETAINED_BATCH_ITEMS
        {
            return Err(AppError::TooManyRequests);
        }
        let active_batches = batches
            .values()
            .filter(|batch| !batch_is_terminal(batch))
            .count();
        if active_batches >= MAX_ACTIVE_BATCHES {
            return Err(AppError::TooManyRequests);
        }
        if batches
            .values()
            .filter(|batch| batch.account == account && !batch_is_terminal(batch))
            .count()
            >= 4
        {
            return Err(AppError::TooManyRequests);
        }
        let active_items = batches
            .values()
            .flat_map(|batch| batch.items.values())
            .filter(|item| !item_is_terminal(item))
            .try_fold(0_usize, |total, _| total.checked_add(1))
            .ok_or(AppError::TooManyRequests)?;
        if active_items.saturating_add(items.len()) > MAX_ACTIVE_BATCH_ITEMS {
            return Err(AppError::TooManyRequests);
        }
        let token = Uuid::new_v4().to_string();
        batches.insert(
            token.clone(),
            UploadBatch {
                subject,
                account,
                storage_id,
                expires_at: now + self.ttl,
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
            },
        );
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
                    .filter(|(_, item)| {
                        !matches!(
                            item.status,
                            UploadStatus::Complete | UploadStatus::Cancelled | UploadStatus::Failed
                        )
                    })
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
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch_is_retained(batch, now));
        let batch = batches.get_mut(token).ok_or(AppError::UploadBatchExpired)?;
        if &batch.subject != subject || batch.storage_id != storage_id {
            return Err(AppError::Forbidden);
        }
        let item = batch
            .items
            .get_mut(path)
            .ok_or_else(|| AppError::BadRequest("上传目标不属于该批次".into()))?;
        if item.size != size {
            return Err(AppError::BadRequest("上传文件大小与批次声明不一致".into()));
        }
        match item.status {
            UploadStatus::Pending | UploadStatus::Failed => {}
            UploadStatus::Complete => return Ok(UploadBegin::AlreadyComplete(item.operation)),
            UploadStatus::InProgress => {
                return Err(AppError::Conflict("上传目标正在执行".into()));
            }
            UploadStatus::Unknown => {
                let outcome = item.operation;
                return Err(AppError::Conflict(
                    "上传结果尚未确认，请先查询任务状态，不要直接重试".into(),
                )
                .with_operation(
                    outcome.map_or(CommitState::Unknown, |value| value.commit),
                    outcome.map_or(CleanupState::Unknown, |value| value.cleanup),
                ));
            }
            UploadStatus::Cancelled => {
                return Err(AppError::Conflict("上传目标已取消".into()));
            }
        }
        item.status = UploadStatus::InProgress;
        item.operation = None;
        batch.expires_at = now + self.ttl;
        Ok(UploadBegin::Start)
    }

    pub async fn cancel(
        &self,
        token: &str,
        subject: &RequestSubject,
        storage_id: &str,
        paths: Option<&HashSet<String>>,
    ) -> AppResult<()> {
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch_is_retained(batch, now));
        let Some(batch) = batches.get_mut(token) else {
            return Ok(());
        };
        if &batch.subject != subject || batch.storage_id != storage_id {
            return Err(AppError::Forbidden);
        }
        if let Some(paths) = paths {
            if paths.iter().any(|path| !batch.items.contains_key(path)) {
                return Err(AppError::BadRequest("取消目标不属于该批次".into()));
            }
        }
        for (path, item) in &mut batch.items {
            if paths.is_some_and(|paths| !paths.contains(path)) {
                continue;
            }
            if matches!(item.status, UploadStatus::Pending | UploadStatus::Failed) {
                item.status = UploadStatus::Cancelled;
                item.operation = None;
            }
        }
        batch.expires_at = now + self.ttl;
        Ok(())
    }

    pub async fn finish_result<T>(&self, token: &str, path: &str, result: &AppResult<T>) {
        let (status, operation) = match result {
            Ok(_) => (UploadStatus::Complete, None),
            Err(error) => match error.operation() {
                Some(outcome) if outcome.commit == CommitState::Committed => {
                    (UploadStatus::Complete, Some(outcome))
                }
                Some(outcome) if outcome.commit == CommitState::Unknown => {
                    (UploadStatus::Unknown, Some(outcome))
                }
                Some(outcome) if outcome.cleanup != CleanupState::Complete => {
                    (UploadStatus::Unknown, Some(outcome))
                }
                outcome => (UploadStatus::Failed, outcome),
            },
        };
        self.finish_with_status(token, path, status, operation)
            .await;
    }

    pub async fn finish(&self, token: &str, path: &str, succeeded: bool) {
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
        let mut batches = self.batches.lock().await;
        let Some(batch) = batches.get_mut(token) else {
            return;
        };
        let Some(item) = batch.items.get_mut(path) else {
            return;
        };
        item.status = status;
        item.operation = operation;
        batch.expires_at = Instant::now() + self.ttl;
    }

    pub async fn status(
        &self,
        token: &str,
        subject: &RequestSubject,
        storage_id: &str,
    ) -> AppResult<UploadBatchStatusResponse> {
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch_is_retained(batch, now));
        let batch = batches.get(token).ok_or(AppError::UploadBatchExpired)?;
        if &batch.subject != subject || batch.storage_id != storage_id {
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

#[derive(Clone, Serialize)]
pub(crate) struct UnknownUpload {
    pub ticket: String,
    pub storage_id: String,
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

fn batch_is_retained(batch: &UploadBatch, now: Instant) -> bool {
    batch.expires_at > now
        || batch.items.values().any(|item| {
            matches!(
                item.status,
                UploadStatus::InProgress | UploadStatus::Unknown
            )
        })
}

fn item_is_terminal(item: &UploadItem) -> bool {
    matches!(
        item.status,
        UploadStatus::Complete | UploadStatus::Cancelled | UploadStatus::Failed
    )
}

fn batch_is_terminal(batch: &UploadBatch) -> bool {
    batch.items.values().all(item_is_terminal)
}

fn retained_item_count(batches: &HashMap<String, UploadBatch>) -> usize {
    batches.values().fold(0_usize, |total, batch| {
        total.saturating_add(batch.items.len())
    })
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
    let share = resolve_share(&state, &headers, &query).await?;
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
    let _planning = state.upload_batches.prepare_gate.lock().await;
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
        .create_for_account(
            subject,
            crate::traffic::browser_subject(&state, &headers).await,
            share.storage_id,
            validated,
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
            .status(ticket, &subject, &share.storage_id)
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "Rename during upload".into(),
                path,
                None,
                true,
                false,
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
    async fn automatically_resolved_upload_releases_the_old_reservation() {
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
        store.resolve_unknown(&review, false).await.unwrap();
        let status = store.status(&ticket, &owner, "local").await.unwrap();
        let item = status
            .items
            .iter()
            .find(|item| item.path == "folder/one.txt")
            .unwrap();
        assert_eq!(item.status, UploadStatus::Failed);
        assert_eq!(item.operation.unwrap().commit, CommitState::NotCommitted);
        store.ensure_storage_idle("local").await.unwrap();
    }
}
