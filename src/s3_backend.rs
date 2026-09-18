use std::{sync::atomic::AtomicUsize, time::Duration};

use aws_sdk_s3::{types::MultipartUpload, Client};
use aws_smithy_types::byte_stream::ByteStream;
use axum::http::HeaderValue;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::{
    config::S3Provider,
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

const S3_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const S3_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(8);
const S3_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const S3_MAX_ATTEMPTS: u32 = 2;
const S3_MAX_IDLE_CONNECTIONS_PER_HOST: usize = 8;
const S3_IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const S3_REQUEST_CONCURRENCY: usize = 8;
const S3_PAGE_SIZE: usize = 1_000;
const S3_MAX_LIST_ENTRIES: usize = 10_000;
const S3_MAX_LIST_PAGES: usize = 16;
const S3_MAX_CAPACITY_SCAN_PAGES: usize = 10_000;
const S3_TRANSACTION_SCHEMA_VERSION: u32 = 1;
const S3_MULTIPART_SESSION_SCHEMA_VERSION: u32 = 2;
const S3_OPERATION_METADATA_KEY: &str = "ycloud-operation";
const S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE: &str = "upload-transaction:v1";
const S3_MULTIPART_SESSION_JOURNAL_PURPOSE: &str = "multipart-session:v1";
const S3_FILE_MOVE_JOURNAL_PURPOSE: &str = "file-move-transaction:v1";
const S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE: &str = "internal-upload-intent:v1";
const S3_ACTIVATION_PROBE_JOURNAL_PURPOSE: &str = "activation-probe-intent:v1";
const S3_MAX_PENDING_TRANSACTIONS: usize = 1_000;
const S3_MAX_MULTIPART_INTENT_MATCHES: usize = 1_000;
const S3_MULTIPART_THRESHOLD: u64 = 64 * 1024 * 1024;
const S3_MULTIPART_PART_BYTES: u64 = 64 * 1024 * 1024;
const S3_MULTIPART_MAX_PARTS: u64 = 10_000;
// Keep one buffered part bounded even when the deployment envelope is set far
// above the defaults. With 10,000 parts this supports objects up to 5 TiB.
const S3_MULTIPART_MAX_PART_BYTES: u64 = 512 * 1024 * 1024;
const S3_SINGLE_COPY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

mod activation_probe_intent;
mod authenticated_journal;
mod body;
mod capabilities;
mod capacity;
mod client;
mod directory_transaction;
mod file_move_transaction;
mod internal_upload_intent;
mod journal;
mod keyspace;
mod listing;
mod multipart;
mod object_read;
#[cfg(test)]
mod protocol_tests;
mod recovery;
mod recovery_runtime;
mod upload;

pub use capabilities::S3CapabilityReport;

use body::{range_not_satisfiable, ExactLengthBody, PermitStream};
use keyspace::{
    copy_source, internal_key, list_prefix, object_key, parent_relative, valid_transaction_id,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Entry {
    pub name: String,
    pub relative: String,
    pub is_dir: bool,
    pub size: u64,
    pub last_modified: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3ListResult {
    pub entries: Vec<S3Entry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Metadata {
    pub relative: String,
    pub is_dir: bool,
    pub size: u64,
    pub last_modified: Option<i64>,
    pub content_type: Option<String>,
    pub(crate) etag: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3UploadResult {
    pub relative: String,
    pub size: u64,
    pub previous_size: u64,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct S3RecoveryStatus {
    pub orphan_uploads: usize,
    pub orphan_backups: usize,
    pub pending_records: usize,
    pub oldest_pending_age_seconds: Option<u64>,
    pub consecutive_failures: u64,
    pub last_failure: Option<String>,
    pub last_failure_unix: Option<i64>,
    pub next_retry_unix: Option<i64>,
    pub recovering: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum S3UploadStage {
    Prepared,
    BackupCreated,
    DestinationCommitted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct S3ObjectSnapshot {
    size: u64,
    etag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct S3UploadTransaction {
    schema_version: u32,
    id: String,
    relative: String,
    stage: S3UploadStage,
    temporary: S3ObjectSnapshot,
    previous: Option<S3ObjectSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct S3MultipartSession {
    schema_version: u32,
    id: String,
    key: String,
    /// Version 1 records always contain the provider ID. Version 2 is first
    /// persisted with `None`, before CreateMultipartUpload is attempted, so a
    /// lost create response can still be recovered by exact-key enumeration.
    #[serde(default)]
    upload_id: Option<String>,
    #[serde(default)]
    purpose: Option<S3MultipartPurpose>,
    #[serde(default)]
    expected_size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum S3MultipartPurpose {
    Upload,
    Copy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum S3MultipartAbortOutcome {
    Aborted,
    Missing,
}

struct S3MultipartCopyOptions<'a> {
    source_etag: Option<&'a str>,
    content_type: Option<&'a str>,
    destination_must_not_exist: bool,
    operation_id: Option<&'a str>,
}

/// One reusable S3 client and its confined namespace.
///
/// Provider presets are validated before construction and all object keys are
/// rooted below `prefix`. The client never retains a second plaintext copy of
/// credentials outside the AWS credential provider.
#[derive(Clone)]
pub struct S3Backend {
    client: Client,
    provider: S3Provider,
    bucket: String,
    prefix: String,
    request_gate: std::sync::Arc<Semaphore>,
    recovery_gate: std::sync::Arc<tokio::sync::RwLock<()>>,
    mutation_gate: std::sync::Arc<Mutex<()>>,
    upload_timeout: Duration,
    orphan_uploads: std::sync::Arc<AtomicUsize>,
    orphan_backups: std::sync::Arc<AtomicUsize>,
    transaction_auth_key: [u8; 32],
    recovery_runtime: std::sync::Arc<recovery_runtime::RecoveryRuntime>,
}

impl S3Backend {
    pub fn capability_report(&self) -> S3CapabilityReport {
        capabilities::report(self.provider)
    }

    fn is_alibaba_oss(&self) -> bool {
        uses_oss_native_write_conditions(self.provider)
    }

    pub fn object_key(&self, relative: &str) -> AppResult<String> {
        object_key(&self.prefix, relative)
    }

    pub fn list_prefix(&self, relative: &str) -> AppResult<String> {
        list_prefix(&self.prefix, relative)
    }

    pub async fn create_directory(&self, relative: &str) -> AppResult<()> {
        let relative = StorageService::normalize_relative(relative)?;
        if relative.is_empty() {
            return Err(AppError::Conflict("存储根目录已经存在".into()));
        }
        let _mutation = self.mutation_gate.lock().await;
        self.ensure_parent_directory(&relative).await?;
        match self.metadata(&relative).await {
            Ok(_) => return Err(AppError::Conflict("目标路径已经存在".into())),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }

        let marker = list_prefix(&self.prefix, &relative)?;
        let _permit = self.acquire_request().await?;
        let request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(marker)
            .content_length(0)
            .content_type("application/x-directory")
            .body(ByteStream::from_static(&[]));
        let result = if self.is_alibaba_oss() {
            request
                .customize()
                .mutate_request(|request| {
                    request
                        .headers_mut()
                        .insert("x-oss-forbid-overwrite", HeaderValue::from_static("true"));
                })
                .send()
                .await
        } else {
            request.if_none_match("*").send().await
        };
        result.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 directory marker creation failed"
            );
            AppError::ServiceUnavailable("对象存储无法创建目录".into())
        })?;
        Ok(())
    }

    pub async fn copy_file(&self, source: &str, destination: &str) -> AppResult<()> {
        self.copy_file_internal(source, destination, None).await
    }

    pub async fn copy_file_with_expected_size(
        &self,
        source: &str,
        destination: &str,
        expected_size: u64,
    ) -> AppResult<()> {
        self.copy_file_internal(source, destination, Some(expected_size))
            .await
    }

    async fn copy_file_internal(
        &self,
        source: &str,
        destination: &str,
        expected_size: Option<u64>,
    ) -> AppResult<()> {
        let source = StorageService::normalize_relative(source)?;
        let destination = StorageService::normalize_relative(destination)?;
        if source.is_empty() || destination.is_empty() || source == destination {
            return Err(AppError::BadRequest("无效的文件复制路径".into()));
        }
        let _mutation = self.mutation_gate.lock().await;
        self.copy_file_locked(&source, &destination, expected_size)
            .await
            .map(|_| ())
    }

    pub async fn delete_file(&self, relative: &str) -> AppResult<u64> {
        let relative = StorageService::normalize_relative(relative)?;
        if relative.is_empty() {
            return Err(AppError::BadRequest("不能删除存储根目录".into()));
        }
        let _mutation = self.mutation_gate.lock().await;
        let metadata = self.metadata(&relative).await?;
        if metadata.is_dir {
            return Err(AppError::Conflict("目标是目录而不是文件".into()));
        }
        let key = object_key(&self.prefix, &relative)?;
        self.delete_key(&key, metadata.etag.as_deref()).await?;
        Ok(metadata.size)
    }

    /// Delete only an empty directory marker. Recursive directory deletion is
    /// deliberately separate because S3 cannot make a whole prefix disappear
    /// atomically.
    pub async fn delete_empty_directory(&self, relative: &str) -> AppResult<()> {
        let relative = StorageService::normalize_relative(relative)?;
        if relative.is_empty() {
            return Err(AppError::BadRequest("不能删除存储根目录".into()));
        }
        let _mutation = self.mutation_gate.lock().await;
        let metadata = self.metadata(&relative).await?;
        if !metadata.is_dir {
            return Err(AppError::Conflict("目标不是目录".into()));
        }
        if !self.list_directory(&relative, 1).await?.entries.is_empty() {
            return Err(AppError::Conflict("目录不为空".into()));
        }
        let marker = list_prefix(&self.prefix, &relative)?;
        let marker_metadata = self.head_key(&marker).await?;
        let Some(marker_metadata) = marker_metadata else {
            return Err(AppError::Conflict(
                "隐式目录没有可安全删除的目录标记".into(),
            ));
        };
        self.delete_key(&marker, marker_metadata.etag.as_deref())
            .await
    }

    async fn copy_file_locked(
        &self,
        source: &str,
        destination: &str,
        expected_size: Option<u64>,
    ) -> AppResult<S3Metadata> {
        let source_metadata = self.metadata(source).await?;
        if source_metadata.is_dir {
            return Err(AppError::Conflict("当前操作只接受普通文件".into()));
        }
        if expected_size.is_some_and(|size| size != source_metadata.size) {
            return Err(AppError::Conflict(
                "Source changed while preparing the copy".into(),
            ));
        }
        self.ensure_parent_directory(destination).await?;
        match self.metadata(destination).await {
            Ok(_) => return Err(AppError::Conflict("目标路径已经存在".into())),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }

        let source_key = object_key(&self.prefix, source)?;
        let destination_key = object_key(&self.prefix, destination)?;
        let copied_etag = self
            .copy_key(
                &source_key,
                &destination_key,
                source_metadata.etag.as_deref(),
                true,
            )
            .await?;
        let destination_metadata = self.metadata(destination).await?;
        let is_our_copy = destination_metadata.etag.as_ref() == Some(&copied_etag);
        if destination_metadata.is_dir
            || destination_metadata.size != source_metadata.size
            || !is_our_copy
        {
            if is_our_copy {
                self.delete_key(&destination_key, destination_metadata.etag.as_deref())
                    .await?;
            }
            return Err(AppError::ServiceUnavailable(
                "对象存储复制结果校验失败".into(),
            ));
        }
        Ok(source_metadata)
    }

    async fn ensure_parent_directory(&self, relative: &str) -> AppResult<()> {
        let parent = parent_relative(relative);
        let metadata = self.metadata(parent).await?;
        if metadata.is_dir {
            Ok(())
        } else {
            Err(AppError::Conflict("目标父路径不是目录".into()))
        }
    }

    async fn head_key(&self, key: &str) -> AppResult<Option<RawS3Metadata>> {
        let _permit = self.acquire_request().await?;
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => Ok(Some(RawS3Metadata {
                size: non_negative_size(output.content_length())?,
                etag: output.e_tag().map(str::to_owned),
                content_type: output.content_type().map(str::to_owned),
                operation_id: output
                    .metadata()
                    .and_then(|metadata| metadata.get(S3_OPERATION_METADATA_KEY))
                    .cloned(),
            })),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_not_found()) =>
            {
                Ok(None)
            }
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 object verification failed"
                );
                Err(AppError::ServiceUnavailable(
                    "无法校验对象存储写入结果".into(),
                ))
            }
        }
    }

    async fn copy_key(
        &self,
        source_key: &str,
        destination_key: &str,
        source_etag: Option<&str>,
        destination_must_not_exist: bool,
    ) -> AppResult<String> {
        self.copy_key_with_operation(
            source_key,
            destination_key,
            source_etag,
            destination_must_not_exist,
            None,
        )
        .await
    }

    async fn copy_key_with_operation(
        &self,
        source_key: &str,
        destination_key: &str,
        source_etag: Option<&str>,
        destination_must_not_exist: bool,
        operation_id: Option<&str>,
    ) -> AppResult<String> {
        let source = self.head_key(source_key).await?.ok_or(AppError::NotFound)?;
        if source.size > S3_SINGLE_COPY_LIMIT {
            return self
                .multipart_copy(
                    source_key,
                    destination_key,
                    source.size,
                    S3MultipartCopyOptions {
                        source_etag,
                        content_type: source.content_type.as_deref(),
                        destination_must_not_exist,
                        operation_id,
                    },
                )
                .await;
        }
        let mut request = self
            .client
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(copy_source(&self.bucket, source_key))
            .key(destination_key);
        if let Some(etag) = source_etag {
            request = request.copy_source_if_match(etag);
        }
        if destination_must_not_exist && !self.is_alibaba_oss() {
            request = request.if_none_match("*");
        }
        let result = {
            let _permit = self.acquire_request().await?;
            if destination_must_not_exist && self.is_alibaba_oss() {
                request
                    .customize()
                    .mutate_request(|request| {
                        request
                            .headers_mut()
                            .insert("x-oss-forbid-overwrite", HeaderValue::from_static("true"));
                    })
                    .send()
                    .await
            } else {
                request.send().await
            }
        };
        let copy_result = match result {
            Ok(output) => output
                .copy_object_result()
                .and_then(|result| result.e_tag())
                .map(str::to_owned)
                .ok_or_else(|| AppError::ServiceUnavailable("对象复制未返回提交 ETag".into())),
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 server-side copy failed"
                );
                Err(AppError::ServiceUnavailable(
                    "对象存储服务端复制失败".into(),
                ))
            }
        };
        let Err(error) = copy_result else {
            return copy_result;
        };
        if !destination_must_not_exist {
            return Err(error);
        }
        match self.head_key(destination_key).await {
            Ok(Some(destination)) if simple_copy_matches(&destination, &source) => {
                Ok(destination.etag.expect("copy match requires an ETag"))
            }
            Ok(_) | Err(_) => {
                Err(error.with_operation(CommitState::Unknown, CleanupState::Unknown))
            }
        }
    }

    async fn delete_key(&self, key: &str, etag: Option<&str>) -> AppResult<()> {
        if self.is_alibaba_oss() {
            if let Some(expected_etag) = etag {
                let current = self.head_key(key).await?;
                if current
                    .as_ref()
                    .and_then(|metadata| metadata.etag.as_deref())
                    != Some(expected_etag)
                {
                    return Err(AppError::Conflict(
                        "对象在删除前已经发生变化，请刷新后重试".into(),
                    ));
                }
            }
        }
        let _permit = self.acquire_request().await?;
        let mut request = self.client.delete_object().bucket(&self.bucket).key(key);
        if !self.is_alibaba_oss() {
            if let Some(etag) = etag {
                request = request.if_match(etag);
            }
        }
        request.send().await.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 object deletion failed"
            );
            AppError::ServiceUnavailable("对象存储删除失败".into())
        })?;
        Ok(())
    }

    async fn delete_key_confirmed(&self, key: &str, etag: Option<&str>) -> AppResult<()> {
        let current = self.head_key(key).await?;
        let Some(current) = current else {
            self.recovery_runtime.journal_settled(key);
            return Ok(());
        };
        if etag.is_some() && current.etag.as_deref() != etag {
            return Err(AppError::ServiceUnavailable(
                "对象存储待清理对象已发生变化；已保留恢复记录".into(),
            ));
        }
        if let Err(error) = self.delete_key(key, etag).await {
            if self.head_key(key).await?.is_some() {
                return Err(error);
            }
            self.recovery_runtime.journal_settled(key);
            return Ok(());
        }
        if self.head_key(key).await?.is_some() {
            return Err(AppError::ServiceUnavailable(
                "对象存储未能确认对象已经删除；已保留恢复记录".into(),
            ));
        }
        self.recovery_runtime.journal_settled(key);
        Ok(())
    }

    async fn acquire_request(&self) -> AppResult<OwnedSemaphorePermit> {
        self.request_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储正在关闭".into()))
    }
}

