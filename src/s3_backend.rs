use std::time::Duration;

use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig},
    types::{CompletedMultipartUpload, CompletedPart},
    Client,
};
use aws_smithy_types::byte_stream::ByteStream;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use axum::{body::Body, http::HeaderValue};
use bytes::BytesMut;
use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::{
    config::S3Provider,
    error::{AppError, AppResult},
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
const S3_MAX_PENDING_TRANSACTIONS: usize = 1_000;
const S3_MAX_TRANSACTION_BYTES: usize = 64 * 1024;
const S3_MULTIPART_THRESHOLD: u64 = 64 * 1024 * 1024;
const S3_MULTIPART_PART_BYTES: u64 = 64 * 1024 * 1024;
const S3_MULTIPART_MAX_PARTS: u64 = 10_000;
// Keep one buffered part bounded even when the deployment envelope is set far
// above the defaults. With 10,000 parts this supports objects up to 5 TiB.
const S3_MULTIPART_MAX_PART_BYTES: u64 = 512 * 1024 * 1024;
const S3_SINGLE_COPY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

mod body;
mod capacity;
mod client;
mod directory_transaction;
mod keyspace;
mod listing;
mod object_read;

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
    upload_id: String,
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
    mutation_gate: std::sync::Arc<Mutex<()>>,
    upload_timeout: Duration,
}

impl S3Backend {
    fn is_alibaba_oss(&self) -> bool {
        uses_oss_native_write_conditions(self.provider)
    }

    pub fn object_key(&self, relative: &str) -> AppResult<String> {
        object_key(&self.prefix, relative)
    }

    pub fn list_prefix(&self, relative: &str) -> AppResult<String> {
        list_prefix(&self.prefix, relative)
    }

