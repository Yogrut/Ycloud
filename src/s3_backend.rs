use std::{
    collections::BTreeMap,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig, BehaviorVersion, Credentials, Region},
    types::{CommonPrefix, Object},
    Client,
};
use aws_smithy_http_client::Builder as HttpClientBuilder;
use aws_smithy_types::byte_stream::ByteStream;
use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
};
use bytes::Bytes;
use futures_util::Stream;
use http_body::{Frame, SizeHint};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;

use crate::{
    config::{
        validate_storage_backend, Config, S3AddressingStyle, S3StorageConfig, StorageBackendConfig,
    },
    error::{AppError, AppResult},
    storage::{
        attachment_header, content_type_for_mode, parse_range, FileResponseMode, StorageService,
    },
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

mod directory_transaction;

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

/// One reusable S3 client and its confined namespace.
///
/// Provider presets are validated before construction and all object keys are
/// rooted below `prefix`. The client never retains a second plaintext copy of
/// credentials outside the AWS credential provider.
#[derive(Clone)]
pub struct S3Backend {
    client: Client,
    bucket: String,
    prefix: String,
    request_gate: std::sync::Arc<Semaphore>,
    mutation_gate: std::sync::Arc<Mutex<()>>,
    upload_timeout: Duration,
}

impl S3Backend {
    pub fn new(settings: &S3StorageConfig, runtime: &Config) -> AppResult<Self> {
        validate_storage_backend(&StorageBackendConfig::S3(settings.clone()))?;
        runtime.allows_storage_backend(&StorageBackendConfig::S3(settings.clone()))?;

        let credentials = Credentials::new(
            settings.access_key_id.clone(),
            settings.secret_access_key.clone(),
            None,
            None,
            "ycloud-static-configuration",
        );
        let timeouts = TimeoutConfig::builder()
            .connect_timeout(S3_CONNECT_TIMEOUT)
            .operation_attempt_timeout(S3_ATTEMPT_TIMEOUT)
            .operation_timeout(S3_OPERATION_TIMEOUT)
            .build();
        let mut sdk_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(Region::new(settings.region.clone()))
            .endpoint_url(settings.endpoint.clone())
            .force_path_style(settings.addressing_style == S3AddressingStyle::Path)
            .disable_multi_region_access_points(true)
            .disable_s3_express_session_auth(true)
            .retry_config(RetryConfig::standard().with_max_attempts(S3_MAX_ATTEMPTS))
            .timeout_config(timeouts);

        // Private MinIO/RustFS deployments commonly use HTTP behind an
        // encrypted EasyTier network. Such endpoints do not need a TLS stack
        // and must not depend on the developer machine's certificate store.
        // HTTPS endpoints continue to use the SDK's verified Rustls client.
        if settings.endpoint.starts_with("http://") {
            let http_client = HttpClientBuilder::new()
                .pool_idle_timeout(S3_IDLE_CONNECTION_TIMEOUT)
                .pool_max_idle_per_host(S3_MAX_IDLE_CONNECTIONS_PER_HOST)
                .build_http();
            sdk_config = sdk_config.http_client(http_client);
        }

        let sdk_config = sdk_config.build();

        Ok(Self {
            client: Client::from_conf(sdk_config),
            bucket: settings.bucket.clone(),
            prefix: settings.prefix.clone(),
            request_gate: std::sync::Arc::new(Semaphore::new(S3_REQUEST_CONCURRENCY)),
            mutation_gate: std::sync::Arc::new(Mutex::new(())),
            upload_timeout: Duration::from_secs(runtime.upload_timeout_secs),
        })
    }

    /// Verify the minimum permission required by the file browser. The error
    /// returned to callers is intentionally generic so upstream SDK responses
    /// cannot leak credentials or signed request details into the UI.
    pub async fn probe(&self) -> AppResult<()> {
        let _permit = self.acquire_request().await?;
        self.client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&self.prefix)
            .max_keys(1)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(error_kind = %error.as_service_error().map_or("transport", |_| "service"), "S3 storage probe failed");
                AppError::ServiceUnavailable(
                    "无法连接对象存储或当前凭据缺少列举权限".into(),
                )
            })?;
        Ok(())
    }

    /// Verify every object capability required before this backend can serve
    /// user traffic. The probe is confined to Ycloud's reserved prefix and
    /// always attempts cleanup; it never touches a user-visible object key.
    pub async fn activation_probe(&self) -> AppResult<()> {
        self.probe().await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let source_key = internal_key(&self.prefix, "activation-tests", &id);
        let copy_key = internal_key(&self.prefix, "activation-tests", &format!("{id}-copy"));
        let payload = format!("ycloud-storage-activation:{id}").into_bytes();
        let result = self
            .activation_probe_inner(&source_key, &copy_key, &payload)
            .await;
        if result.is_err() {
            self.delete_internal_best_effort(&copy_key).await;
            self.delete_internal_best_effort(&source_key).await;
        }
        result
    }

    async fn activation_probe_inner(
        &self,
        source_key: &str,
        copy_key: &str,
        payload: &[u8],
    ) -> AppResult<()> {
        let length = i64::try_from(payload.len())
            .map_err(|_| AppError::ServiceUnavailable("对象存储激活探测数据无效".into()))?;
        let _permit = self.acquire_request().await?;
        let output = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(source_key)
            .content_length(length)
            .if_none_match("*")
            .body(ByteStream::from(payload.to_vec()))
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 activation write probe failed"
                );
                AppError::ServiceUnavailable("对象存储缺少安全写入权限或条件写入能力".into())
            })?;
        let source_etag = output
            .e_tag()
            .map(str::to_owned)
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储写入未返回 ETag".into()))?;
        drop(_permit);

        let source = self
            .head_key(source_key)
            .await?
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储写入探测对象不可见".into()))?;
        if source.size != payload.len() as u64 || source.etag.as_deref() != Some(&source_etag) {
            return Err(AppError::ServiceUnavailable(
                "对象存储写入后的长度或版本校验失败".into(),
            ));
        }

        let _permit = self.acquire_request().await?;
        let downloaded = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(source_key)
            .if_match(&source_etag)
            .send()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储读取校验失败".into()))?
            .body
            .collect()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储读取响应不完整".into()))?
            .into_bytes();
        drop(_permit);
        if downloaded.as_ref() != payload {
            return Err(AppError::ServiceUnavailable(
                "对象存储读取内容校验失败".into(),
            ));
        }

        let copied_etag = self
            .copy_key(source_key, copy_key, Some(&source_etag), true)
            .await?;
        let copied = self
            .head_key(copy_key)
            .await?
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储复制探测对象不可见".into()))?;
        if copied.size != payload.len() as u64 || copied.etag.as_deref() != Some(&copied_etag) {
            return Err(AppError::ServiceUnavailable(
                "对象存储服务端复制校验失败".into(),
            ));
        }
        self.delete_key(copy_key, Some(&copied_etag)).await?;
        self.delete_key(source_key, Some(&source_etag)).await?;
        Ok(())
    }

    pub fn object_key(&self, relative: &str) -> AppResult<String> {
        object_key(&self.prefix, relative)
    }

    pub fn list_prefix(&self, relative: &str) -> AppResult<String> {
        list_prefix(&self.prefix, relative)
    }

    /// Sum the user namespace once when a backend is prepared. S3 has no
    /// portable bucket-capacity API, so logical quota accounting is scoped to
    /// Ycloud's configured prefix and excludes its reserved transaction area.
    pub async fn user_data_size(&self) -> AppResult<u64> {
        self.sum_object_bytes(&self.prefix, true).await
    }

    pub async fn path_size(&self, relative: &str) -> AppResult<u64> {
        let metadata = self.metadata(relative).await?;
        if !metadata.is_dir {
            return Ok(metadata.size);
        }
        let prefix = list_prefix(&self.prefix, relative)?;
        self.sum_object_bytes(&prefix, false).await
    }

    async fn sum_object_bytes(&self, prefix: &str, exclude_internal: bool) -> AppResult<u64> {
        let internal_prefix = format!("{}.ycloud-system/", self.prefix);
        let mut continuation_token: Option<String> = None;
        let mut total = 0_u64;
        for _ in 0..S3_MAX_CAPACITY_SCAN_PAGES {
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .max_keys(i32::try_from(S3_PAGE_SIZE).unwrap_or(1_000));
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 capacity scan failed"
                );
                AppError::ServiceUnavailable("无法统计对象存储已用容量".into())
            })?;
            drop(permit);

            for object in output.contents() {
                let key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储容量统计遇到无键名对象".into())
                })?;
                if exclude_internal && key.starts_with(&internal_prefix) {
                    continue;
                }
                total = total
                    .checked_add(non_negative_size(object.size())?)
                    .ok_or_else(|| AppError::internal("S3 storage usage exceeds u64"))?;
            }
            if !output.is_truncated().unwrap_or(false) {
                return Ok(total);
            }
            let next = output.next_continuation_token().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储容量统计分页结果无效".into())
            })?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储容量统计分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        Err(AppError::ServiceUnavailable(
            "对象存储对象数量超过 Ycloud 容量统计安全上限".into(),
        ))
    }

    /// List direct children only. Pagination, result count and concurrent S3
    /// requests are bounded so a hostile or unexpectedly large bucket cannot
    /// monopolize a small VPS.
    pub async fn list_directory(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<S3ListResult> {
        let relative = StorageService::normalize_relative(relative)?;
        let directory_prefix = list_prefix(&self.prefix, &relative)?;
        let limit = max_entries.clamp(1, S3_MAX_LIST_ENTRIES);
        let target = limit.saturating_add(1);
        let _permit = self.acquire_request().await?;
        let mut continuation_token = None;
        let mut entries = BTreeMap::new();
        let mut remote_truncated = false;

        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = target.saturating_sub(entries.len());
            if remaining == 0 {
                break;
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 list page size"))?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&directory_prefix)
                .delimiter("/")
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory listing failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储目录".into())
            })?;

            collect_page_entries(
                &relative,
                &directory_prefix,
                output.common_prefixes(),
                output.contents(),
                &mut entries,
            )?;
            remote_truncated = output.is_truncated().unwrap_or(false);
            if !remote_truncated {
                break;
            }
            let Some(next) = output.next_continuation_token() else {
                return Err(AppError::ServiceUnavailable(
                    "对象存储返回了无效的分页结果".into(),
                ));
            };
            continuation_token = Some(next.to_owned());
        }

        let truncated = remote_truncated || entries.len() > limit;
        let entries = entries.into_values().take(limit).collect();
        Ok(S3ListResult { entries, truncated })
    }

    /// Resolve an object or an emulated directory prefix without allowing an
    /// ambiguous `name` object and `name/` directory to masquerade as one path.
    pub async fn metadata(&self, relative: &str) -> AppResult<S3Metadata> {
        let relative = StorageService::normalize_relative(relative)?;
        if relative.is_empty() {
            return Ok(directory_metadata(relative));
        }
        let key = object_key(&self.prefix, &relative)?;
        let directory_prefix = list_prefix(&self.prefix, &relative)?;
        let _permit = self.acquire_request().await?;

        let file = match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(output) => Some(S3Metadata {
                relative: relative.clone(),
                is_dir: false,
                size: non_negative_size(output.content_length())?,
                last_modified: output.last_modified().map(|value| value.secs()),
                content_type: output.content_type().map(str::to_owned),
                etag: output.e_tag().map(str::to_owned),
            }),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_not_found()) =>
            {
                None
            }
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 object metadata request failed"
                );
                return Err(AppError::ServiceUnavailable(
                    "无法读取对象存储元数据".into(),
                ));
            }
        };

        let directory_exists = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(directory_prefix)
            .max_keys(1)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory metadata request failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储元数据".into())
            })?
            .key_count()
            .unwrap_or(0)
            > 0;

        match (file, directory_exists) {
            (Some(_), true) => Err(AppError::Conflict(
                "对象存储中同一路径同时存在文件和目录前缀".into(),
            )),
            (Some(file), false) => Ok(file),
            (None, true) => Ok(directory_metadata(relative)),
            (None, false) => Err(AppError::NotFound),
        }
    }

    /// Stream one immutable view of an object. `If-Match` ties GET to the
    /// metadata used for Range validation, preventing a replacement between
    /// HEAD and GET from producing inconsistent length or range headers.
    pub async fn stream_file(
        &self,
        relative: &str,
        request_headers: &HeaderMap,
        mode: FileResponseMode,
    ) -> AppResult<Response<Body>> {
        let metadata = self.metadata(relative).await?;
        if metadata.is_dir {
            return Err(AppError::NotFound);
        }
        let range = parse_range(request_headers, metadata.size);
        let (start, length, status) = match range {
            Ok(Some(range)) => range,
            Ok(None) => (0, metadata.size, StatusCode::OK),
            Err(()) => return range_not_satisfiable(metadata.size),
        };
        let key = object_key(&self.prefix, &metadata.relative)?;
        let permit = self.acquire_request().await?;
        let mut request = self.client.get_object().bucket(&self.bucket).key(key);
        if let Some(etag) = metadata.etag.as_deref() {
            request = request.if_match(etag);
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            request = request.range(format!("bytes={start}-{end}"));
        }
        let output = request.send().await.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 object download failed"
            );
            AppError::ServiceUnavailable("对象已发生变化、无权读取或对象存储暂不可用".into())
        })?;
        let response_length = non_negative_size(output.content_length())?;
        if response_length != length {
            return Err(AppError::ServiceUnavailable(
                "对象存储返回了不一致的内容长度".into(),
            ));
        }

        let reader = output.body.into_async_read();
        let stream = PermitStream {
            inner: ReaderStream::new(reader),
            _permit: permit,
        };
        let mut response = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
        let guessed = mime_guess::from_path(&metadata.relative).first_or_octet_stream();
        let (mut content_type, force_attachment) = content_type_for_mode(&guessed, mode);
        if matches!(
            mode,
            FileResponseMode::Attachment | FileResponseMode::WebDav
        ) {
            if let Some(value) = metadata
                .content_type
                .as_deref()
                .and_then(|value| HeaderValue::from_str(value).ok())
            {
                content_type = value;
            }
        }
        response = response.header(header::CONTENT_TYPE, content_type);
        if matches!(mode, FileResponseMode::Attachment) || force_attachment {
            response = response.header(
                header::CONTENT_DISPOSITION,
                attachment_header(Path::new(&metadata.relative)),
            );
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            response = response.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", metadata.size),
            );
        }
        response
            .body(Body::from_stream(stream))
            .map_err(|error| AppError::with_source("failed to build S3 response", error))
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
        if content_length > max_upload_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        let relative = StorageService::normalize_relative(relative)?;
        let destination_key = object_key(&self.prefix, &relative)?;
        self.ensure_parent_directory(&relative).await?;
        let upload_id = uuid::Uuid::new_v4().simple().to_string();
        let temporary_key = internal_key(&self.prefix, "uploads", &upload_id);
        let backup_key = internal_key(&self.prefix, "backups", &upload_id);
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

        let permit = self.acquire_request().await?;
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&temporary_key)
            .content_length(length)
            .body(stream);
        if let Some(content_type) = sanitize_content_type(content_type)? {
            request = request.content_type(content_type);
        }
        let upload = request
            .customize()
            .config_override(operation_override)
            .send()
            .await;
        drop(permit);
        if let Err(error) = upload {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 temporary upload failed"
            );
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
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(marker)
            .content_length(0)
            .content_type("application/x-directory")
            .if_none_match("*")
            .body(ByteStream::from_static(&[]))
            .send()
            .await
            .map_err(|error| {
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
        let transaction_keys = self.list_transaction_keys().await?;
        let recovered = transaction_keys.len();
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
        if destination_must_not_exist {
            request = request.if_none_match("*");
        }
        let output = request.send().await.map_err(|error| {
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
        let _permit = self.acquire_request().await?;
        let mut request = self.client.delete_object().bucket(&self.bucket).key(key);
        if let Some(etag) = etag {
            request = request.if_match(etag);
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
        let data = serde_json::to_vec(transaction)
            .map_err(|error| AppError::with_source("failed to encode S3 transaction", error))?;
        if data.len() > S3_MAX_TRANSACTION_BYTES {
            return Err(AppError::ServiceUnavailable(
                "对象存储事务记录超过安全上限".into(),
            ));
        }
        let content_length = i64::try_from(data.len())
            .map_err(|_| AppError::ServiceUnavailable("对象存储事务记录过大".into()))?;
        let _permit = self.acquire_request().await?;
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_length(content_length)
            .content_type("application/json")
            .body(ByteStream::from(data));
        request = if let Some(etag) = previous_etag {
            request.if_match(etag)
        } else {
            request.if_none_match("*")
        };
        let output = request.send().await.map_err(|error| {
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

    async fn delete_transaction_best_effort(&self, key: &str, etag: Option<&str>) {
        if let Err(error) = self.delete_key(key, etag).await {
            tracing::warn!(%error, "failed to clean up an S3 transaction journal");
        }
    }

    async fn list_transaction_keys(&self) -> AppResult<Vec<String>> {
        let prefix = internal_key(&self.prefix, "transactions", "");
        let _permit = self.acquire_request().await?;
        let output = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .max_keys(i32::try_from(S3_MAX_PENDING_TRANSACTIONS).unwrap_or(i32::MAX))
            .send()
            .await
            .map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 transaction journal listing failed"
                );
                AppError::ServiceUnavailable("无法列举对象存储事务记录".into())
            })?;
        if output.is_truncated().unwrap_or(false) {
            return Err(AppError::ServiceUnavailable(
                "待恢复对象存储事务超过安全上限".into(),
            ));
        }
        let mut keys = Vec::new();
        for object in output.contents() {
            let key = object
                .key()
                .ok_or_else(|| AppError::ServiceUnavailable("对象存储事务记录缺少键名".into()))?;
            let id = key.strip_prefix(&prefix).ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储事务记录越出保留前缀".into())
            })?;
            if !valid_transaction_id(id) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储事务区包含无法识别的记录".into(),
                ));
            }
            keys.push(key.to_owned());
        }
        Ok(keys)
    }

    async fn read_upload_transaction(&self, key: &str) -> AppResult<(S3UploadTransaction, String)> {
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
                    "S3 transaction journal read failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储事务记录".into())
            })?;
        if output.content_length().is_some_and(|length| {
            length < 0
                || usize::try_from(length).map_or(true, |value| value > S3_MAX_TRANSACTION_BYTES)
        }) {
            return Err(AppError::ServiceUnavailable(
                "对象存储事务记录超过安全上限".into(),
            ));
        }
        let etag = output
            .e_tag()
            .map(str::to_owned)
            .ok_or_else(|| AppError::ServiceUnavailable("事务记录缺少 ETag".into()))?;
        let data = output.body.collect().await.map_err(|error| {
            AppError::with_source("failed to stream S3 transaction journal", error)
        })?;
        let data = data.into_bytes();
        if data.len() > S3_MAX_TRANSACTION_BYTES {
            return Err(AppError::ServiceUnavailable(
                "对象存储事务记录超过安全上限".into(),
            ));
        }
        let transaction: S3UploadTransaction = serde_json::from_slice(&data)
            .map_err(|error| AppError::with_source("invalid S3 transaction journal", error))?;
        validate_upload_transaction(&self.prefix, key, &transaction)?;
        Ok((transaction, etag))
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawS3Metadata {
    size: u64,
    etag: Option<String>,
}