fn uses_oss_native_write_conditions(provider: S3Provider) -> bool {
    provider == S3Provider::AlibabaOss
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawS3Metadata {
    size: u64,
    etag: Option<String>,
    content_type: Option<String>,
    operation_id: Option<String>,
}

fn directory_metadata(relative: String) -> S3Metadata {
    S3Metadata {
        relative,
        is_dir: true,
        size: 0,
        last_modified: None,
        content_type: None,
        etag: None,
    }
}

fn non_negative_size(size: Option<i64>) -> AppResult<u64> {
    size.ok_or_else(|| AppError::ServiceUnavailable("对象存储未返回内容长度".into()))?
        .try_into()
        .map_err(|_| AppError::ServiceUnavailable("对象存储返回了无效的内容长度".into()))
}

fn validate_multipart_session(
    prefix: &str,
    journal_key: &str,
    session: &S3MultipartSession,
) -> AppResult<()> {
    let internal_upload = is_internal_multipart_upload_key(prefix, &session.key);
    let internal_backup = is_internal_multipart_backup_key(prefix, &session.key);
    let regular_object = session.key.strip_prefix(prefix).is_some_and(|relative| {
        !relative.is_empty()
            && !relative.starts_with(".ycloud-system/")
            && StorageService::normalize_relative(relative)
                .is_ok_and(|normalized| normalized == relative)
    });
    let upload_id_valid = session
        .upload_id
        .as_deref()
        .is_none_or(valid_multipart_upload_id);
    let valid_state = match session.schema_version {
        // Read-only compatibility for records written before pre-create
        // intents were introduced.
        S3_TRANSACTION_SCHEMA_VERSION => {
            session.upload_id.is_some()
                && upload_id_valid
                && session.purpose.is_none()
                && session.expected_size.is_none()
                && (internal_upload || regular_object)
        }
        S3_MULTIPART_SESSION_SCHEMA_VERSION => {
            let expected_size = session.expected_size.unwrap_or(0);
            let within_provider_limit =
                expected_size <= S3_MULTIPART_MAX_PART_BYTES.saturating_mul(S3_MULTIPART_MAX_PARTS);
            upload_id_valid
                && within_provider_limit
                && match session.purpose {
                    Some(S3MultipartPurpose::Upload) => {
                        internal_upload && expected_size >= S3_MULTIPART_THRESHOLD
                    }
                    Some(S3MultipartPurpose::Copy) => {
                        (regular_object || internal_backup) && expected_size > S3_SINGLE_COPY_LIMIT
                    }
                    None => false,
                }
        }
        _ => false,
    };
    if !valid_state
        || !valid_transaction_id(&session.id)
        || journal_key != internal_key(prefix, "multipart-sessions", &session.id)
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储分片恢复记录无法安全处理".into(),
        ));
    }
    Ok(())
}