    /// Upload directly to an inaccessible temporary object and commit with a
    /// server-side copy. The caller-provided length is enforced while the
    /// stream is consumed, so a misleading Content-Length cannot bypass the
    /// configured single-file limit.
    pub async fn upload_file(
        &self,
        relative: &str,
        body: Body,
        content_length: u64,
        max_upload_bytes: u64,
        content_type: Option<&str>,
    ) -> AppResult<S3UploadResult> {
        if content_length > max_upload_bytes
            || content_length > S3_MULTIPART_MAX_PART_BYTES * S3_MULTIPART_MAX_PARTS
        {
            return Err(AppError::PayloadTooLarge);
        }
        let relative = StorageService::normalize_relative(relative)?;
        let destination_key = object_key(&self.prefix, &relative)?;
        self.ensure_parent_directory(&relative).await?;
        let upload_id = uuid::Uuid::new_v4().simple().to_string();
        let temporary_key = internal_key(&self.prefix, "uploads", &upload_id);
        let backup_key = internal_key(&self.prefix, "backups", &upload_id);
        let content_type = sanitize_content_type(content_type)?;
        let upload = if content_length >= S3_MULTIPART_THRESHOLD {
            self.multipart_upload(
                &temporary_key,
                body,
                content_length,
                content_type.as_deref(),
            )
            .await
        } else {
            self.single_upload(
                &temporary_key,
                body,
                content_length,
                content_type.as_deref(),
            )
            .await
        };
        if let Err(error) = upload {
            tracing::warn!(%error, "S3 temporary upload failed");
            self.delete_internal_best_effort(&temporary_key).await;
            return Err(AppError::ServiceUnavailable(
                "对象存储上传失败或请求体长度不一致".into(),
            ));
        }

        let temporary = match self.head_key(&temporary_key).await? {
            Some(metadata) if metadata.size == content_length => metadata,
            Some(_) => {
                self.delete_internal_best_effort(&temporary_key).await;
                return Err(AppError::ServiceUnavailable(
                    "对象存储暂存对象长度校验失败".into(),
                ));
            }
            None => {
                return Err(AppError::ServiceUnavailable(
                    "对象存储未保存上传的暂存对象".into(),
                ));
            }
        };

        let _mutation = self.mutation_gate.lock().await;
        self.ensure_parent_directory(&relative).await?;
        let existing = match self.metadata(&relative).await {
            Ok(metadata) if metadata.is_dir => {
                self.delete_internal_best_effort(&temporary_key).await;
                return Err(AppError::Conflict("不能用文件覆盖目录".into()));
            }
            Ok(metadata) => Some(metadata),
            Err(AppError::NotFound) => None,
            Err(error) => {
                self.delete_internal_best_effort(&temporary_key).await;
                return Err(error);
            }
        };
        if temporary.etag.is_none()
            || existing
                .as_ref()
                .is_some_and(|metadata| metadata.etag.is_none())
        {
            self.delete_internal_best_effort(&temporary_key).await;
            return Err(AppError::ServiceUnavailable(
                "对象存储未返回数据对象 ETag，无法安全提交".into(),
            ));
        }

        let journal_key = internal_key(&self.prefix, "transactions", &upload_id);
        let mut transaction = S3UploadTransaction {
            schema_version: S3_TRANSACTION_SCHEMA_VERSION,
            id: upload_id,
            relative: relative.clone(),
            stage: S3UploadStage::Prepared,
            temporary: S3ObjectSnapshot {
                size: temporary.size,
                etag: temporary.etag.clone(),
            },
            previous: existing.as_ref().map(|metadata| S3ObjectSnapshot {
                size: metadata.size,
                etag: metadata.etag.clone(),
            }),
        };
        let mut journal_etag = match self
            .write_upload_transaction(&journal_key, &transaction, None)
            .await
        {
            Ok(etag) => etag,
            Err(error) => {
                self.delete_internal_best_effort(&temporary_key).await;
                return Err(error);
            }
        };

        let mut backup_created = false;
        if let Some(existing) = existing.as_ref() {
            let backup_etag = self
                .copy_key(
                    &destination_key,
                    &backup_key,
                    existing.etag.as_deref(),
                    true,
                )
                .await?;
            let backup = self.head_key(&backup_key).await?;
            if !backup.as_ref().is_some_and(|backup| {
                backup.size == existing.size && backup.etag.as_ref() == Some(&backup_etag)
            }) {
                return Err(AppError::ServiceUnavailable(
                    "旧对象备份结果无法确认，请停止写入并检查存储后端".into(),
                ));
            }
            backup_created = true;
            transaction.stage = S3UploadStage::BackupCreated;
            journal_etag = self
                .write_upload_transaction(&journal_key, &transaction, journal_etag.as_deref())
                .await?;
        }

        let commit = self
            .copy_key(
                &temporary_key,
                &destination_key,
                temporary.etag.as_deref(),
                false,
            )
            .await;
        let committed = match commit {
            Ok(committed_etag) => self.head_key(&destination_key).await?.filter(|value| {
                value.size == content_length && value.etag.as_ref() == Some(&committed_etag)
            }),
            Err(error) => {
                tracing::warn!(%error, "S3 upload commit returned an ambiguous failure");
                self.head_key(&destination_key).await?.filter(|value| {
                    value.size == content_length
                        && temporary.etag.is_some()
                        && value.etag == temporary.etag
                })
            }
        };

        let Some(committed) = committed else {
            if backup_created {
                let restored_etag = match self
                    .copy_key(
                        &backup_key,
                        &destination_key,
                        transaction
                            .previous
                            .as_ref()
                            .and_then(|snapshot| snapshot.etag.as_deref()),
                        false,
                    )
                    .await
                {
                    Ok(etag) => etag,
                    Err(error) => {
                        tracing::error!(%error, "failed to restore S3 object backup after upload commit failure");
                        return Err(AppError::ServiceUnavailable(
                            "对象提交失败且旧对象恢复失败，请停止写入并检查存储后端".into(),
                        ));
                    }
                };
                let restored = self.head_key(&destination_key).await?;
                let previous = transaction.previous.as_ref();
                let restore_verified =
                    restored
                        .as_ref()
                        .zip(previous)
                        .is_some_and(|(restored, previous)| {
                            restored.size == previous.size
                                && restored.etag.as_ref() == Some(&restored_etag)
                        });
                if !restore_verified {
                    return Err(AppError::ServiceUnavailable(
                        "旧对象恢复结果无法确认，请停止写入并检查存储后端".into(),
                    ));
                }
                self.delete_internal_best_effort(&temporary_key).await;
                self.delete_internal_best_effort(&backup_key).await;
                self.delete_transaction_best_effort(&journal_key, journal_etag.as_deref())
                    .await;
            } else if self.head_key(&destination_key).await?.is_none() {
                self.delete_internal_best_effort(&temporary_key).await;
                self.delete_transaction_best_effort(&journal_key, journal_etag.as_deref())
                    .await;
            }
            return Err(AppError::ServiceUnavailable(
                "对象存储未能确认上传提交结果".into(),
            ));
        };

        transaction.stage = S3UploadStage::DestinationCommitted;
        journal_etag = self
            .write_upload_transaction(&journal_key, &transaction, journal_etag.as_deref())
            .await?;

        self.delete_internal_best_effort(&temporary_key).await;
        if backup_created {
            self.delete_internal_best_effort(&backup_key).await;
        }
        self.delete_transaction_best_effort(&journal_key, journal_etag.as_deref())
            .await;
        Ok(S3UploadResult {
            relative,
            size: committed.size,
            previous_size: existing.as_ref().map_or(0, |metadata| metadata.size),
            etag: committed.etag,
        })
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

    /// Resolve upload journals left by an interrupted process. Recovery only
    /// accepts states that can be proven to be either the old object or the
    /// newly uploaded object. Anything else stops activation for inspection.
    pub async fn recover_transactions(&self) -> AppResult<usize> {
        let _mutation = self.mutation_gate.lock().await;
        let multipart_sessions = self.list_multipart_session_keys().await?;
        let mut recovered = multipart_sessions.len();
        for key in multipart_sessions {
            let (session, journal_etag) = self.read_multipart_session(&key).await?;
            self.recover_multipart_session(&key, &journal_etag, &session)
                .await?;
        }
        let transaction_keys = self.list_transaction_keys().await?;
        recovered = recovered.saturating_add(transaction_keys.len());
        for key in transaction_keys {
            let (transaction, journal_etag) = self.read_upload_transaction(&key).await?;
            self.recover_upload_transaction(&key, &journal_etag, &transaction)
                .await?;
        }
        Ok(recovered.saturating_add(self.recover_directory_transactions().await?))
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

    pub async fn move_file(&self, source: &str, destination: &str) -> AppResult<()> {
        let source = StorageService::normalize_relative(source)?;
        let destination = StorageService::normalize_relative(destination)?;
        if source.is_empty() || destination.is_empty() || source == destination {
            return Err(AppError::BadRequest("无效的文件移动路径".into()));
        }
        let _mutation = self.mutation_gate.lock().await;
        let source_metadata = self.copy_file_locked(&source, &destination, None).await?;
        let source_key = object_key(&self.prefix, &source)?;
        if let Err(error) = self
            .delete_key(&source_key, source_metadata.etag.as_deref())
            .await
        {
            tracing::warn!(%error, "S3 move copied the object but could not remove the source");
            return Err(AppError::ServiceUnavailable(
                "文件已复制到目标，但源文件删除失败；为避免数据丢失已保留两份".into(),
            ));
        }
        Ok(())
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
        let source = self.head_key(source_key).await?.ok_or(AppError::NotFound)?;
        if source.size > S3_SINGLE_COPY_LIMIT {
            return self
                .multipart_copy(
                    source_key,
                    destination_key,
                    source.size,
                    source_etag,
                    destination_must_not_exist,
                )
                .await;
        }
        let _permit = self.acquire_request().await?;
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
        let result = if destination_must_not_exist && self.is_alibaba_oss() {
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
        };
        let output = result.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 server-side copy failed"
            );
            AppError::ServiceUnavailable("对象存储服务端复制失败".into())
        })?;
        output
            .copy_object_result()
            .and_then(|result| result.e_tag())
            .map(str::to_owned)
            .ok_or_else(|| AppError::ServiceUnavailable("对象复制未返回提交 ETag".into()))
    }

    async fn single_upload(
        &self,
        key: &str,
        body: Body,
        content_length: u64,
        content_type: Option<&str>,
    ) -> AppResult<()> {
        let exact_body = ExactLengthBody::new(body.into_data_stream(), content_length);
        let stream = ByteStream::from_body_1_x(exact_body);
        let length = i64::try_from(content_length).map_err(|_| AppError::PayloadTooLarge)?;
        let upload_timeouts = TimeoutConfig::builder()
            .connect_timeout(S3_CONNECT_TIMEOUT)
            .operation_attempt_timeout(self.upload_timeout)
            .operation_timeout(self.upload_timeout)
            .build();
        let operation_override = aws_sdk_s3::config::Builder::new()
            .retry_config(RetryConfig::standard().with_max_attempts(1))
            .timeout_config(upload_timeouts);
        let _permit = self.acquire_request().await?;
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_length(length)
            .body(stream);
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        request
            .customize()
            .config_override(operation_override)
            .send()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储上传失败".into()))?;
        Ok(())
    }

    async fn multipart_upload(
        &self,
        key: &str,
        body: Body,
        content_length: u64,
        content_type: Option<&str>,
    ) -> AppResult<()> {
        let mut create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key);
        if let Some(content_type) = content_type {
            create = create.content_type(content_type);
        }
        let upload_id = {
            let _permit = self.acquire_request().await?;
            create
                .send()
                .await
                .map_err(|_| AppError::ServiceUnavailable("无法创建对象存储分片上传".into()))?
                .upload_id()
                .map(str::to_owned)
                .ok_or_else(|| AppError::ServiceUnavailable("对象存储未返回分片上传 ID".into()))?
        };
        let (session_key, session_etag) =
            match self.register_multipart_session_task(key, &upload_id).await {
                Ok(session) => session,
                Err(error) => {
                    self.abort_multipart_best_effort(key, &upload_id).await;
                    return Err(error);
                }
            };
        let result = self
            .upload_multipart_parts(key, &upload_id, body, content_length)
            .await;
        if result.is_err() {
            if self
                .abort_multipart_operation(key, &upload_id)
                .await
                .is_ok()
            {
                self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                    .await;
            }
        } else {
            self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                .await;
        }
        result
    }

    async fn upload_multipart_parts(
        &self,
        key: &str,
        upload_id: &str,
        body: Body,
        content_length: u64,
    ) -> AppResult<()> {
        let part_size = multipart_part_size(content_length)?;
        let mut stream = body.into_data_stream();
        let mut buffer = BytesMut::with_capacity(part_size);
        let mut completed = Vec::new();
        let mut received = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| AppError::ServiceUnavailable("读取上传请求失败".into()))?;
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or(AppError::PayloadTooLarge)?;
            if received > content_length {
                return Err(AppError::PayloadTooLarge);
            }
            let mut offset = 0;
            while offset < chunk.len() {
                let take = (part_size - buffer.len()).min(chunk.len() - offset);
                buffer.extend_from_slice(&chunk[offset..offset + take]);
                offset += take;
                if buffer.len() == part_size {
                    completed.push(
                        self.upload_part(
                            key,
                            upload_id,
                            completed.len() + 1,
                            buffer.split().freeze(),
                        )
                        .await?,
                    );
                }
            }
        }
        if received != content_length {
            return Err(AppError::ServiceUnavailable("上传请求体长度不一致".into()));
        }
        if !buffer.is_empty() {
            completed.push(
                self.upload_part(key, upload_id, completed.len() + 1, buffer.freeze())
                    .await?,
            );
        }
        let multipart = CompletedMultipartUpload::builder()
            .set_parts(Some(completed))
            .build();
        let _permit = self.acquire_request().await?;
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(multipart)
            .send()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储分片上传提交失败".into()))?;
        Ok(())
    }

    async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        number: usize,
        bytes: bytes::Bytes,
    ) -> AppResult<CompletedPart> {
        let part_number = i32::try_from(number).map_err(|_| AppError::PayloadTooLarge)?;
        let length = i64::try_from(bytes.len()).map_err(|_| AppError::PayloadTooLarge)?;
        let upload_timeouts = TimeoutConfig::builder()
            .connect_timeout(S3_CONNECT_TIMEOUT)
            .operation_attempt_timeout(self.upload_timeout)
            .operation_timeout(self.upload_timeout)
            .build();
        let operation_override = aws_sdk_s3::config::Builder::new()
            .retry_config(RetryConfig::standard().with_max_attempts(1))
            .timeout_config(upload_timeouts);
        let _permit = self.acquire_request().await?;
        let output = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .content_length(length)
            .body(ByteStream::from(bytes))
            .customize()
            .config_override(operation_override)
            .send()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储分片上传失败".into()))?;
        let etag = output
            .e_tag()
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储分片未返回 ETag".into()))?;
        Ok(CompletedPart::builder()
            .part_number(part_number)
            .e_tag(etag)
            .build())
    }

    async fn multipart_copy(
        &self,
        source_key: &str,
        destination_key: &str,
        source_size: u64,
        source_etag: Option<&str>,
        destination_must_not_exist: bool,
    ) -> AppResult<String> {
        if destination_must_not_exist && self.head_key(destination_key).await?.is_some() {
            return Err(AppError::Conflict("目标对象已存在".into()));
        }
        let upload_id = {
            let _permit = self.acquire_request().await?;
            self.client
                .create_multipart_upload()
                .bucket(&self.bucket)
                .key(destination_key)
                .send()
                .await
                .map_err(|_| AppError::ServiceUnavailable("无法创建对象存储分片复制".into()))?
                .upload_id()
                .map(str::to_owned)
                .ok_or_else(|| AppError::ServiceUnavailable("对象存储未返回分片复制 ID".into()))?
        };
        let (session_key, session_etag) = match self
            .register_multipart_session_task(destination_key, &upload_id)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                self.abort_multipart_best_effort(destination_key, &upload_id)
                    .await;
                return Err(error);
            }
        };
        let result = async {
            let part_size = multipart_part_size(source_size)? as u64;
            let mut completed = Vec::new();
            let mut start = 0_u64;
            while start < source_size {
                let end = (start + part_size).min(source_size) - 1;
                let part_number =
                    i32::try_from(completed.len() + 1).map_err(|_| AppError::PayloadTooLarge)?;
                let _permit = self.acquire_request().await?;
                let mut request = self
                    .client
                    .upload_part_copy()
                    .bucket(&self.bucket)
                    .key(destination_key)
                    .upload_id(&upload_id)
                    .part_number(part_number)
                    .copy_source(copy_source(&self.bucket, source_key))
                    .copy_source_range(format!("bytes={start}-{end}"));
                if let Some(etag) = source_etag {
                    request = request.copy_source_if_match(etag);
                }
                let output = request
                    .send()
                    .await
                    .map_err(|_| AppError::ServiceUnavailable("对象存储分片复制失败".into()))?;
                let etag = output
                    .copy_part_result()
                    .and_then(|result| result.e_tag())
                    .ok_or_else(|| {
                        AppError::ServiceUnavailable("对象存储复制分片未返回 ETag".into())
                    })?;
                completed.push(
                    CompletedPart::builder()
                        .part_number(part_number)
                        .e_tag(etag)
                        .build(),
                );
                start = end + 1;
            }
            let multipart = CompletedMultipartUpload::builder()
                .set_parts(Some(completed))
                .build();
            let _permit = self.acquire_request().await?;
            self.client
                .complete_multipart_upload()
                .bucket(&self.bucket)
                .key(destination_key)
                .upload_id(&upload_id)
                .multipart_upload(multipart)
                .send()
                .await
                .map_err(|_| AppError::ServiceUnavailable("对象存储分片复制提交失败".into()))?
                .e_tag()
                .map(str::to_owned)
                .ok_or_else(|| AppError::ServiceUnavailable("对象存储复制未返回提交 ETag".into()))
        }
        .await;
        if result.is_err() {
            if self
                .abort_multipart_operation(destination_key, &upload_id)
                .await
                .is_ok()
            {
                self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                    .await;
            }
        } else {
            self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                .await;
        }
        result
    }

    async fn abort_multipart_best_effort(&self, key: &str, upload_id: &str) {
        if let Err(error) = self.abort_multipart_operation(key, upload_id).await {
            tracing::warn!(%error, "failed to abort an incomplete S3 multipart operation");
        }
    }

    async fn abort_multipart_operation(&self, key: &str, upload_id: &str) -> AppResult<()> {
        let _permit = self.acquire_request().await?;
        let result = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .as_service_error()
                    .and_then(ProvideErrorMetadata::code)
                    == Some("NoSuchUpload") =>
            {
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 multipart abort failed"
                );
                Err(AppError::ServiceUnavailable(
                    "对象存储分片会话暂时无法终止".into(),
                ))
            }
        }
    }

    async fn delete_internal_best_effort(&self, key: &str) {
        let Ok(_permit) = self.acquire_request().await else {
            return;
        };
        if self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .is_err()
        {
            tracing::warn!("failed to clean up an internal S3 transaction object");
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

    async fn write_upload_transaction(
        &self,
        key: &str,
        transaction: &S3UploadTransaction,
        previous_etag: Option<&str>,
    ) -> AppResult<Option<String>> {
        self.write_json_journal(key, transaction, previous_etag)
            .await
    }

    async fn write_json_journal<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        previous_etag: Option<&str>,
    ) -> AppResult<Option<String>> {
        let data = serde_json::to_vec(value)
            .map_err(|error| AppError::with_source("failed to encode S3 journal", error))?;
        if data.len() > S3_MAX_TRANSACTION_BYTES {
            return Err(AppError::ServiceUnavailable(
                "对象存储事务记录超过安全上限".into(),
            ));
        }
        let content_length = i64::try_from(data.len())
            .map_err(|_| AppError::ServiceUnavailable("对象存储事务记录过大".into()))?;
        if self.is_alibaba_oss() {
            if let Some(expected_etag) = previous_etag {
                let current = self.head_key(key).await?;
                if current
                    .as_ref()
                    .and_then(|metadata| metadata.etag.as_deref())
                    != Some(expected_etag)
                {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储事务记录在更新前发生变化".into(),
                    ));
                }
            }
        }
        let _permit = self.acquire_request().await?;
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_length(content_length)
            .content_type("application/json")
            .body(ByteStream::from(data));
        if !self.is_alibaba_oss() {
            request = if let Some(etag) = previous_etag {
                request.if_match(etag)
            } else {
                request.if_none_match("*")
            };
        }
        let result = if self.is_alibaba_oss() && previous_etag.is_none() {
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
        };
        let output = result.map_err(|error| {
            tracing::error!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 transaction journal write failed"
            );
            AppError::ServiceUnavailable("无法持久化对象存储事务状态".into())
        })?;
        let etag = output
            .e_tag()
            .map(str::to_owned)
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储未返回事务记录 ETag".into()))?;
        Ok(Some(etag))
    }

    async fn register_multipart_session(
        &self,
        key: &str,
        upload_id: &str,
    ) -> AppResult<(String, Option<String>)> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let journal_key = internal_key(&self.prefix, "multipart-sessions", &id);
        let session = S3MultipartSession {
            schema_version: S3_TRANSACTION_SCHEMA_VERSION,
            id,
            key: key.to_owned(),
            upload_id: upload_id.to_owned(),
        };
        validate_multipart_session(&self.prefix, &journal_key, &session)?;
        let etag = self
            .write_json_journal(&journal_key, &session, None)
            .await?;
        Ok((journal_key, etag))
    }

    async fn register_multipart_session_task(
        &self,
        key: &str,
        upload_id: &str,
    ) -> AppResult<(String, Option<String>)> {
        let backend = self.clone();
        let key = key.to_owned();
        let upload_id = upload_id.to_owned();
        tokio::spawn(async move { backend.register_multipart_session(&key, &upload_id).await })
            .await
            .map_err(|error| AppError::with_source("S3 multipart journal task failed", error))?
    }

    async fn delete_transaction_best_effort(&self, key: &str, etag: Option<&str>) {
        if let Err(error) = self.delete_key(key, etag).await {
            tracing::warn!(%error, "failed to clean up an S3 transaction journal");
        }
    }

    async fn list_transaction_keys(&self) -> AppResult<Vec<String>> {
        self.list_recovery_journal_keys("transactions").await
    }

    async fn list_multipart_session_keys(&self) -> AppResult<Vec<String>> {
        self.list_recovery_journal_keys("multipart-sessions").await
    }

    async fn list_recovery_journal_keys(&self, area: &str) -> AppResult<Vec<String>> {
        let prefix = internal_key(&self.prefix, area, "");
        let mut continuation_token: Option<String> = None;
        let mut keys = Vec::new();
        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = S3_MAX_PENDING_TRANSACTIONS
                .saturating_add(1)
                .saturating_sub(keys.len());
            if remaining == 0 {
                return Err(AppError::ServiceUnavailable(
                    "待恢复对象存储记录超过安全上限".into(),
                ));
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 recovery list page size"))?;
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 recovery journal listing failed"
                );
                AppError::ServiceUnavailable("无法列举对象存储恢复记录".into())
            })?;
            drop(permit);

            for object in output.contents() {
                if keys.len() == S3_MAX_PENDING_TRANSACTIONS {
                    return Err(AppError::ServiceUnavailable(
                        "待恢复对象存储记录超过安全上限".into(),
                    ));
                }
                let key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储恢复记录缺少键名".into())
                })?;
                let id = key.strip_prefix(&prefix).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储恢复记录越出保留前缀".into())
                })?;
                if !valid_transaction_id(id) {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储恢复区包含无法识别的记录".into(),
                    ));
                }
                keys.push(key.to_owned());
            }
            if !output.is_truncated().unwrap_or(false) {
                return Ok(keys);
            }
            let next = output
                .next_continuation_token()
                .ok_or_else(|| AppError::ServiceUnavailable("对象存储恢复分页结果无效".into()))?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储恢复分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        Err(AppError::ServiceUnavailable(
            "对象存储恢复分页超过安全页数上限".into(),
        ))
    }

    async fn read_upload_transaction(&self, key: &str) -> AppResult<(S3UploadTransaction, String)> {
        let (transaction, etag) = self.read_json_journal(key).await?;
        validate_upload_transaction(&self.prefix, key, &transaction)?;
        Ok((transaction, etag))
    }

    async fn read_multipart_session(&self, key: &str) -> AppResult<(S3MultipartSession, String)> {
        let (session, etag) = self.read_json_journal(key).await?;
        validate_multipart_session(&self.prefix, key, &session)?;
        Ok((session, etag))
    }

    async fn read_json_journal<T: DeserializeOwned>(&self, key: &str) -> AppResult<(T, String)> {
        let _permit = self.acquire_request().await?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 recovery journal read failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储恢复记录".into())
            })?;
        if output.content_length().is_some_and(|length| {
            length < 0
                || usize::try_from(length).map_or(true, |value| value > S3_MAX_TRANSACTION_BYTES)
        }) {
            return Err(AppError::ServiceUnavailable(
                "对象存储恢复记录超过安全上限".into(),
            ));
        }
        let etag = output
            .e_tag()
            .map(str::to_owned)
            .ok_or_else(|| AppError::ServiceUnavailable("恢复记录缺少 ETag".into()))?;
        let data = output.body.collect().await.map_err(|error| {
            AppError::with_source("failed to stream S3 recovery journal", error)
        })?;
        let data = data.into_bytes();
        if data.len() > S3_MAX_TRANSACTION_BYTES {
            return Err(AppError::ServiceUnavailable(
                "对象存储恢复记录超过安全上限".into(),
            ));
        }
        let value = serde_json::from_slice(&data)
            .map_err(|error| AppError::with_source("invalid S3 recovery journal", error))?;
        Ok((value, etag))
    }

    async fn recover_multipart_session(
        &self,
        journal_key: &str,
        journal_etag: &str,
        session: &S3MultipartSession,
    ) -> AppResult<()> {
        self.abort_multipart_operation(&session.key, &session.upload_id)
            .await?;
        self.delete_key(journal_key, Some(journal_etag)).await?;
        Ok(())
    }

    async fn recover_upload_transaction(
        &self,
        journal_key: &str,
        journal_etag: &str,
        transaction: &S3UploadTransaction,
    ) -> AppResult<()> {
        let destination_key = object_key(&self.prefix, &transaction.relative)?;
        let temporary_key = internal_key(&self.prefix, "uploads", &transaction.id);
        let backup_key = internal_key(&self.prefix, "backups", &transaction.id);
        let destination = self.head_key(&destination_key).await?;

        let committed = destination
            .as_ref()
            .is_some_and(|value| snapshot_matches(value, &transaction.temporary));
        let rolled_back = match transaction.previous.as_ref() {
            Some(previous) => destination
                .as_ref()
                .is_some_and(|value| snapshot_matches(value, previous)),
            None => destination.is_none(),
        };
        if !committed && !rolled_back {
            tracing::error!(
                transaction_id = %transaction.id,
                stage = ?transaction.stage,
                "S3 transaction recovery found an ambiguous destination"
            );
            return Err(AppError::ServiceUnavailable(
                "对象存储事务目标状态不明确；已保留恢复数据并拒绝继续写入".into(),
            ));
        }

        self.delete_internal_best_effort(&temporary_key).await;
        self.delete_internal_best_effort(&backup_key).await;
        self.delete_transaction_best_effort(journal_key, Some(journal_etag))
            .await;
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
    let upload_prefix = internal_key(prefix, "uploads", "");
    let internal_upload = session
        .key
        .strip_prefix(&upload_prefix)
        .is_some_and(valid_transaction_id);
    let regular_object = session.key.strip_prefix(prefix).is_some_and(|relative| {
        !relative.is_empty()
            && !relative.starts_with(".ycloud-system/")
            && StorageService::normalize_relative(relative)
                .is_ok_and(|normalized| normalized == relative)
    });
    if session.schema_version != S3_TRANSACTION_SCHEMA_VERSION
        || !valid_transaction_id(&session.id)
        || journal_key != internal_key(prefix, "multipart-sessions", &session.id)
        || session.upload_id.is_empty()
        || session.upload_id.len() > 4_096
        || session.upload_id.chars().any(char::is_control)
        || (!internal_upload && !regular_object)
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储分片恢复记录无法安全处理".into(),
        ));
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
mod tests {
    use std::{
        collections::{BTreeMap, HashSet},
        net::IpAddr,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    use aws_sdk_s3::types::{CommonPrefix, Object};
    use aws_smithy_types::error::metadata::ProvideErrorMetadata;
    use axum::{
        body::Body,
        http::{HeaderMap, Uri},
    };
    use bytes::Bytes;
    use futures_util::{stream, StreamExt};
    use http_body_util::BodyExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    use crate::{
        config::{normalize_s3_endpoint, Config, S3AddressingStyle, S3Provider, S3StorageConfig},
        error::{AppError, AppResult},
        storage::FileResponseMode,
    };

    use super::{
        copy_source, internal_key, list_prefix, listing::collect_page_entries, multipart_part_size,
        object_key, parent_relative, snapshot_matches, uses_oss_native_write_conditions,
        valid_transaction_id, validate_multipart_session, validate_upload_transaction,
        ExactLengthBody, RawS3Metadata, S3Backend, S3MultipartSession, S3ObjectSnapshot,
        S3UploadStage, S3UploadTransaction, S3_MULTIPART_MAX_PARTS, S3_MULTIPART_MAX_PART_BYTES,
        S3_MULTIPART_THRESHOLD,
    };

    const SMOKE_STREAM_CHUNK_BYTES: u64 = 1024 * 1024;
    const SMOKE_UPLOAD_PART_QUERY: &[u8] = b"partNumber=1";

    struct SmokeFaultProxy {
        endpoint: String,
        armed: Arc<AtomicBool>,
        fired: Arc<AtomicBool>,
        offline: Arc<AtomicBool>,
        sustain_after_cut: Arc<AtomicBool>,
        task: JoinHandle<()>,
    }

    impl SmokeFaultProxy {
        async fn start(target_endpoint: &str) -> AppResult<Self> {
            let uri = target_endpoint.parse::<Uri>().map_err(|error| {
                AppError::with_source("failed to parse fault-proxy target endpoint", error)
            })?;
            if uri.scheme_str() != Some("http") {
                return Err(AppError::ServiceUnavailable(
                    "传输层故障代理仅允许隔离环境中的 HTTP Endpoint".into(),
                ));
            }
            let authority = uri.authority().ok_or_else(|| {
                AppError::ServiceUnavailable("传输层故障代理 Endpoint 缺少地址".into())
            })?;
            let port = authority.port_u16().unwrap_or(80);
            let mut targets = tokio::net::lookup_host((authority.host(), port))
                .await
                .map_err(|error| {
                    AppError::with_source("failed to resolve fault-proxy target", error)
                })?;
            let target = targets.next().ok_or_else(|| {
                AppError::ServiceUnavailable("传输层故障代理无法解析目标地址".into())
            })?;
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .map_err(|error| {
                    AppError::with_source("failed to bind local S3 fault proxy", error)
                })?;
            let local = listener.local_addr().map_err(|error| {
                AppError::with_source("failed to read local S3 fault-proxy address", error)
            })?;
            let armed = Arc::new(AtomicBool::new(false));
            let fired = Arc::new(AtomicBool::new(false));
            let offline = Arc::new(AtomicBool::new(false));
            let sustain_after_cut = Arc::new(AtomicBool::new(false));
            let task_armed = armed.clone();
            let task_fired = fired.clone();
            let task_offline = offline.clone();
            let task_sustain_after_cut = sustain_after_cut.clone();
            let task = tokio::spawn(async move {
                while let Ok((client, _)) = listener.accept().await {
                    if task_offline.load(Ordering::SeqCst) {
                        drop(client);
                        continue;
                    }
                    let connection_armed = task_armed.clone();
                    let connection_fired = task_fired.clone();
                    let connection_offline = task_offline.clone();
                    let connection_sustain_after_cut = task_sustain_after_cut.clone();
                    tokio::spawn(async move {
                        let Ok(upstream) = TcpStream::connect(target).await else {
                            return;
                        };
                        let _ = forward_fault_proxy_connection(
                            client,
                            upstream,
                            connection_armed,
                            connection_fired,
                            connection_offline,
                            connection_sustain_after_cut,
                        )
                        .await;
                    });
                }
            });
            Ok(Self {
                endpoint: format!("http://{local}"),
                armed,
                fired,
                offline,
                sustain_after_cut,
                task,
            })
        }

        fn arm(&self) {
            self.arm_with_outage(false);
        }

        fn arm_sustained(&self) {
            self.arm_with_outage(true);
        }

        fn arm_with_outage(&self, sustained: bool) {
            self.fired.store(false, Ordering::SeqCst);
            self.offline.store(false, Ordering::SeqCst);
            self.sustain_after_cut.store(sustained, Ordering::SeqCst);
            self.armed.store(true, Ordering::SeqCst);
        }

        fn fired(&self) -> bool {
            self.fired.load(Ordering::SeqCst)
        }

        fn restore(&self) {
            self.armed.store(false, Ordering::SeqCst);
            self.sustain_after_cut.store(false, Ordering::SeqCst);
            self.offline.store(false, Ordering::SeqCst);
        }
    }

    impl Drop for SmokeFaultProxy {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn forward_fault_proxy_connection(
        client: TcpStream,
        upstream: TcpStream,
        armed: Arc<AtomicBool>,
        fired: Arc<AtomicBool>,
        offline: Arc<AtomicBool>,
        sustain_after_cut: Arc<AtomicBool>,
    ) -> std::io::Result<()> {
        let (mut client_read, mut client_write) = client.into_split();
        let (mut upstream_read, mut upstream_write) = upstream.into_split();
        tokio::select! {
            result = forward_fault_proxy_requests(
                &mut client_read,
                &mut upstream_write,
                &armed,
                &fired,
                &offline,
                &sustain_after_cut,
            ) => result,
            result = tokio::io::copy(&mut upstream_read, &mut client_write) => {
                result.map(|_| ())
            },
        }
    }

    async fn forward_fault_proxy_requests(
        client: &mut tokio::net::tcp::OwnedReadHalf,
        upstream: &mut tokio::net::tcp::OwnedWriteHalf,
        armed: &AtomicBool,
        fired: &AtomicBool,
        offline: &AtomicBool,
        sustain_after_cut: &AtomicBool,
    ) -> std::io::Result<()> {
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut tail = Vec::with_capacity(SMOKE_UPLOAD_PART_QUERY.len().saturating_sub(1));
        let mut bytes_before_cut = None::<usize>;
        loop {
            let read = client.read(&mut buffer).await?;
            if read == 0 {
                return Ok(());
            }
            if offline.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Some(remaining) = bytes_before_cut.as_mut() {
                let forward = (*remaining).min(read);
                upstream.write_all(&buffer[..forward]).await?;
                *remaining -= forward;
                if *remaining == 0 {
                    fired.store(true, Ordering::SeqCst);
                    if sustain_after_cut.load(Ordering::SeqCst) {
                        offline.store(true, Ordering::SeqCst);
                    }
                    return Ok(());
                }
                continue;
            }

            if armed.load(Ordering::SeqCst) {
                let mut searchable = Vec::with_capacity(tail.len() + read);
                searchable.extend_from_slice(&tail);
                searchable.extend_from_slice(&buffer[..read]);
                if searchable
                    .windows(SMOKE_UPLOAD_PART_QUERY.len())
                    .any(|window| window == SMOKE_UPLOAD_PART_QUERY)
                    && armed
                        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    upstream.write_all(&buffer[..read]).await?;
                    bytes_before_cut = Some(SMOKE_STREAM_CHUNK_BYTES as usize);
                    tail.clear();
                    continue;
                }
                let keep = SMOKE_UPLOAD_PART_QUERY.len().saturating_sub(1);
                tail.clear();
                tail.extend_from_slice(&searchable[searchable.len().saturating_sub(keep)..]);
            }
            upstream.write_all(&buffer[..read]).await?;
        }
    }

    #[test]
    fn multipart_part_sizing_stays_within_provider_limits() {
        assert_eq!(
            multipart_part_size(64 * 1024 * 1024).unwrap(),
            64 * 1024 * 1024
        );
        let four_tebibytes = 4_u64 * 1024 * 1024 * 1024 * 1024;
        let part_size = multipart_part_size(four_tebibytes).unwrap() as u64;
        assert!(four_tebibytes.div_ceil(part_size) <= S3_MULTIPART_MAX_PARTS);
        assert!(part_size <= S3_MULTIPART_MAX_PART_BYTES);
        assert!(multipart_part_size(6_u64 * 1024 * 1024 * 1024 * 1024).is_err());
    }

    fn required_smoke_env(name: &str) -> String {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("missing required smoke-test variable {name}"))
    }

