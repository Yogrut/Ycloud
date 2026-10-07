//! Remote I/O for authenticated directory manifests; the wire format is unchanged.

use super::record::{sign_transaction, validate_transaction, Transaction};
use crate::{
    error::{AppError, AppResult},
    s3_backend::S3Backend,
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
        self.write_encoded_journal(key, data, previous_etag).await
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
