//! Authorized browser-to-S3 uploads, with bounded, server-owned sessions.
use crate::{
    auth::{self, RequestSubject},
    error::{AppError, AppResult, CommitState, OperationOutcome},
    file_access::{
        check_folder_locks, ensure_non_root, ensure_storage_action, ensure_writable, resolve_share,
        share_storage_path, FileQuery, StorageAction,
    },
    s3_backend::{DirectChannel, DirectCommand, DirectDescriptor, SignedPart},
    state::AppState,
    upload_batch::UploadBegin,
};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

type ResultSummary = Result<(), (String, Option<OperationOutcome>)>;

#[derive(Clone, Default)]
pub(crate) struct DirectUploadStore(Arc<Mutex<HashMap<String, Arc<Session>>>>);

struct Session {
    subject: RequestSubject,
    account: String,
    storage_id: String,
    path: String,
    commands: mpsc::Sender<DirectCommand>,
    active: AtomicBool,
    completing: AtomicBool,
    done: Notify,
    result: Mutex<Option<ResultSummary>>,
    expires: std::sync::Mutex<Instant>,
}

impl DirectUploadStore {
    #[cfg(test)]
    pub async fn ensure_idle(&self, storage_id: &str) -> AppResult<()> {
        if self.0.lock().await.values().any(|session| {
            session.storage_id == storage_id && session.active.load(Ordering::Acquire)
        }) {
            return Err(AppError::Conflict(
                "该存储正在直传，请等待完成或终止上传后再修改连接设置".into(),
            ));
        }
        Ok(())
    }
    async fn find(
        &self,
        id: &str,
        subject: &RequestSubject,
        storage: &str,
        path: &str,
    ) -> AppResult<Arc<Session>> {
        let session =
            self.0.lock().await.get(id).cloned().ok_or_else(|| {
                AppError::BadRequest("直传会话不存在或已过期，请核对上传结果".into())
            })?;
        if &session.subject != subject || session.storage_id != storage || session.path != path {
            return Err(AppError::Forbidden);
        }
        Ok(session)
    }
}

#[derive(Deserialize)]
pub(crate) struct StartRequest {
    size: u64,
}

#[derive(Serialize)]
pub(crate) struct StartResponse {
    mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(flatten)]
    descriptor: Option<DirectDescriptor>,
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    query: &FileQuery,
) -> AppResult<(RequestSubject, String, String)> {
    let share = resolve_share(state, headers, query).await?;
    ensure_writable(&share)?;
    ensure_storage_action(state, headers, &share.storage_id, StorageAction::Upload).await?;
    let path =
        crate::storage::StorageService::normalize_relative(query.path.as_deref().unwrap_or(""))?;
    ensure_non_root(&path)?;
    let path = share_storage_path(&share, &path);
    check_folder_locks(state, headers, &share.storage_id, &path).await?;
    let subject = auth::current_request_subject(state, headers)
        .await
        .ok_or(AppError::Forbidden)?;
    Ok((subject, share.storage_id, path))
}

