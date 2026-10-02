use std::{sync::atomic::AtomicUsize, time::Duration};

use aws_sdk_s3::Client;
use aws_smithy_types::byte_stream::ByteStream;
use axum::http::HeaderValue;
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
const S3_OPERATION_METADATA_KEY: &str = "ycloud-operation";
const S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE: &str = "upload-transaction:v1";
const S3_MULTIPART_SESSION_JOURNAL_PURPOSE: &str = "multipart-session:v1";
const S3_FILE_MOVE_JOURNAL_PURPOSE: &str = "file-move-transaction:v1";
const S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE: &str = "internal-upload-intent:v1";
const S3_ACTIVATION_PROBE_JOURNAL_PURPOSE: &str = "activation-probe-intent:v1";
const S3_MAX_PENDING_TRANSACTIONS: usize = 1_000;
const S3_MULTIPART_THRESHOLD: u64 = 64 * 1024 * 1024;
const S3_MULTIPART_PART_BYTES: u64 = 64 * 1024 * 1024;
const S3_MULTIPART_MAX_PARTS: u64 = 10_000;
// Bound each buffered part even when the deployment envelope is set far
// above the defaults. With 10,000 parts this supports objects up to 5 TiB.
const S3_MULTIPART_MAX_PART_BYTES: u64 = 512 * 1024 * 1024;
const S3_SINGLE_COPY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

mod activation_probe_intent;
mod authenticated_journal;
mod body;
mod capabilities;
mod capacity;
mod client;
mod committed_cleanup;
mod copy;
mod deletion;
mod direct;
mod directory_transaction;
mod file_move_transaction;
mod internal_upload_intent;
mod journal;
mod keyspace;
mod listing;
mod maintenance;
mod multipart;
mod object_read;
#[cfg(test)]
pub(crate) mod protocol_tests;
mod recovery;
mod recovery_runtime;
mod relay;
mod transaction_record;
mod upload;

pub use capabilities::S3CapabilityReport;
use committed_cleanup::CompletionMode;
pub(crate) use direct::{DirectChannel, DirectCommand, DirectDescriptor, SignedPart, UploadInput};

use body::{range_not_satisfiable, ExactLengthBody, PermitStream};
use keyspace::{
    copy_source, internal_key, list_prefix, object_key, parent_relative, valid_transaction_id,
};

use transaction_record::{
    directory_trash_transaction_id, validate_multipart_session, validate_upload_transaction,
    S3MultipartPurpose, S3MultipartSession, S3ObjectSnapshot, S3UploadStage, S3UploadTransaction,
    S3_MULTIPART_SESSION_SCHEMA_VERSION, S3_TRANSACTION_SCHEMA_VERSION,
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
    stream_gate: std::sync::Arc<Semaphore>,
    recovery_gate: std::sync::Arc<tokio::sync::RwLock<()>>,
    mutation_gate: std::sync::Arc<Mutex<()>>,
    capacity_gate: std::sync::Arc<Mutex<()>>,
    upload_timeout: Duration,
    orphan_uploads: std::sync::Arc<AtomicUsize>,
    orphan_backups: std::sync::Arc<AtomicUsize>,
    transaction_auth_key: [u8; 32],
    recovery_runtime: std::sync::Arc<recovery_runtime::RecoveryRuntime>,
    maintenance: std::sync::Arc<maintenance::Maintenance>,
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

    pub async fn delete_file(&self, relative: &str) -> AppResult<u64> {
        let relative = StorageService::normalize_relative(relative)?;
        if relative.is_empty() {
            return Err(AppError::BadRequest("不能删除存储根目录".into()));
        }
        let backend = self.scoped_work(None, None);
        let _mutation = backend
            .maintenance
            .read(async { Ok(backend.mutation_gate.lock().await) })
            .await?;
        let metadata = backend.metadata(&relative).await?;
        if metadata.is_dir {
            return Err(AppError::Conflict("目标是目录而不是文件".into()));
        }
        let key = object_key(&backend.prefix, &relative)?;
        backend
            .delete_key_confirmed(&key, metadata.etag.as_deref())
            .await?;
        Ok(metadata.size)
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
        self.maintenance
            .read(self.head_key_uninterrupted(key))
            .await
    }

    async fn head_key_uninterrupted(&self, key: &str) -> AppResult<Option<RawS3Metadata>> {
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

    async fn acquire_request(&self) -> AppResult<OwnedSemaphorePermit> {
        self.maintenance
            .read(async {
                self.request_gate
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| AppError::ServiceUnavailable("对象存储正在关闭".into()))
            })
            .await
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

fn multipart_session_matches(metadata: &RawS3Metadata, session: &S3MultipartSession) -> bool {
    session.schema_version == S3_MULTIPART_SESSION_SCHEMA_VERSION
        && session.expected_size == Some(metadata.size)
        && metadata.operation_id.as_deref() == Some(session.id.as_str())
        && metadata.etag.is_some()
}

fn simple_copy_matches(destination: &RawS3Metadata, source: &RawS3Metadata) -> bool {
    source.etag.is_some() && destination.etag == source.etag && destination.size == source.size
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