fn collect_page_entries(
    parent: &str,
    directory_prefix: &str,
    common_prefixes: &[CommonPrefix],
    objects: &[Object],
    entries: &mut BTreeMap<String, S3Entry>,
) -> AppResult<()> {
    for prefix in common_prefixes {
        let Some(prefix) = prefix.prefix() else {
            continue;
        };
        let Some(name) = direct_child_name(directory_prefix, prefix, true) else {
            continue;
        };
        insert_safe_child(entries, parent, name, true, 0, None)?;
    }
    for object in objects {
        let Some(key) = object.key() else {
            continue;
        };
        if key == directory_prefix {
            continue;
        }
        let is_dir = key.ends_with('/');
        let Some(name) = direct_child_name(directory_prefix, key, is_dir) else {
            continue;
        };
        let size = if is_dir {
            0
        } else {
            non_negative_size(object.size())?
        };
        let modified = object.last_modified().map(|value| value.secs());
        insert_safe_child(entries, parent, name, is_dir, size, modified)?;
    }
    Ok(())
}

fn direct_child_name<'a>(directory_prefix: &str, key: &'a str, is_dir: bool) -> Option<&'a str> {
    let suffix = key.strip_prefix(directory_prefix)?;
    let suffix = if is_dir {
        suffix.strip_suffix('/')?
    } else {
        suffix
    };
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    Some(suffix)
}