fn valid_multipart_upload_id(upload_id: &str) -> bool {
    !upload_id.is_empty() && upload_id.len() <= 4_096 && !upload_id.chars().any(char::is_control)
}

fn is_internal_multipart_upload_key(prefix: &str, key: &str) -> bool {
    key.strip_prefix(&internal_key(prefix, "uploads", ""))
        .is_some_and(valid_transaction_id)
}

fn is_internal_multipart_backup_key(prefix: &str, key: &str) -> bool {
    key.strip_prefix(&internal_key(prefix, "backups", ""))
        .is_some_and(valid_transaction_id)
}

fn is_owned_internal_multipart_key(prefix: &str, session: &S3MultipartSession) -> bool {
    match session.purpose {
        Some(S3MultipartPurpose::Upload) => is_internal_multipart_upload_key(prefix, &session.key),
        Some(S3MultipartPurpose::Copy) => is_internal_multipart_backup_key(prefix, &session.key),
        None => false,
    }
}

fn multipart_session_matches(metadata: &RawS3Metadata, session: &S3MultipartSession) -> bool {
    session.schema_version == S3_MULTIPART_SESSION_SCHEMA_VERSION
        && session.expected_size == Some(metadata.size)
        && metadata.operation_id.as_deref() == Some(session.id.as_str())
        && metadata.etag.is_some()
}

