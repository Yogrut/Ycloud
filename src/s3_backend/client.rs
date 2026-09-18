use std::{
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use aws_sdk_s3::{
    config::{
        retry::RetryConfig, timeout::TimeoutConfig, BehaviorVersion, Credentials, Region,
        RequestChecksumCalculation, ResponseChecksumValidation,
    },
    Client,
};
use aws_smithy_http_client::Builder as HttpClientBuilder;
use aws_smithy_types::byte_stream::ByteStream;
use axum::http::HeaderValue;
use tokio::sync::{Mutex, RwLock, Semaphore};

use super::{
    capabilities, internal_key, S3Backend, S3_ATTEMPT_TIMEOUT, S3_CONNECT_TIMEOUT,
    S3_IDLE_CONNECTION_TIMEOUT, S3_MAX_ATTEMPTS, S3_MAX_IDLE_CONNECTIONS_PER_HOST,
    S3_OPERATION_METADATA_KEY, S3_OPERATION_TIMEOUT, S3_REQUEST_CONCURRENCY,
};
use crate::{
    config::{
        validate_storage_backend, Config, S3AddressingStyle, S3Provider, S3StorageConfig,
        StorageBackendConfig,
    },
    error::{AppError, AppResult},
};

impl S3Backend {
    pub fn new(settings: &S3StorageConfig, runtime: &Config) -> AppResult<Self> {
        if runtime.transaction_auth_key == [0; 32] {
            return Err(AppError::ServiceUnavailable(
                "对象存储事务认证密钥尚未初始化".into(),
            ));
        }
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
            // The AWS SDK enables optional S3 checksum trailers by default.
            // Third-party S3 implementations do not consistently support the
            // aws-chunked trailer framing, so request checksums are used only
            // for operations whose protocol model requires them. Ycloud still
            // verifies Content-Length, ETag and downloaded bytes itself.
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
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

        Ok(Self {
            client: Client::from_conf(sdk_config.build()),
            provider: settings.provider,
            bucket: settings.bucket.clone(),
            prefix: settings.prefix.clone(),
            request_gate: Arc::new(Semaphore::new(S3_REQUEST_CONCURRENCY)),
            recovery_gate: Arc::new(RwLock::new(())),
            mutation_gate: Arc::new(Mutex::new(())),
            upload_timeout: Duration::from_secs(runtime.upload_timeout_secs),
            orphan_uploads: Arc::new(AtomicUsize::new(0)),
            orphan_backups: Arc::new(AtomicUsize::new(0)),
            transaction_auth_key: runtime.transaction_auth_key,
            recovery_runtime: Arc::new(super::recovery_runtime::RecoveryRuntime::new()),
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
    /// is journaled before either test object can be created.
    pub async fn activation_probe(&self) -> AppResult<()> {
        self.probe().await.map_err(|error| {
            capability_failure(
                capabilities::PREFIX_LIST,
                "无法连接对象存储或当前凭据缺少列举权限",
                error,
            )
        })?;
        let _mutation = self.mutation_gate.lock().await;
        let recovered = self
            .recover_activation_probe_intents()
            .await
            .map_err(|error| {
                capability_failure(
                    capabilities::CONFIRMED_DELETE,
                    "对象存储无法确认遗留能力探针已经清理",
                    error,
                )
            })?;
        if recovered > 0 {
            tracing::warn!(recovered, "recovered pending S3 activation probe resources");
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let source_key = internal_key(&self.prefix, "activation-tests", &id);
        let copy_key = internal_key(&self.prefix, "activation-tests", &format!("{id}-copy"));
        let payload = format!("ycloud-storage-activation:{id}").into_bytes();
        let (journal_key, intent, journal_etag) = self
            .create_activation_probe_intent(&id, payload.len() as u64)
            .await
            .map_err(|error| {
                capability_failure(
                    capabilities::CONDITIONAL_JOURNAL,
                    "对象存储无法安全建立能力探针责任记录",
                    error,
                )
            })?;
        let result = self
            .activation_probe_inner(&id, &source_key, &copy_key, &payload)
            .await;
        let cleanup = self
            .settle_activation_probe_intent(&journal_key, journal_etag.as_deref(), &intent)
            .await;
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(capability_failure(
                capabilities::CONFIRMED_DELETE,
                "对象存储无法确认能力探针资源已经清理",
                error,
            )),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup_error)) => {
                tracing::warn!(%cleanup_error, "S3 activation probe cleanup remains pending");
                Err(error)
            }
        }
    }

    async fn activation_probe_inner(
        &self,
        operation_id: &str,
        source_key: &str,
        copy_key: &str,
        payload: &[u8],
    ) -> AppResult<()> {
        let length = i64::try_from(payload.len()).map_err(|_| {
            AppError::storage_capability(
                capabilities::CONDITIONAL_CREATE,
                "对象存储激活探测数据无效",
            )
        })?;
        let _permit = self.acquire_request().await?;
        let request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(source_key)
            .content_length(length)
            .metadata(S3_OPERATION_METADATA_KEY, operation_id)
            .body(ByteStream::from(payload.to_vec()));
        let result = if self.provider == S3Provider::AlibabaOss {
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
        let output = result.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 activation write probe failed"
            );
            AppError::storage_capability(
                capabilities::CONDITIONAL_CREATE,
                "对象存储缺少安全写入权限或条件写入能力",
            )
        })?;
        let source_etag = output.e_tag().map(str::to_owned).ok_or_else(|| {
            AppError::storage_capability(
                capabilities::CONDITIONAL_CREATE,
                "对象存储写入未返回 ETag",
            )
        })?;
        drop(_permit);

        let source = self
            .head_key(source_key)
            .await
            .map_err(|error| {
                capability_failure(
                    capabilities::HEAD_METADATA,
                    "对象存储无法读取写入探测对象的元数据",
                    error,
                )
            })?
            .ok_or_else(|| {
                AppError::storage_capability(
                    capabilities::HEAD_METADATA,
                    "对象存储写入探测对象不可见",
                )
            })?;
        if source.size != payload.len() as u64
            || source.etag.as_deref() != Some(&source_etag)
            || source.operation_id.as_deref() != Some(operation_id)
        {
            return Err(AppError::storage_capability(
                capabilities::HEAD_METADATA,
                "对象存储写入后的长度或版本校验失败",
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
            .map_err(|error| {
                capability_failure(
                    capabilities::CONDITIONAL_READ,
                    "对象存储读取校验失败",
                    AppError::with_source("S3 conditional read failed", error),
                )
            })?
            .body
            .collect()
            .await
            .map_err(|error| {
                capability_failure(
                    capabilities::CONDITIONAL_READ,
                    "对象存储读取响应不完整",
                    AppError::with_source("S3 conditional read body failed", error),
                )
            })?
            .into_bytes();
        drop(_permit);
        if downloaded.as_ref() != payload {
            return Err(AppError::storage_capability(
                capabilities::CONDITIONAL_READ,
                "对象存储读取内容校验失败",
            ));
        }

        let copied_etag = self
            .copy_key(source_key, copy_key, Some(&source_etag), true)
            .await
            .map_err(|error| {
                capability_failure(
                    capabilities::SERVER_SIDE_COPY,
                    "对象存储缺少安全服务端复制能力",
                    error,
                )
            })?;
        let copied = self
            .head_key(copy_key)
            .await
            .map_err(|error| {
                capability_failure(
                    capabilities::SERVER_SIDE_COPY,
                    "对象存储无法读取复制探测对象的元数据",
                    error,
                )
            })?
            .ok_or_else(|| {
                AppError::storage_capability(
                    capabilities::SERVER_SIDE_COPY,
                    "对象存储复制探测对象不可见",
                )
            })?;
        if copied.size != payload.len() as u64
            || copied.etag.as_deref() != Some(&copied_etag)
            || copied.operation_id.as_deref() != Some(operation_id)
        {
            return Err(AppError::storage_capability(
                capabilities::SERVER_SIDE_COPY,
                "对象存储服务端复制校验失败",
            ));
        }
        Ok(())
    }
}

fn capability_failure(
    capability: &'static str,
    public_message: &'static str,
    error: AppError,
) -> AppError {
    tracing::warn!(%error, capability, "S3 capability check failed");
    AppError::storage_capability(capability, public_message)
}
