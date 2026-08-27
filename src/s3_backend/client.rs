use std::{sync::Arc, time::Duration};

use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig, BehaviorVersion, Credentials, Region},
    Client,
};
use aws_smithy_http_client::Builder as HttpClientBuilder;
use aws_smithy_types::byte_stream::ByteStream;
use tokio::sync::{Mutex, Semaphore};

use super::{
    internal_key, S3Backend, S3_ATTEMPT_TIMEOUT, S3_CONNECT_TIMEOUT, S3_IDLE_CONNECTION_TIMEOUT,
    S3_MAX_ATTEMPTS, S3_MAX_IDLE_CONNECTIONS_PER_HOST, S3_OPERATION_TIMEOUT,
    S3_REQUEST_CONCURRENCY,
};
use crate::{
    config::{
        validate_storage_backend, Config, S3AddressingStyle, S3StorageConfig, StorageBackendConfig,
    },
    error::{AppError, AppResult},
};

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

        Ok(Self {
            client: Client::from_conf(sdk_config.build()),
            bucket: settings.bucket.clone(),
            prefix: settings.prefix.clone(),
            request_gate: Arc::new(Semaphore::new(S3_REQUEST_CONCURRENCY)),
            mutation_gate: Arc::new(Mutex::new(())),
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
}
