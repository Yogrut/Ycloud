use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig},
    types::{CompletedMultipartUpload, CompletedPart},
};
use aws_smithy_types::{byte_stream::ByteStream, error::metadata::ProvideErrorMetadata};
use axum::body::Body;
use bytes::BytesMut;
use futures_util::StreamExt;

use super::{
    capabilities, copy_source, multipart_part_size, multipart_session_matches, S3Backend,
    S3MultipartAbortOutcome, S3MultipartCopyOptions, S3MultipartPurpose, S3_CONNECT_TIMEOUT,
    S3_OPERATION_METADATA_KEY,
};
use crate::error::{AppError, AppResult, CleanupState, CommitState};

impl S3Backend {
    pub(super) async fn multipart_upload(
        &self,
        key: &str,
        body: Body,
        content_length: u64,
        content_type: Option<&str>,
        operation_id: &str,
    ) -> AppResult<()> {
        let (session_key, mut session, mut session_etag) = self
            .register_multipart_intent(
                key,
                S3MultipartPurpose::Upload,
                content_length,
                Some(operation_id),
            )
            .await?;
        let mut create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key);
        if let Some(content_type) = content_type {
            create = create.content_type(content_type);
        }
        create = create.metadata(S3_OPERATION_METADATA_KEY, &session.id);
        let upload_id = {
            let _permit = self.acquire_request().await?;
            create
                .send()
                .await
                .map_err(|_| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_CREATE,
                        "无法创建对象存储分片上传",
                    )
                })?
                .upload_id()
                .map(str::to_owned)
                .ok_or_else(|| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_CREATE,
                        "对象存储未返回分片上传 ID",
                    )
                })?
        };
        session.upload_id = Some(upload_id.clone());
        session_etag = match self
            .write_multipart_session(&session_key, &session, session_etag.as_deref())
            .await
        {
            Ok(etag) => etag,
            Err(error) => {
                if self
                    .abort_multipart_operation(key, &upload_id)
                    .await
                    .is_ok()
                {
                    self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                        .await;
                }
                return Err(error);
            }
        };
        let result = self
            .upload_multipart_parts(key, &upload_id, body, content_length)
            .await;
        match result {
            Ok(()) => {
                self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                    .await;
                Ok(())
            }
            Err(error) => match self.head_key(key).await {
                Ok(Some(metadata)) if multipart_session_matches(&metadata, &session) => {
                    self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                        .await;
                    Ok(())
                }
                Ok(object) => match self.abort_multipart_operation(key, &upload_id).await {
                    Ok(S3MultipartAbortOutcome::Aborted) => {
                        self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                            .await;
                        Err(error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
                    }
                    Ok(S3MultipartAbortOutcome::Missing) if object.is_none() => {
                        self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                            .await;
                        Err(error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
                    }
                    Ok(S3MultipartAbortOutcome::Missing) => {
                        Err(error.with_operation(CommitState::Unknown, CleanupState::Unknown))
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

    pub(super) async fn multipart_copy(
        &self,
        source_key: &str,
        destination_key: &str,
        source_size: u64,
        options: S3MultipartCopyOptions<'_>,
    ) -> AppResult<String> {
        if options.destination_must_not_exist && self.head_key(destination_key).await?.is_some() {
            return Err(AppError::Conflict("目标对象已存在".into()));
        }
        let (session_key, mut session, mut session_etag) = self
            .register_multipart_intent(
                destination_key,
                S3MultipartPurpose::Copy,
                source_size,
                options.operation_id,
            )
            .await?;
        let upload_id = {
            let _permit = self.acquire_request().await?;
            let mut create = self
                .client
                .create_multipart_upload()
                .bucket(&self.bucket)
                .key(destination_key)
                .metadata(S3_OPERATION_METADATA_KEY, &session.id);
            if let Some(content_type) = options.content_type {
                create = create.content_type(content_type);
            }
            create
                .send()
                .await
                .map_err(|_| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_CREATE,
                        "无法创建对象存储分片复制",
                    )
                })?
                .upload_id()
                .map(str::to_owned)
                .ok_or_else(|| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_CREATE,
                        "对象存储未返回分片复制 ID",
                    )
                })?
        };
        session.upload_id = Some(upload_id.clone());
        session_etag = match self
            .write_multipart_session(&session_key, &session, session_etag.as_deref())
            .await
        {
            Ok(etag) => etag,
            Err(error) => {
                if self
                    .abort_multipart_operation(destination_key, &upload_id)
                    .await
                    .is_ok()
                {
                    self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                        .await;
                }
                return Err(error);
            }
        };
        let completed = async {
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
                if let Some(etag) = options.source_etag {
                    request = request.copy_source_if_match(etag);
                }
                let output = request.send().await.map_err(|_| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_PART_COPY,
                        "对象存储分片复制失败",
                    )
                })?;
                let etag = output
                    .copy_part_result()
                    .and_then(|result| result.e_tag())
                    .ok_or_else(|| {
                        AppError::storage_capability(
                            capabilities::MULTIPART_PART_COPY,
                            "对象存储复制分片未返回 ETag",
                        )
                    })?;
                completed.push(
                    CompletedPart::builder()
                        .part_number(part_number)
                        .e_tag(etag)
                        .build(),
                );
                start = end + 1;
            }
            Ok::<_, AppError>(completed)
        }
        .await;
        let completed = match completed {
            Ok(completed) => completed,
            Err(error) => {
                return match self
                    .abort_multipart_operation(destination_key, &upload_id)
                    .await
                {
                    Ok(_) => {
                        self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                            .await;
                        Err(error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
                    }
                    Err(_) => {
                        Err(error.with_operation(CommitState::NotCommitted, CleanupState::Pending))
                    }
                };
            }
        };
        let multipart = CompletedMultipartUpload::builder()
            .set_parts(Some(completed))
            .build();
        let completion = {
            let _permit = self.acquire_request().await?;
            self.client
                .complete_multipart_upload()
                .bucket(&self.bucket)
                .key(destination_key)
                .upload_id(&upload_id)
                .multipart_upload(multipart)
                .send()
                .await
        };
        let (error, completion_confirmed) = match completion {
            Ok(output) => {
                if let Some(etag) = output.e_tag() {
                    self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                        .await;
                    return Ok(etag.to_owned());
                }
                (
                    AppError::ServiceUnavailable("对象存储复制未返回提交 ETag".into()),
                    true,
                )
            }
            Err(_) => (
                AppError::ServiceUnavailable("对象存储分片复制提交失败".into()),
                false,
            ),
        };
        match self.head_key(destination_key).await {
            Ok(Some(metadata)) if multipart_session_matches(&metadata, &session) => {
                self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                    .await;
                Ok(metadata
                    .etag
                    .expect("multipart session match requires an ETag"))
            }
            Ok(_) if completion_confirmed => {
                Err(error.with_operation(CommitState::Committed, CleanupState::Pending))
            }
            Ok(_) => match self
                .abort_multipart_operation(destination_key, &upload_id)
                .await
            {
                Ok(S3MultipartAbortOutcome::Aborted) => {
                    self.delete_transaction_best_effort(&session_key, session_etag.as_deref())
                        .await;
                    Err(error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
                }
                Ok(S3MultipartAbortOutcome::Missing) => {
                    Err(error.with_operation(CommitState::Unknown, CleanupState::Pending))
                }
                Err(_) => Err(error.with_operation(CommitState::Unknown, CleanupState::Pending)),
            },
            Err(_) => Err(error.with_operation(CommitState::Unknown, CleanupState::Unknown)),
        }
    }

    pub(super) async fn abort_multipart_operation(
        &self,
        key: &str,
        upload_id: &str,
    ) -> AppResult<S3MultipartAbortOutcome> {
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
            Ok(_) => Ok(S3MultipartAbortOutcome::Aborted),
            Err(error)
                if error
                    .as_service_error()
                    .and_then(ProvideErrorMetadata::code)
                    == Some("NoSuchUpload") =>
            {
                Ok(S3MultipartAbortOutcome::Missing)
            }
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 multipart abort failed"
                );
                Err(AppError::storage_capability(
                    capabilities::MULTIPART_ABORT,
                    "对象存储分片会话暂时无法终止",
                ))
            }
        }
    }
}
