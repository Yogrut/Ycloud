//! Remote I/O for authenticated directory manifests; the wire format is unchanged.

use aws_smithy_types::byte_stream::ByteStream;
use axum::http::HeaderValue;

use super::record::{sign_transaction, validate_transaction, Transaction};
use crate::{
    error::{AppError, AppResult},
    s3_backend::{capabilities, S3Backend},
};

const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;

impl S3Backend {
    pub(super) async fn write_directory_transaction(
        &self,
        key: &str,
        transaction: &mut Transaction,
        previous_etag: Option<&str>,
    ) -> AppResult<String> {
        sign_transaction(&self.transaction_auth_key, transaction)?;
        validate_transaction(&self.transaction_auth_key, &self.prefix, key, transaction)?;
        let data = serde_json::to_vec(transaction).map_err(|error| {
            AppError::with_source("failed to encode S3 directory transaction", error)
        })?;
        if data.len() > MAX_JOURNAL_BYTES {
            return Err(AppError::Conflict(
                "目录事务清单超过固定安全上限；请拆分目录后重试".into(),
            ));
        }
        let content_length = i64::try_from(data.len())
            .map_err(|_| AppError::ServiceUnavailable("对象存储目录事务记录过大".into()))?;
        if self.is_alibaba_oss() {
            if let Some(expected_etag) = previous_etag {
                let current = self.head_key(key).await.map_err(|_| {
                    AppError::storage_capability(
                        capabilities::CONDITIONAL_JOURNAL_UPDATE,
                        "对象存储无法核对目录事务记录版本",
                    )
                })?;
                if current
                    .as_ref()
                    .and_then(|metadata| metadata.etag.as_deref())
                    != Some(expected_etag)
                {
                    return Err(AppError::storage_capability(
                        capabilities::CONDITIONAL_JOURNAL_UPDATE,
                        "对象存储目录事务记录在更新前发生变化",
                    ));
                }
            }
        }
        self.recovery_runtime.journal_write_started(key);
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
        let capability = if previous_etag.is_some() {
            capabilities::CONDITIONAL_JOURNAL_UPDATE
        } else {
            capabilities::CONDITIONAL_JOURNAL
        };
        result
            .map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory transaction journal write failed"
                );
                AppError::storage_capability(capability, "无法持久化对象存储目录事务状态")
            })?
            .e_tag()
            .map(str::to_owned)
            .ok_or_else(|| {
                AppError::storage_capability(capability, "对象存储未返回目录事务记录 ETag")
            })
    }

    pub(super) async fn read_directory_transaction(
        &self,
        key: &str,
    ) -> AppResult<(Transaction, String)> {
        self.maintenance
            .read(self.read_directory_transaction_uninterrupted(key))
            .await
    }

    async fn read_directory_transaction_uninterrupted(
        &self,
        key: &str,
    ) -> AppResult<(Transaction, String)> {
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
                    "S3 directory transaction journal read failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储目录事务记录".into())
            })?;
        let (data, etag) =
            crate::s3_backend::journal::read_journal_payload(output, MAX_JOURNAL_BYTES).await?;
        let transaction: Transaction = serde_json::from_slice(&data)
            .map_err(|error| AppError::with_source("invalid S3 directory transaction", error))?;
        validate_transaction(&self.transaction_auth_key, &self.prefix, key, &transaction)?;
        Ok((transaction, etag))
    }
}