fn parent_relative(relative: &str) -> &str {
    relative.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn child_entry(
    parent: &str,
    name: &str,
    is_dir: bool,
    size: u64,
    last_modified: Option<i64>,
) -> AppResult<S3Entry> {
    if matches!(name, "" | "." | "..") {
        return Err(AppError::BadRequest("Invalid storage path".into()));
    }
    let relative = if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    };
    let relative = StorageService::normalize_relative(&relative)?;
    Ok(S3Entry {
        name: name.to_owned(),
        relative,
        is_dir,
        size,
        last_modified,
    })
}

fn insert_safe_child(
    entries: &mut BTreeMap<String, S3Entry>,
    parent: &str,
    name: &str,
    is_dir: bool,
    size: u64,
    last_modified: Option<i64>,
) -> AppResult<()> {
    match child_entry(parent, name, is_dir, size, last_modified) {
        Ok(entry) => insert_entry(entries, entry),
        Err(AppError::BadRequest(_)) => {
            tracing::warn!("skipping invalid S3 object key below configured prefix");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn insert_entry(entries: &mut BTreeMap<String, S3Entry>, entry: S3Entry) -> AppResult<()> {
    if let Some(existing) = entries.get(&entry.name) {
        if existing.is_dir != entry.is_dir {
            return Err(AppError::Conflict(
                "对象存储中同一路径同时存在文件和目录前缀".into(),
            ));
        }
        return Ok(());
    }
    entries.insert(entry.name.clone(), entry);
    Ok(())
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

fn range_not_satisfiable(total_length: u64) -> AppResult<Response<Body>> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{total_length}"))
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .map_err(|error| AppError::with_source("failed to build S3 range response", error))
}

fn internal_key(prefix: &str, category: &str, id: &str) -> String {
    format!("{prefix}.ycloud-system/{category}/{id}")
}

fn valid_transaction_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
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

fn copy_source(bucket: &str, key: &str) -> String {
    format!("{bucket}/{}", percent_encode_s3_key(key))
}

fn percent_encode_s3_key(key: &str) -> String {
    let mut encoded = String::with_capacity(key.len());
    for byte in key.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

struct ExactLengthBody<S> {
    state: std::sync::Mutex<ExactLengthState<S>>,
}

struct ExactLengthState<S> {
    inner: Option<S>,
    remaining: u64,
}

impl<S> ExactLengthBody<S> {
    fn new(inner: S, expected: u64) -> Self {
        Self {
            state: std::sync::Mutex::new(ExactLengthState {
                inner: Some(inner),
                remaining: expected,
            }),
        }
    }
}

impl<S, E> http_body::Body for ExactLengthBody<S>
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + 'static,
{
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let Ok(mut state) = self.state.lock() else {
            return Poll::Ready(Some(Err(std::io::Error::other(
                "upload body state is unavailable",
            ))));
        };
        let Some(inner) = state.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(inner).poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                let Ok(length) = u64::try_from(chunk.len()) else {
                    state.inner = None;
                    return Poll::Ready(Some(Err(std::io::Error::other(
                        "upload chunk length overflow",
                    ))));
                };
                if length > state.remaining {
                    state.inner = None;
                    return Poll::Ready(Some(Err(std::io::Error::other(
                        "upload body exceeds Content-Length",
                    ))));
                }
                state.remaining -= length;
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Some(Err(error))) => {
                state.inner = None;
                Poll::Ready(Some(Err(std::io::Error::other(error.to_string()))))
            }
            Poll::Ready(None) if state.remaining == 0 => {
                state.inner = None;
                Poll::Ready(None)
            }
            Poll::Ready(None) => {
                state.inner = None;
                Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "upload body is shorter than Content-Length",
                ))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |state| state.inner.is_none())
    }

    fn size_hint(&self) -> SizeHint {
        let remaining = self.state.lock().map_or(0, |state| state.remaining);
        SizeHint::with_exact(remaining)
    }
}

