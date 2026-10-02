use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig},
    types::{CompletedMultipartUpload, CompletedPart},
};
use aws_smithy_types::byte_stream::ByteStream;
use axum::body::Body;

use super::{
    multipart_part_size, multipart_session_matches, S3Backend, S3MultipartAbortOutcome,
    S3MultipartPurpose, S3_CONNECT_TIMEOUT,
};
use crate::error::{AppError, AppResult, CleanupState, CommitState};

mod copy;
mod session;

impl S3Backend {
    pub(super) async fn multipart_upload(
        &self,
        key: &str,
        input: super::UploadInput,
        content_length: u64,
        content_type: Option<&str>,
        operation_id: &str,
    ) -> AppResult<()> {
        let started = self
            .start_multipart_operation(
                key,
                S3MultipartPurpose::Upload,
                content_length,
                content_type,
                Some(operation_id),
            )
            .await?;
        let upload_id = started.upload_id.as_str();
        let cancellation = input.cancellation.clone().unwrap_or_default();
        let transfer = async {
            if let Some(channel) = input.direct {
                self.receive_direct_parts(key, upload_id, content_length, channel)
                    .await
            } else {
                self.upload_multipart_parts(key, upload_id, input.body, content_length)
                    .await
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(AppError::Conflict("存储配置已变更，上传已中断，请重试".into())),
            result = transfer => result,
        };
        // The persisted session is enough for the recovery worker to abort
        // this exact upload. Do not make an administrator edit wait for a
        // remote HEAD/Abort round trip after payload transfer was cancelled.
        if cancellation.is_cancelled() {
            return Err(
                AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                    .with_operation(CommitState::NotCommitted, CleanupState::Pending),
            );
        }
        match result {
            Ok(()) => {
                self.delete_transaction_best_effort(
                    &started.journal_key,
                    started.journal_etag.as_deref(),
                )
                .await;
                Ok(())
            }
            Err(error) => match self.head_key(key).await {
                Ok(Some(metadata)) if multipart_session_matches(&metadata, &started.session) => {
                    self.delete_transaction_best_effort(
                        &started.journal_key,
                        started.journal_etag.as_deref(),
                    )
                    .await;
                    Ok(())
                }
                Ok(object) => match self.abort_multipart_operation(key, upload_id).await {
                    Ok(S3MultipartAbortOutcome::Missing) if object.is_some() => {
                        Err(error.with_operation(CommitState::Unknown, CleanupState::Unknown))
                    }
                    Ok(S3MultipartAbortOutcome::Aborted | S3MultipartAbortOutcome::Missing) => {
                        self.delete_transaction_best_effort(
                            &started.journal_key,
                            started.journal_etag.as_deref(),
                        )
                        .await;
                        Err(error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
                    }
                    Err(_) => {
                        Err(error.with_operation(CommitState::Unknown, CleanupState::Pending))
                    }
                },
                Err(_) => Err(error.with_operation(CommitState::Unknown, CleanupState::Unknown)),
            },
        }
    }

    async fn upload_multipart_parts(
        &self,
        key: &str,
        upload_id: &str,
        body: Body,
        content_length: u64,
    ) -> AppResult<()> {
        let part_size = multipart_part_size(content_length)?;
        let source = super::relay::RelaySource::new(body);
        let mut completed = Vec::new();
        let mut offset = 0;
        while offset < content_length {
            let length = (content_length - offset).min(part_size as u64);
            let body = super::relay::RelaySource::part(source.clone(), length);
            completed.push(
                self.upload_part(key, upload_id, completed.len() + 1, body, length)
                    .await?,
            );
            offset += length;
        }
        source.lock().await.finish().await?;
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
        body: Body,
        size: u64,
    ) -> AppResult<CompletedPart> {
        let part_number = i32::try_from(number).map_err(|_| AppError::PayloadTooLarge)?;
        let length = i64::try_from(size).map_err(|_| AppError::PayloadTooLarge)?;
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
            .body(ByteStream::from_body_1_x(super::ExactLengthBody::new(
                body.into_data_stream(),
                size,
            )))
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
}