fn simple_copy_matches(destination: &RawS3Metadata, source: &RawS3Metadata) -> bool {
    source.etag.is_some() && destination.etag == source.etag && destination.size == source.size
}

fn append_exact_multipart_matches(
    expected_key: &str,
    uploads: &[MultipartUpload],
    matches: &mut Vec<String>,
) -> AppResult<()> {
    for upload in uploads {
        let candidate_key = upload
            .key()
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储分片会话缺少键名".into()))?;
        if candidate_key != expected_key {
            continue;
        }
        let upload_id = upload
            .upload_id()
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储分片会话缺少 ID".into()))?;
        if !valid_multipart_upload_id(upload_id) {
            return Err(AppError::ServiceUnavailable(
                "对象存储分片会话 ID 无法安全处理".into(),
            ));
        }
        if matches.len() == S3_MAX_MULTIPART_INTENT_MATCHES {
            return Err(AppError::ServiceUnavailable(
                "单个对象的待恢复分片会话超过安全上限".into(),
            ));
        }
        matches.push(upload_id.to_owned());
    }
    Ok(())
}

fn validate_upload_transaction(
    prefix: &str,
    journal_key: &str,
    transaction: &S3UploadTransaction,
) -> AppResult<()> {
    if transaction.schema_version != S3_TRANSACTION_SCHEMA_VERSION
        || !valid_transaction_id(&transaction.id)
        || journal_key != internal_key(prefix, "transactions", &transaction.id)
        || transaction.temporary.etag.is_none()
        || transaction
            .previous
            .as_ref()
            .is_some_and(|snapshot| snapshot.etag.is_none())
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务记录无法安全恢复".into(),
        ));
    }
    let relative = StorageService::normalize_relative(&transaction.relative)?;
    if relative.is_empty() || relative != transaction.relative {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务记录包含无效目标路径".into(),
        ));
    }
    Ok(())
}

fn snapshot_matches(metadata: &RawS3Metadata, snapshot: &S3ObjectSnapshot) -> bool {
    metadata.size == snapshot.size && metadata.etag.is_some() && metadata.etag == snapshot.etag
}

fn sanitize_content_type(content_type: Option<&str>) -> AppResult<Option<String>> {
    let Some(content_type) = content_type else {
        return Ok(None);
    };
    if content_type.len() > 255 || HeaderValue::from_str(content_type).is_err() {
        return Err(AppError::BadRequest("无效的 Content-Type".into()));
    }
    Ok(Some(content_type.to_owned()))
}

fn multipart_part_size(total_bytes: u64) -> AppResult<usize> {
    let minimum = total_bytes.div_ceil(S3_MULTIPART_MAX_PARTS);
    let mebibyte = 1024 * 1024;
    let rounded = minimum.div_ceil(mebibyte) * mebibyte;
    let size = S3_MULTIPART_PART_BYTES.max(rounded);
    if size > S3_MULTIPART_MAX_PART_BYTES {
        return Err(AppError::PayloadTooLarge);
    }
    usize::try_from(size).map_err(|_| AppError::PayloadTooLarge)
}

#[cfg(test)]
mod tests;