    fn smoke_flag(name: &str) -> bool {
        match std::env::var(name).ok().as_deref() {
            None | Some("") | Some("0") | Some("false") => false,
            Some("1") | Some("true") => true,
            Some(value) => panic!("invalid {name} value {value:?}; expected true, false, 1, or 0"),
        }
    }

    fn smoke_pattern_body(content_length: u64) -> Body {
        let chunks = stream::unfold(0_u64, move |offset| async move {
            if offset >= content_length {
                return None;
            }
            let length = SMOKE_STREAM_CHUNK_BYTES.min(content_length - offset) as usize;
            let value = ((offset / SMOKE_STREAM_CHUNK_BYTES) % 251) as u8;
            let next = offset + length as u64;
            Some((
                Ok::<_, std::io::Error>(Bytes::from(vec![value; length])),
                next,
            ))
        });
        Body::from_stream(chunks)
    }

    fn interrupted_smoke_body(bytes_before_failure: u64) -> Body {
        let chunks = stream::unfold(
            (0_u64, false),
            move |(offset, failure_emitted)| async move {
                if failure_emitted {
                    return None;
                }
                if offset >= bytes_before_failure {
                    return Some((
                        Err::<Bytes, _>(std::io::Error::other(
                            "injected multipart request-body interruption",
                        )),
                        (offset, true),
                    ));
                }
                let length = SMOKE_STREAM_CHUNK_BYTES.min(bytes_before_failure - offset) as usize;
                let next = offset + length as u64;
                Some((Ok(Bytes::from(vec![0xA5; length])), (next, false)))
            },
        );
        Body::from_stream(chunks)
    }

