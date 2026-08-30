use std::{
    collections::HashMap,
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
    error::{AppError, AppResult},
    file_access::{
        check_folder_locks, ensure_non_root, ensure_storage_action, resolve_share,
        share_storage_path, FileQuery, StorageAction,
    },
    state::AppState,
    storage::StorageService,
};

const MAX_ACTIVE_BATCHES: usize = 32;
const MAX_ACTIVE_BATCH_ITEMS: usize = 20_000;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum UploadStatus {
    Pending,
    InProgress,
    Complete,
}

struct UploadItem {
    size: u64,
    status: UploadStatus,
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
        items: HashMap<String, u64>,
    ) -> AppResult<String> {
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch.expires_at > now);
        if batches.len() >= MAX_ACTIVE_BATCHES {
            return Err(AppError::TooManyRequests);
        }
        let active_items = batches
            .values()
            .try_fold(0_usize, |total, batch| total.checked_add(batch.items.len()))
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
                    .map(|(path, size)| {
                        (
                            path,
                            UploadItem {
                                size,
                                status: UploadStatus::Pending,
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
    ) -> AppResult<()> {
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
        if item.status != UploadStatus::Pending {
            return Err(AppError::Conflict("上传目标已开始或已经完成".into()));
        }
        item.status = UploadStatus::InProgress;
        Ok(())
    }

    pub async fn cancel(
        &self,
        token: &str,
        subject: &RequestSubject,
        storage_id: &str,
    ) -> AppResult<()> {
        let mut batches = self.batches.lock().await;
        let now = Instant::now();
        batches.retain(|_, batch| batch.expires_at > now);
        let Some(batch) = batches.get(token) else {
            return Ok(());
        };
        if &batch.subject != subject || batch.storage_id != storage_id {
            return Err(AppError::Forbidden);
        }
        batches.remove(token);
        Ok(())
    }

    pub async fn finish(&self, token: &str, path: &str, succeeded: bool) {
        let mut batches = self.batches.lock().await;
        let Some(batch) = batches.get_mut(token) else {
            return;
        };
        let Some(item) = batch.items.get_mut(path) else {
            return;
        };
        item.status = if succeeded {
            UploadStatus::Complete
        } else {
            UploadStatus::Pending
        };
        if batch
            .items
            .values()
            .all(|item| item.status == UploadStatus::Complete)
        {
            batches.remove(token);
        }
    }
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
        if validated.insert(storage_path, item.size).is_some() {
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
    state
        .upload_batches
        .cancel(&body.ticket, &subject, &share.storage_id)
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(value: &str) -> RequestSubject {
        RequestSubject::Session(value.into())
    }

    fn items() -> HashMap<String, u64> {
        HashMap::from([
            ("folder/one.txt".to_string(), 11),
            ("folder/two.txt".to_string(), 22),
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
    async fn failed_item_can_retry_and_completed_batch_is_removed() {
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
        assert!(matches!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await,
            Err(AppError::BadRequest(_))
        ));
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
            store.cancel(&ticket, &owner, "other").await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            store.cancel(&ticket, &subject("attacker"), "local").await,
            Err(AppError::Forbidden)
        ));
        store.cancel(&ticket, &owner, "local").await.unwrap();
        store.cancel(&ticket, &owner, "local").await.unwrap();
        assert!(matches!(
            store
                .begin(&ticket, &owner, "local", "folder/one.txt", 11)
                .await,
            Err(AppError::BadRequest(_))
        ));
    }
}