pub(crate) async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<StartRequest>,
) -> AppResult<Json<StartResponse>> {
    // Serialize admission against connection edits; the registered session then keeps edits out.
    let _configuration = state.config_updates.lock().await;
    let (subject, storage_id, path) = authorize(&state, &headers, &query).await?;
    let policy = state.config_file.read().await;
    let relay = policy
        .storage_instances
        .iter()
        .find(|storage| storage.id == storage_id)
        .is_none_or(|storage| match &storage.backend {
            crate::config::StorageBackendConfig::S3(settings) => settings.relay_upload,
            _ => true,
        });
    let maximum = policy.max_upload_bytes;
    drop(policy);
    // An empty object needs no payload transfer and avoids provider-specific
    // zero-length multipart behaviour; use the existing bounded PutObject path.
    if relay || body.size == 0 {
        return Ok(Json(StartResponse {
            mode: "relay",
            session: None,
            descriptor: None,
        }));
    }
    if body.size > maximum {
        return Err(AppError::PayloadTooLarge);
    }
    let batch = query
        .batch
        .ok_or_else(|| AppError::BadRequest("直传必须使用上传批次".into()))?;
    let backend = state.storage_backend(&storage_id).await?;
    if !backend.supports_direct_upload() {
        return Err(AppError::BadRequest("该存储不支持直传".into()));
    }
    let mut sessions = state.direct_uploads.0.lock().await;
    let account = crate::traffic::browser_subject(&state, &headers).await;
    sessions.retain(|_, session| {
        session.active.load(Ordering::Acquire)
            || *session
                .expires
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                > Instant::now()
    });
    if sessions.len() >= 128
        || sessions
            .values()
            .filter(|session| session.active.load(Ordering::Acquire))
            .count()
            >= 32
        || sessions
            .values()
            .filter(|session| session.account == account && session.active.load(Ordering::Acquire))
            .count()
            >= 4
    {
        return Err(AppError::TooManyRequests);
    }
    match state
        .upload_batches
        .begin(&batch, &subject, &storage_id, &path, body.size)
        .await?
    {
        UploadBegin::Start => {}
        UploadBegin::AlreadyComplete(outcome) => {
            return Err(
                AppError::Conflict("上传已提交，请核对文件，不要重复上传".into()).with_operation(
                    CommitState::Committed,
                    outcome.map_or(crate::error::CleanupState::Complete, |value| value.cleanup),
                ),
            )
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = mpsc::channel(4);
    let (ready, descriptor) = oneshot::channel();
    let session = Arc::new(Session {
        subject,
        account,
        storage_id,
        path: path.clone(),
        commands: sender,
        active: AtomicBool::new(true),
        completing: AtomicBool::new(false),
        done: Notify::new(),
        result: Mutex::new(None),
        expires: std::sync::Mutex::new(
            Instant::now() + Duration::from_secs(state.config.upload_timeout_secs + 60),
        ),
    });
    sessions.insert(id.clone(), session.clone());
    drop(sessions);
    drop(_configuration);
    let batches = state.upload_batches.clone();
    let mut execution =
        crate::upload_batch::UploadExecutionGuard::new(batches, batch.clone(), path.clone());
    tokio::spawn(async move {
        let _completion = SessionCompletionGuard(session.clone());
        let result = backend
            .upload_direct_file(
                &path,
                body.size,
                maximum,
                DirectChannel {
                    ready,
                    commands: receiver,
                },
                crate::upload_batch::operation_id(&batch, &path),
            )
            .await;
        // Payload travels between the browser and S3. Do not check or charge
        // VPS traffic allowances; authorization and storage limits still apply.
        execution.finish(&result).await;
        *session.result.lock().await = Some(
            result
                .map(|_| ())
                .map_err(|error| (error.to_string(), error.operation())),
        );
        session.active.store(false, Ordering::Release);
        *session
            .expires
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Instant::now() + Duration::from_secs(60);
        session.done.notify_waiters();
    });
    match descriptor.await {
        Ok(result) => Ok(Json(StartResponse {
            mode: "direct",
            session: Some(id),
            descriptor: Some(result?),
        })),
        Err(_) => Err(AppError::ServiceUnavailable(
            "无法建立 S3 直传，请检查存储连接".into(),
        )),
    }
}

// Always release admission and wake waiters, including an interrupted owner.
// An absent result stays explicitly unknown instead of advertising success.
struct SessionCompletionGuard(Arc<Session>);
impl Drop for SessionCompletionGuard {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
        self.0.done.notify_waiters();
    }
}

#[derive(Deserialize)]
pub(crate) struct CommandRequest {
    session: String,
    #[serde(default)]
    part: u64,
}

async fn session_for(
    state: &AppState,
    headers: &HeaderMap,
    query: &FileQuery,
    id: &str,
) -> AppResult<Arc<Session>> {
    let (subject, storage, path) = authorize(state, headers, query).await?;
    state
        .direct_uploads
        .find(id, &subject, &storage, &path)
        .await
}

async fn send(session: &Session, command: DirectCommand) -> AppResult<()> {
    tokio::time::timeout(Duration::from_secs(15), session.commands.send(command))
        .await
        .map_err(|_| AppError::TooManyRequests)?
        .map_err(|_| AppError::Conflict("直传已结束，请查询上传状态".into()))
}

pub(crate) async fn sign_part(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<CommandRequest>,
) -> AppResult<Json<SignedPart>> {
    let session = session_for(&state, &headers, &query, &body.session).await?;
    let (reply, receiver) = oneshot::channel();
    send(&session, DirectCommand::Sign(body.part, reply)).await?;
    Ok(Json(receiver.await.map_err(|_| {
        AppError::ServiceUnavailable("无法签发分片上传地址".into())
    })??))
}

async fn wait_result(session: &Session) -> AppResult<()> {
    loop {
        let notified = session.done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = session.result.lock().await.clone() {
            return result.map_err(|(message, outcome)| {
                let error = AppError::ServiceUnavailable(message.into());
                match outcome {
                    Some(outcome) => error.with_operation(outcome.commit, outcome.cleanup),
                    None => error,
                }
            });
        }
        if !session.active.load(Ordering::Acquire) {
            return Err(
                AppError::ServiceUnavailable("上传执行已中断，请核对结果".into())
                    .with_operation(CommitState::Unknown, crate::error::CleanupState::Unknown),
            );
        }
        notified.await;
    }
}

pub(crate) async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    Json(body): Json<CommandRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let session = session_for(&state, &headers, &query, &body.session).await?;
    if !session.completing.swap(true, Ordering::AcqRel) && session.active.load(Ordering::Acquire) {
        if let Err(error) = send(&session, DirectCommand::Complete).await {
            session.completing.store(false, Ordering::Release);
            return Err(error);
        }
    }
    wait_result(&session).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub(crate) async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(_query): Query<FileQuery>,
    Json(body): Json<CommandRequest>,
) -> AppResult<Json<serde_json::Value>> {
    // A session owner may abort after storage permissions are revoked.
    let subject = auth::current_request_subject(&state, &headers)
        .await
        .ok_or(AppError::Forbidden)?;
    let session = state
        .direct_uploads
        .0
        .lock()
        .await
        .get(&body.session)
        .cloned()
        .ok_or(AppError::NotFound)?;
    if session.subject != subject {
        return Err(AppError::Forbidden);
    }
    if session.active.load(Ordering::Acquire) {
        let _ = send(&session, DirectCommand::Cancel).await;
    }
    let _ = wait_result(&session).await;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_waiters_observe_one_cached_completion_and_connection_edit_guard() {
        let (commands, _receiver) = mpsc::channel(4);
        let session = Arc::new(Session {
            subject: RequestSubject::Session("test-session".into()),
            account: "test-account".into(),
            storage_id: "store".into(),
            path: "file".into(),
            commands,
            active: AtomicBool::new(true),
            completing: AtomicBool::new(false),
            done: Notify::new(),
            result: Mutex::new(None),
            expires: std::sync::Mutex::new(Instant::now() + Duration::from_secs(60)),
        });
        let store = DirectUploadStore::default();
        store
            .0
            .lock()
            .await
            .insert("session".into(), session.clone());
        assert!(store.ensure_idle("store").await.is_err());
        let first = session.clone();
        let second = session.clone();
        let first = tokio::spawn(async move { wait_result(&first).await });
        let second = tokio::spawn(async move { wait_result(&second).await });
        *session.result.lock().await = Some(Ok(()));
        session.active.store(false, Ordering::Release);
        session.done.notify_waiters();
        tokio::time::timeout(Duration::from_secs(1), async {
            first.await.unwrap().unwrap();
            second.await.unwrap().unwrap();
            wait_result(&session).await.unwrap();
        })
        .await
        .unwrap();
        store.ensure_idle("store").await.unwrap();
    }
}