struct PermitStream<S> {
    inner: S,
    _permit: OwnedSemaphorePermit,
}

impl<S> Stream for PermitStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(context)
    }
}

fn object_key(prefix: &str, relative: &str) -> AppResult<String> {
    let relative = StorageService::normalize_relative(relative)?;
    if relative.is_empty() {
        return Err(AppError::BadRequest("对象路径不能是存储根目录".into()));
    }
    Ok(format!("{prefix}{relative}"))
}

fn list_prefix(prefix: &str, relative: &str) -> AppResult<String> {
    let relative = StorageService::normalize_relative(relative)?;
    if relative.is_empty() {
        return Ok(prefix.to_owned());
    }
    Ok(format!("{prefix}{relative}/"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashSet},
        net::IpAddr,
        path::PathBuf,
    };

    use aws_sdk_s3::types::{CommonPrefix, Object};
    use axum::{body::Body, http::HeaderMap};
    use bytes::Bytes;
    use futures_util::stream;
    use http_body_util::BodyExt;

    use crate::{
        config::{normalize_s3_endpoint, Config, S3AddressingStyle, S3Provider, S3StorageConfig},
        error::{AppError, AppResult},
        storage::FileResponseMode,
    };

    use super::{
        collect_page_entries, copy_source, internal_key, list_prefix, object_key, parent_relative,
        snapshot_matches, valid_transaction_id, validate_upload_transaction, ExactLengthBody,
        RawS3Metadata, S3Backend, S3ObjectSnapshot, S3UploadStage, S3UploadTransaction,
    };

    fn required_smoke_env(name: &str) -> String {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("missing required smoke-test variable {name}"))
    }

    /// Opt-in compatibility test for an isolated RustFS or MinIO bucket.
    ///
    /// The test never runs in the normal suite and creates a unique Prefix for
    /// every execution. Credentials are read only from process environment
    /// variables and are never printed.
    #[tokio::test]
    #[ignore = "requires an isolated RustFS/MinIO test bucket and explicit credentials"]
    async fn rustfs_minio_s3_smoke() {
        let endpoint = required_smoke_env("YCLOUD_S3_SMOKE_ENDPOINT");
        let bucket = required_smoke_env("YCLOUD_S3_SMOKE_BUCKET");
        let access_key_id = required_smoke_env("YCLOUD_S3_SMOKE_ACCESS_KEY_ID");
        let secret_access_key = required_smoke_env("YCLOUD_S3_SMOKE_SECRET_ACCESS_KEY");
        let region = std::env::var("YCLOUD_S3_SMOKE_REGION").unwrap_or_else(|_| "us-east-1".into());
        let base_prefix =
            std::env::var("YCLOUD_S3_SMOKE_PREFIX").unwrap_or_else(|_| "ycloud-smoke/".into());
        let base_prefix = base_prefix.trim_matches('/');
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let prefix = if base_prefix.is_empty() {
            format!("{run_id}/")
        } else {
            format!("{base_prefix}/{run_id}/")
        };
        let settings = S3StorageConfig {
            provider: S3Provider::Minio,
            endpoint: endpoint.clone(),
            bucket,
            region,
            prefix,
            addressing_style: S3AddressingStyle::Path,
            access_key_id,
            secret_access_key,
            capacity_limit_bytes: Some(1024 * 1024),
        };
        let runtime = Config {
            bind_address: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            storage_path: PathBuf::from("unused-smoke-local-storage"),
            config_path: PathBuf::from("unused-smoke-config.json"),
            max_upload_bytes: 1024 * 1024,
            io_concurrency: 2,
            max_list_entries: 100,
            request_timeout_secs: 30,
            upload_timeout_secs: 30,
            disk_reserve_bytes: 0,
            secure_cookies: false,
            allow_lan_http: true,
            public_base_url: None,
            public_host: None,
            trusted_proxy_ips: HashSet::new(),
            s3_allowed_endpoints: HashSet::from([normalize_s3_endpoint(&endpoint).unwrap()]),
        };
        let backend = S3Backend::new(&settings, &runtime).unwrap();
        let payload = Bytes::from_static(b"ycloud-rustfs-compatibility-smoke");

        let result: AppResult<()> = async {
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
            Ok(())
        }
        .await;

        // Preserve transaction evidence on ambiguous failures, but clean all
        // ordinary test paths so successful and simple failed runs do not
        // contaminate later capacity scans.
        let _ = backend.recover_transactions().await;
        for path in ["suite-moved", "suite-copy", "suite"] {
            let _ = backend.delete_directory(path).await;
        }
        result.unwrap();
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
