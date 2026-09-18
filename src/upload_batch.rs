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
}

struct UploadBatch {
    subject: RequestSubject,
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
    pub fn new(ttl: Duration) -> Self {
        Self {
            batches: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub async fn create(
        &self,
        subject: RequestSubject,
        storage_id: String,
        items: HashMap<String, (String, u64)>,
    ) -> AppResult<String> {
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch.expires_at > now);
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
        batches.retain(|_, batch| batch.expires_at > now);
        let batch = batches
            .get_mut(token)
            .ok_or_else(|| AppError::BadRequest("上传批次不存在或已过期".into()))?;
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
        batches.retain(|_, batch| batch.expires_at > now);
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
        batches.retain(|_, batch| batch.expires_at > now);
        let batch = batches
            .get(token)
            .ok_or_else(|| AppError::BadRequest("上传批次不存在或已过期".into()))?;
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

fn item_is_terminal(item: &UploadItem) -> bool {
    matches!(
        item.status,
        UploadStatus::Complete | UploadStatus::Cancelled
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
    if body.items.is_empty() || body.items.len() > policy.max_upload_batch_entries {
        return Err(AppError::PayloadTooLarge);
    }
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
        let request_path = StorageService::normalize_relative(&item.path)?;
        ensure_non_root(&request_path)?;
        let name = request_path.rsplit('/').next().unwrap_or_default();
        if name.trim() != name
            || name
                .chars()
                .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(AppError::BadRequest("上传目标文件名无效".into()));
        }
        let storage_path = share_storage_path(&share, &request_path);
        check_folder_locks(&state, &headers, &share.storage_id, &storage_path).await?;
        if validated
            .insert(storage_path, (request_path, item.size))
            .is_some()
        {
            return Err(AppError::Conflict("批量上传包含重复目标路径".into()));
        }
    }
    let ticket = state
        .upload_batches
        .create(subject, share.storage_id, validated)
        .await?;
    Ok(Json(PrepareUploadBatchResponse { ticket }))
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
}
