//! Server-side multipart copy. Its commit evidence differs from an upload.
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};

use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    s3_backend::{
        capabilities, copy_source, multipart_part_size, multipart_session_matches, S3Backend,
        S3MultipartAbortOutcome, S3MultipartCopyOptions, S3MultipartPurpose,
    },
};

const COPY_PART_BYTES: usize = 64 * 1024 * 1024;

impl S3Backend {
    pub(in crate::s3_backend) async fn multipart_copy(
        &self,
        source_key: &str,
        destination_key: &str,
        source_size: u64,
        options: S3MultipartCopyOptions<'_>,
    ) -> AppResult<String> {
        if options.destination_must_not_exist && self.head_key(destination_key).await?.is_some() {
            return Err(AppError::Conflict("目标对象已存在".into()));
        }
        let started = self
            .start_multipart_operation(
                destination_key,
                S3MultipartPurpose::Copy,
                source_size,
                options.content_type,
                options.operation_id,
            )
            .await?;
        let upload_id = started.upload_id.as_str();
        let completed = async {
            let part_size = multipart_part_size(source_size)?.max(COPY_PART_BYTES) as u64;
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
                    .upload_id(upload_id)
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
                    .abort_multipart_operation(destination_key, upload_id)
                    .await
                {
                    Ok(_) => {
                        self.delete_transaction_best_effort(
                            &started.journal_key,
                            started.journal_etag.as_deref(),
                        )
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
                .upload_id(upload_id)
                .multipart_upload(multipart)
                .send()
                .await
        };
        let (error, completion_confirmed) = match completion {
            Ok(output) => {
                if let Some(etag) = output.e_tag() {
                    self.delete_transaction_best_effort(
                        &started.journal_key,
                        started.journal_etag.as_deref(),
                    )
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
            Ok(Some(metadata)) if multipart_session_matches(&metadata, &started.session) => {
                self.delete_transaction_best_effort(
                    &started.journal_key,
                    started.journal_etag.as_deref(),
                )
                .await;
                Ok(metadata
                    .etag
                    .expect("multipart session match requires an ETag"))
            }
            Ok(_) if completion_confirmed => {
                Err(error.with_operation(CommitState::Committed, CleanupState::Pending))
            }
            Ok(_) => match self
                .abort_multipart_operation(destination_key, upload_id)
                .await
            {
                Ok(S3MultipartAbortOutcome::Aborted) => {
                    self.delete_transaction_best_effort(
                        &started.journal_key,
                        started.journal_etag.as_deref(),
                    )
                    .await;
                    Err(error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
                }
                Ok(S3MultipartAbortOutcome::Missing) | Err(_) => {
                    Err(error.with_operation(CommitState::Unknown, CleanupState::Pending))
                }
            },
            Err(_) => Err(error.with_operation(CommitState::Unknown, CleanupState::Unknown)),
        }
    }
}