    async fn verify_smoke_pattern_download(
        backend: &S3Backend,
        relative: &str,
        expected_length: u64,
    ) -> AppResult<()> {
        let response = backend
            .stream_file(relative, &HeaderMap::new(), FileResponseMode::Attachment)
            .await?;
        let mut body = response.into_body().into_data_stream();
        let mut received = 0_u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|error| {
                AppError::with_source("failed to stream multipart smoke download", error)
            })?;
            let mut local_offset = 0_usize;
            while local_offset < chunk.len() {
                let absolute_offset = received + local_offset as u64;
                let expected = ((absolute_offset / SMOKE_STREAM_CHUNK_BYTES) % 251) as u8;
                let until_boundary =
                    SMOKE_STREAM_CHUNK_BYTES - (absolute_offset % SMOKE_STREAM_CHUNK_BYTES);
                let length = (until_boundary as usize).min(chunk.len() - local_offset);
                if !chunk[local_offset..local_offset + length]
                    .iter()
                    .all(|value| *value == expected)
                {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储 Multipart 下载内容校验失败".into(),
                    ));
                }
                local_offset += length;
            }
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or(AppError::PayloadTooLarge)?;
        }
        if received != expected_length {
            return Err(AppError::ServiceUnavailable(
                "对象存储 Multipart 下载长度校验失败".into(),
            ));
        }
        Ok(())
    }

    async fn cleanup_smoke_multipart_state(backend: &S3Backend) {
        if let Ok(output) = backend
            .client
            .list_multipart_uploads()
            .bucket(&backend.bucket)
            .prefix(&backend.prefix)
            .max_uploads(1_000)
            .send()
            .await
        {
            let uploads = output
                .uploads()
                .iter()
                .filter_map(|upload| {
                    Some((upload.key()?.to_owned(), upload.upload_id()?.to_owned()))
                })
                .collect::<Vec<_>>();
            for (key, upload_id) in uploads {
                let _ = backend.abort_multipart_operation(&key, &upload_id).await;
            }
        }
        if let Ok(keys) = backend.list_multipart_session_keys().await {
            for key in keys {
                let _ = backend.delete_key(&key, None).await;
            }
        }
    }

    async fn cleanup_smoke_base_multipart_state(backend: &S3Backend, base_prefix: &str) {
        if let Ok(output) = backend
            .client
            .list_multipart_uploads()
            .bucket(&backend.bucket)
            .prefix(base_prefix)
            .max_uploads(1_000)
            .send()
            .await
        {
            let uploads = output
                .uploads()
                .iter()
                .filter_map(|upload| {
                    Some((upload.key()?.to_owned(), upload.upload_id()?.to_owned()))
                })
                .collect::<Vec<_>>();
            for (key, upload_id) in uploads {
                let _ = backend.abort_multipart_operation(&key, &upload_id).await;
            }
        }

        if let Ok(output) = backend
            .client
            .list_objects_v2()
            .bucket(&backend.bucket)
            .prefix(base_prefix)
            .max_keys(1_000)
            .send()
            .await
        {
            const MARKER: &str = "/.ycloud-system/multipart-sessions/";
            let keys = output
                .contents()
                .iter()
                .filter_map(|object| object.key())
                .filter(|key| {
                    key.rsplit_once(MARKER)
                        .is_some_and(|(_, id)| valid_transaction_id(id))
                })
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for key in keys {
                let _ = backend.delete_key(&key, None).await;
            }
        }
    }

    fn smoke_provider(value: Option<&str>) -> S3Provider {
        match value.unwrap_or("minio") {
            "alibaba_oss" => S3Provider::AlibabaOss,
            "tencent_cos" => S3Provider::TencentCos,
            "minio" => S3Provider::Minio,
            "s3_compatible" => S3Provider::S3Compatible,
            value => panic!(
                "invalid YCLOUD_S3_SMOKE_PROVIDER {value:?}; expected alibaba_oss, tencent_cos, minio, or s3_compatible"
            ),
        }
    }

    fn smoke_addressing_style(value: Option<&str>, provider: S3Provider) -> S3AddressingStyle {
        match value {
            Some("path") => S3AddressingStyle::Path,
            Some("virtual_hosted") => S3AddressingStyle::VirtualHosted,
            Some(value) => panic!(
                "invalid YCLOUD_S3_SMOKE_ADDRESSING_STYLE {value:?}; expected path or virtual_hosted"
            ),
            None if matches!(provider, S3Provider::AlibabaOss | S3Provider::TencentCos) => {
                S3AddressingStyle::VirtualHosted
            }
            None => S3AddressingStyle::Path,
        }
    }

    fn sanitize_smoke_text(mut text: String, secrets: &[&str]) -> String {
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            text = text.replace(secret, "<redacted>");
        }
        text.chars().take(1_024).collect()
    }

    fn sanitized_smoke_error_chain(
        error: &(dyn std::error::Error + 'static),
        secrets: &[&str],
    ) -> String {
        let mut parts = Vec::new();
        let mut current = Some(error);
        while let Some(cause) = current {
            parts.push(sanitize_smoke_text(cause.to_string(), secrets));
            current = cause.source();
        }
        parts.join(" -> ").chars().take(1_024).collect()
    }

    /// Opt-in compatibility test for an isolated S3-compatible bucket.
    ///
    /// The test never runs in the normal suite and creates a unique Prefix for
    /// every execution. Credentials are read only from process environment
    /// variables and are never printed. Set `YCLOUD_S3_SMOKE_MULTIPART=1`
    /// to add a streamed upload larger than the 64 MiB Multipart threshold,
    /// `YCLOUD_S3_SMOKE_CHECK_INVALID_CREDENTIALS=1` verifies that the service
    /// rejects a valid access-key ID paired with an invalid secret. Set
    /// `YCLOUD_S3_SMOKE_INTERRUPTED_MULTIPART=1` to fail the request body after
    /// the first part and verify that Ycloud aborts the Multipart session. Set
    /// `YCLOUD_S3_SMOKE_TRANSPORT_CUT=1` for HTTP test endpoints to route the
    /// test through a loopback proxy that cuts the first UploadPart connection.
    /// `YCLOUD_S3_SMOKE_SUSTAINED_OUTAGE=1` keeps that proxy offline while the
    /// immediate Abort fails, restores it, and exercises persisted recovery.
    #[tokio::test]
    #[ignore = "requires an isolated S3 test bucket and explicit credentials"]
    async fn s3_compatibility_smoke() {
        let configured_endpoint = required_smoke_env("YCLOUD_S3_SMOKE_ENDPOINT");
        let bucket = required_smoke_env("YCLOUD_S3_SMOKE_BUCKET");
        let access_key_id = required_smoke_env("YCLOUD_S3_SMOKE_ACCESS_KEY_ID");
        let secret_access_key = required_smoke_env("YCLOUD_S3_SMOKE_SECRET_ACCESS_KEY");
        let provider = smoke_provider(std::env::var("YCLOUD_S3_SMOKE_PROVIDER").ok().as_deref());
        let addressing_style = smoke_addressing_style(
            std::env::var("YCLOUD_S3_SMOKE_ADDRESSING_STYLE")
                .ok()
                .as_deref(),
            provider,
        );
        let region = std::env::var("YCLOUD_S3_SMOKE_REGION").unwrap_or_else(|_| "us-east-1".into());
        let base_prefix =
            std::env::var("YCLOUD_S3_SMOKE_PREFIX").unwrap_or_else(|_| "ycloud-smoke/".into());
        let multipart_length = smoke_flag("YCLOUD_S3_SMOKE_MULTIPART")
            .then_some(S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES);
        let check_invalid_credentials = smoke_flag("YCLOUD_S3_SMOKE_CHECK_INVALID_CREDENTIALS");
        let check_interrupted_multipart = smoke_flag("YCLOUD_S3_SMOKE_INTERRUPTED_MULTIPART");
        let check_transport_cut = smoke_flag("YCLOUD_S3_SMOKE_TRANSPORT_CUT");
        let check_sustained_outage = smoke_flag("YCLOUD_S3_SMOKE_SUSTAINED_OUTAGE");
        let fault_proxy = if check_transport_cut || check_sustained_outage {
            Some(SmokeFaultProxy::start(&configured_endpoint).await.unwrap())
        } else {
            None
        };
        let endpoint = fault_proxy.as_ref().map_or_else(
            || configured_endpoint.clone(),
            |proxy| proxy.endpoint.clone(),
        );
        let large_object_length = multipart_length.or_else(|| {
            (check_interrupted_multipart || check_transport_cut || check_sustained_outage)
                .then_some(S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES)
        });
        let base_prefix = base_prefix.trim_matches('/');
        let smoke_root_prefix = if base_prefix.is_empty() {
            String::new()
        } else {
            format!("{base_prefix}/")
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let prefix = if base_prefix.is_empty() {
            format!("{run_id}/")
        } else {
            format!("{base_prefix}/{run_id}/")
        };
        let capacity_limit = large_object_length
            .and_then(|length| length.checked_mul(4))
            .unwrap_or(1024 * 1024);
        let settings = S3StorageConfig {
            provider,
            endpoint: endpoint.clone(),
            bucket,
            region,
            prefix,
            addressing_style,
            access_key_id,
            secret_access_key,
            capacity_limit_bytes: Some(capacity_limit),
        };
        let runtime = Config {
            bind_address: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            storage_path: PathBuf::from("unused-smoke-local-storage"),
            local_mounts: crate::storage_catalog::LocalMountCatalog::new(
                PathBuf::from("unused-smoke-local-storage"),
                Vec::new(),
            )
            .unwrap(),
            config_path: PathBuf::from("unused-smoke-config.json"),
            max_upload_bytes: large_object_length.unwrap_or(1024 * 1024),
            max_upload_batch_bytes: 100 * 1024 * 1024 * 1024,
            max_upload_batch_entries: 10_000,
            max_archive_bytes: 100 * 1024 * 1024 * 1024,
            max_archive_entries: 100_000,
            io_concurrency: 2,
            max_list_entries: 100,
            request_timeout_secs: 30,
            upload_timeout_secs: if large_object_length.is_some() {
                120
            } else {
                30
            },
            disk_reserve_bytes: 0,
            secure_cookies: false,
            allow_lan_http: true,
            public_base_url: None,
            public_host: None,
            trusted_proxy_ips: HashSet::new(),
            allowed_hosts: HashSet::new(),
            s3_allowed_endpoints: HashSet::from([normalize_s3_endpoint(&endpoint).unwrap()]),
        };
        let backend = S3Backend::new(&settings, &runtime).unwrap();
        let payload = Bytes::from_static(b"ycloud-s3-compatibility-smoke");

        let result: AppResult<()> = async {
            cleanup_smoke_base_multipart_state(&backend, &smoke_root_prefix).await;
            if check_invalid_credentials {
                let mut invalid_settings = settings.clone();
                invalid_settings.secret_access_key =
                    format!("invalid-{}", uuid::Uuid::new_v4().simple());
                let invalid_backend = S3Backend::new(&invalid_settings, &runtime)?;
                if invalid_backend.probe().await.is_ok() {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储接受了错误凭据".into(),
                    ));
                }
            }
            if let Err(error) = backend
                .client
                .list_objects_v2()
                .bucket(&backend.bucket)
                .prefix(&backend.prefix)
                .max_keys(1)
                .send()
                .await
            {
                let service = error.as_service_error();
                let secrets = [
                    settings.access_key_id.as_str(),
                    settings.secret_access_key.as_str(),
                ];
                let code = service
                    .and_then(ProvideErrorMetadata::code)
                    .unwrap_or("transport");
                let message = sanitize_smoke_text(
                    service
                        .and_then(ProvideErrorMetadata::message)
                        .unwrap_or("no service message")
                        .to_owned(),
                    &secrets,
                );
                let status = error
                    .raw_response()
                    .map(|response| response.status().as_u16());
                let chain = sanitized_smoke_error_chain(&error, &secrets);
                panic!(
                    "S3 list probe failed for {provider:?}: status={status:?}, code={code}, message={message}, cause={chain}"
                );
            }
            backend.activation_probe().await?;
            assert_eq!(backend.user_data_size().await?, 0);
            backend.create_directory("suite").await?;
            let uploaded = backend
                .upload_file(
                    "suite/source.bin",
                    Body::from(payload.clone()),
                    payload.len() as u64,
                    1024 * 1024,
                    Some("application/octet-stream"),
                )
                .await?;
            assert_eq!(uploaded.previous_size, 0);
            assert_eq!(uploaded.size, payload.len() as u64);

            let response = backend
                .stream_file(
                    "suite/source.bin",
                    &HeaderMap::new(),
                    FileResponseMode::Attachment,
                )
                .await?;
            let downloaded = response
                .into_body()
                .collect()
                .await
                .map_err(|error| AppError::with_source("failed to collect smoke download", error))?
                .to_bytes();
            assert_eq!(downloaded, payload);

            backend
                .copy_file("suite/source.bin", "suite/copied.bin")
                .await?;
            backend
                .move_file("suite/copied.bin", "suite/moved.bin")
                .await?;
            assert_eq!(
                backend.delete_file("suite/moved.bin").await?,
                payload.len() as u64
            );

            let replacement = Bytes::from_static(b"replacement");
            let replaced = backend
                .upload_file(
                    "suite/source.bin",
                    Body::from(replacement.clone()),
                    replacement.len() as u64,
                    1024 * 1024,
                    Some("application/octet-stream"),
                )
                .await?;
            assert_eq!(replaced.previous_size, payload.len() as u64);
            assert_eq!(replaced.size, replacement.len() as u64);

            backend.copy_directory("suite", "suite-copy").await?;
            backend.move_directory("suite-copy", "suite-moved").await?;
            assert_eq!(
                backend.delete_directory("suite-moved").await?,
                replacement.len() as u64
            );
            assert_eq!(
                backend.delete_directory("suite").await?,
                replacement.len() as u64
            );
            assert_eq!(backend.user_data_size().await?, 0);

            if let Some(content_length) = multipart_length {
                backend.create_directory("multipart").await?;
                let uploaded = backend
                    .upload_file(
                        "multipart/source.bin",
                        smoke_pattern_body(content_length),
                        content_length,
                        content_length,
                        Some("application/octet-stream"),
                    )
                    .await?;
                assert_eq!(uploaded.previous_size, 0);
                assert_eq!(uploaded.size, content_length);
                verify_smoke_pattern_download(
                    &backend,
                    "multipart/source.bin",
                    content_length,
                )
                .await?;

                backend
                    .copy_file("multipart/source.bin", "multipart/copied.bin")
                    .await?;
                backend
                    .move_file("multipart/copied.bin", "multipart/moved.bin")
                    .await?;
                assert_eq!(
                    backend.delete_file("multipart/moved.bin").await?,
                    content_length
                );
                assert_eq!(
                    backend.delete_file("multipart/source.bin").await?,
                    content_length
                );
                assert_eq!(backend.delete_directory("multipart").await?, 0);
                assert_eq!(backend.user_data_size().await?, 0);
            }

            if check_interrupted_multipart {
                let content_length = S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES;
                backend.create_directory("interrupted").await?;
                let upload = backend
                    .upload_file(
                        "interrupted/source.bin",
                        interrupted_smoke_body(S3_MULTIPART_THRESHOLD),
                        content_length,
                        content_length,
                        Some("application/octet-stream"),
                    )
                    .await;
                if upload.is_ok() {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储接受了中途断流的 Multipart 上传".into(),
                    ));
                }
                match backend.metadata("interrupted/source.bin").await {
                    Err(AppError::NotFound) => {}
                    Ok(_) => {
                        return Err(AppError::ServiceUnavailable(
                            "中途断流后目标对象仍然可见".into(),
                        ));
                    }
                    Err(error) => return Err(error),
                }
                let pending = backend
                    .client
                    .list_multipart_uploads()
                    .bucket(&backend.bucket)
                    .prefix(&backend.prefix)
                    .max_uploads(1)
                    .send()
                    .await
                    .map_err(|error| {
                        AppError::with_source(
                            "failed to inspect interrupted multipart uploads",
                            error,
                        )
                    })?;
                if !pending.uploads().is_empty() {
                    return Err(AppError::ServiceUnavailable(
                        "中途断流后遗留了未完成的 Multipart 会话".into(),
                    ));
                }
                assert_eq!(backend.delete_directory("interrupted").await?, 0);
                assert_eq!(backend.user_data_size().await?, 0);
            }

            if check_transport_cut {
                let proxy = fault_proxy.as_ref().expect("fault proxy is configured");
                let content_length = S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES;
                backend.create_directory("transport-cut").await?;
                proxy.arm();
                let upload = backend
                    .upload_file(
                        "transport-cut/source.bin",
                        smoke_pattern_body(content_length),
                        content_length,
                        content_length,
                        Some("application/octet-stream"),
                    )
                    .await;
                if upload.is_ok() || !proxy.fired() {
                    return Err(AppError::ServiceUnavailable(
                        "传输层故障代理没有中断 Multipart 上传".into(),
                    ));
                }
                match backend.metadata("transport-cut/source.bin").await {
                    Err(AppError::NotFound) => {}
                    Ok(_) => {
                        return Err(AppError::ServiceUnavailable(
                            "传输层中断后目标对象仍然可见".into(),
                        ));
                    }
                    Err(error) => return Err(error),
                }
                let pending = backend
                    .client
                    .list_multipart_uploads()
                    .bucket(&backend.bucket)
                    .prefix(&backend.prefix)
                    .max_uploads(1)
                    .send()
                    .await
                    .map_err(|error| {
                        AppError::with_source(
                            "failed to inspect transport-cut multipart uploads",
                            error,
                        )
                    })?;
                if !pending.uploads().is_empty() {
                    return Err(AppError::ServiceUnavailable(
                        "传输层中断后遗留了未完成的 Multipart 会话".into(),
                    ));
                }
                assert_eq!(backend.delete_directory("transport-cut").await?, 0);
                assert_eq!(backend.user_data_size().await?, 0);
            }

            if check_sustained_outage {
                let proxy = fault_proxy.as_ref().expect("fault proxy is configured");
                let content_length = S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES;
                backend.create_directory("sustained-outage").await?;
                proxy.arm_sustained();
                let upload = backend
                    .upload_file(
                        "sustained-outage/source.bin",
                        smoke_pattern_body(content_length),
                        content_length,
                        content_length,
                        Some("application/octet-stream"),
                    )
                    .await;
                let fault_fired = proxy.fired();
                proxy.restore();
                if upload.is_ok() || !fault_fired {
                    return Err(AppError::ServiceUnavailable(
                        "持续断网代理没有中断 Multipart 上传".into(),
                    ));
                }
                let recovered = backend.recover_transactions().await?;
                if recovered == 0 {
                    return Err(AppError::ServiceUnavailable(
                        "持续断网后没有发现持久化的 Multipart 恢复记录".into(),
                    ));
                }
                match backend.metadata("sustained-outage/source.bin").await {
                    Err(AppError::NotFound) => {}
                    Ok(_) => {
                        return Err(AppError::ServiceUnavailable(
                            "持续断网恢复后目标对象仍然可见".into(),
                        ));
                    }
                    Err(error) => return Err(error),
                }
                let pending = backend
                    .client
                    .list_multipart_uploads()
                    .bucket(&backend.bucket)
                    .prefix(&backend.prefix)
                    .max_uploads(1)
                    .send()
                    .await
                    .map_err(|error| {
                        AppError::with_source(
                            "failed to inspect sustained-outage multipart uploads",
                            error,
                        )
                    })?;
                if !pending.uploads().is_empty() {
                    return Err(AppError::ServiceUnavailable(
                        "持续断网恢复后仍遗留未完成的 Multipart 会话".into(),
                    ));
                }
                assert_eq!(backend.delete_directory("sustained-outage").await?, 0);
                assert_eq!(backend.user_data_size().await?, 0);
            }
            Ok(())
        }
        .await;

        // Preserve transaction evidence on ambiguous failures, but clean all
        // ordinary test paths so successful and simple failed runs do not
        // contaminate later capacity scans.
        let _ = backend.recover_transactions().await;
        cleanup_smoke_multipart_state(&backend).await;
        for path in [
            "sustained-outage",
            "transport-cut",
            "interrupted",
            "multipart",
            "suite-moved",
            "suite-copy",
            "suite",
        ] {
            let _ = backend.delete_directory(path).await;
        }
        result.unwrap();
    }

    #[test]
    fn smoke_defaults_match_provider_addressing_requirements() {
        assert_eq!(smoke_provider(None), S3Provider::Minio);
        assert_eq!(
            smoke_addressing_style(None, S3Provider::Minio),
            S3AddressingStyle::Path
        );
        assert_eq!(
            smoke_addressing_style(None, S3Provider::TencentCos),
            S3AddressingStyle::VirtualHosted
        );
        assert_eq!(
            smoke_addressing_style(None, S3Provider::AlibabaOss),
            S3AddressingStyle::VirtualHosted
        );
        assert!(uses_oss_native_write_conditions(S3Provider::AlibabaOss));
        assert!(!uses_oss_native_write_conditions(S3Provider::TencentCos));
        assert!(!uses_oss_native_write_conditions(S3Provider::Minio));
        assert!(!uses_oss_native_write_conditions(S3Provider::S3Compatible));
    }

    #[test]
    fn object_keys_are_confined_below_the_configured_prefix() {
        let prefix = "users/yogrut/";
        assert_eq!(
            object_key(prefix, "documents/report.pdf").unwrap(),
            "users/yogrut/documents/report.pdf"
        );
        assert_eq!(
            list_prefix(prefix, "documents").unwrap(),
            "users/yogrut/documents/"
        );
        assert_eq!(list_prefix(prefix, "/").unwrap(), "users/yogrut/");
        assert!(object_key(prefix, "../outside").is_err());
        assert!(object_key(prefix, ".ycloud-system/journal").is_err());
    }

    #[test]
    fn listing_only_exposes_direct_children_below_the_prefix() {
        let prefixes = vec![CommonPrefix::builder()
            .prefix("users/yogrut/photos/")
            .build()];
        let objects = vec![
            Object::builder()
                .key("users/yogrut/report.pdf")
                .size(42)
                .build(),
            Object::builder()
                .key("users/yogrut/nested/hidden.txt")
                .size(8)
                .build(),
            Object::builder().key("outside/secret.txt").size(12).build(),
        ];
        let mut entries = BTreeMap::new();
        collect_page_entries("", "users/yogrut/", &prefixes, &objects, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.get("photos").unwrap().is_dir);
        assert_eq!(entries.get("report.pdf").unwrap().size, 42);
        assert!(!entries.contains_key("secret.txt"));
        assert!(!entries.contains_key("hidden.txt"));
    }

    #[test]
    fn listing_rejects_file_and_directory_name_collisions() {
        let prefixes = vec![CommonPrefix::builder()
            .prefix("users/yogrut/archive/")
            .build()];
        let objects = vec![Object::builder()
            .key("users/yogrut/archive")
            .size(1)
            .build()];
        let mut entries = BTreeMap::new();
        assert!(
            collect_page_entries("", "users/yogrut/", &prefixes, &objects, &mut entries,).is_err()
        );
    }

    #[test]
    fn listing_hides_reserved_system_names() {
        let prefixes = vec![CommonPrefix::builder()
            .prefix("users/yogrut/.ycloud-system/")
            .build()];
        let mut entries = BTreeMap::new();
        collect_page_entries("", "users/yogrut/", &prefixes, &[], &mut entries).unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn upload_stream_enforces_exact_content_length() {
        let complete = stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from_static(b"abc")),
            Ok(Bytes::from_static(b"def")),
        ]);
        let result = ExactLengthBody::new(complete, 6).collect().await;
        assert_eq!(result.unwrap().to_bytes(), Bytes::from_static(b"abcdef"));

        let short = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))]);
        assert!(ExactLengthBody::new(short, 4).collect().await.is_err());

        let long = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"abcd"))]);
        assert!(ExactLengthBody::new(long, 3).collect().await.is_err());
    }

    #[test]
    fn transaction_keys_are_hidden_and_copy_sources_are_encoded() {
        assert_eq!(
            internal_key("users/yogrut/", "uploads", "1234"),
            "users/yogrut/.ycloud-system/uploads/1234"
        );
        assert_eq!(
            copy_source("bucket", "users/yogrut/游戏 备份.zip"),
            "bucket/users/yogrut/%E6%B8%B8%E6%88%8F%20%E5%A4%87%E4%BB%BD.zip"
        );
    }

    #[test]
    fn parent_paths_remain_relative_to_the_storage_prefix() {
        assert_eq!(parent_relative("file.txt"), "");
        assert_eq!(parent_relative("folder/file.txt"), "folder");
        assert_eq!(parent_relative("a/b/file.txt"), "a/b");
    }

    #[test]
    fn transaction_records_are_confined_and_require_etags() {
        let transaction = S3UploadTransaction {
            schema_version: 1,
            id: "0123456789abcdef0123456789abcdef".into(),
            relative: "folder/file.bin".into(),
            stage: S3UploadStage::Prepared,
            temporary: S3ObjectSnapshot {
                size: 42,
                etag: Some("new".into()),
            },
            previous: Some(S3ObjectSnapshot {
                size: 21,
                etag: Some("old".into()),
            }),
        };
        let key = internal_key("tenant/", "transactions", &transaction.id);
        assert!(validate_upload_transaction("tenant/", &key, &transaction).is_ok());
        assert!(valid_transaction_id(&transaction.id));

        let mut escaped = transaction.clone();
        escaped.relative = "../outside".into();
        assert!(validate_upload_transaction("tenant/", &key, &escaped).is_err());

        let mut unverifiable = transaction.clone();
        unverifiable.temporary.etag = None;
        assert!(validate_upload_transaction("tenant/", &key, &unverifiable).is_err());
    }

    #[test]
    fn multipart_recovery_records_are_confined_to_the_backend_namespace() {
        let id = "0123456789abcdef0123456789abcdef";
        let mut session = S3MultipartSession {
            schema_version: 1,
            id: id.into(),
            key: format!("tenant/.ycloud-system/uploads/{id}"),
            upload_id: "provider-upload-id".into(),
        };
        let journal_key = internal_key("tenant/", "multipart-sessions", id);
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

        session.key = "tenant/folder/file.bin".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

        session.key = "other-tenant/file.bin".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());

        session.key = "tenant/.ycloud-system/transactions/forged".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());

        session.key = "tenant/folder/file.bin".into();
        session.upload_id = "bad\nupload-id".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
    }

    #[test]
    fn recovery_snapshots_match_both_size_and_etag() {
        let metadata = RawS3Metadata {
            size: 42,
            etag: Some("etag-a".into()),
        };
        assert!(snapshot_matches(
            &metadata,
            &S3ObjectSnapshot {
                size: 42,
                etag: Some("etag-a".into()),
            }
        ));
        assert!(!snapshot_matches(
            &metadata,
            &S3ObjectSnapshot {
                size: 42,
                etag: Some("etag-b".into()),
            }
        ));
        assert!(!snapshot_matches(
            &metadata,
            &S3ObjectSnapshot {
                size: 41,
                etag: Some("etag-a".into()),
            }
        ));
    }
}
