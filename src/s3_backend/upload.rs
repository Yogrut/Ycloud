use aws_sdk_s3::config::{retry::RetryConfig, timeout::TimeoutConfig};
use aws_smithy_types::byte_stream::ByteStream;
use axum::body::Body;

use super::{
    internal_key, object_key, sanitize_content_type, ExactLengthBody, RawS3Metadata, S3Backend,
    S3ObjectSnapshot, S3UploadResult, S3UploadStage, S3UploadTransaction, S3_CONNECT_TIMEOUT,
    S3_MULTIPART_MAX_PARTS, S3_MULTIPART_MAX_PART_BYTES, S3_MULTIPART_THRESHOLD,
    S3_OPERATION_METADATA_KEY, S3_TRANSACTION_SCHEMA_VERSION,
};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

impl S3Backend {
    pub(crate) fn validate_write_conditions(
        &self,
        conditions: &crate::storage::WriteConditions,
        content_length: u64,
    ) -> AppResult<()> {
        if conditions.is_conditional() && content_length > super::S3_SINGLE_COPY_LIMIT {
            return Err(AppError::storage_capability(
                super::capabilities::CONDITIONAL_FILE_PUBLISH,
                "S3 条件上传目前限于单对象原子复制范围（最多 4 GiB），已拒绝分片条件发布",
            ));
        }
        if conditions.is_conditional() && self.is_alibaba_oss() {
            return Err(AppError::storage_capability(
                "conditional_write",
                "当前 OSS 写入路径不能原子核验这些条件，已拒绝上传",
            ));
        }
        if conditions.has_date() {
            return Err(AppError::storage_capability(
                "conditional_write",
                "S3 正式发布无法原子核验目标修改时间，请使用 If-Match 版本标签",
            ));
        }
        Ok(())
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
        self.upload_file_mode(
            relative,
            super::UploadInput::relay(body),
            content_length,
            max_upload_bytes,
            content_type,
            false,
        )
        .await
    }

    pub(crate) async fn upload_file_mode(
        &self,
        relative: &str,
        input: super::UploadInput,
        content_length: u64,
        max_upload_bytes: u64,
        content_type: Option<&str>,
        create_only: bool,
    ) -> AppResult<S3UploadResult> {
        let backend = self.scoped_work(None, None);
        Box::pin(backend.upload_file_scoped(
            relative,
            input,
            content_length,
            max_upload_bytes,
            content_type,
            create_only,
        ))
        .await
    }

    async fn upload_file_scoped(
        &self,
        relative: &str,
        mut input: super::UploadInput,
        content_length: u64,
        max_upload_bytes: u64,
        content_type: Option<&str>,
        create_only: bool,
    ) -> AppResult<S3UploadResult> {
        self.validate_write_conditions(&input.conditions, content_length)?;
        let conditions = input.conditions.clone();
        if content_length > max_upload_bytes
            || content_length > S3_MULTIPART_MAX_PART_BYTES * S3_MULTIPART_MAX_PARTS
        {
            return Err(AppError::PayloadTooLarge);
        }
        if input.direct.is_none() {
            input.body = crate::relay_budget::wrap(input.body);
        }
        // Upload streams may run concurrently before their short commit turn
        // on mutation_gate. Recovery takes this gate exclusively, so it can
        // never adopt a journal or temporary object still owned by a live
        // upload future. Cancellation releases the guard and makes the
        // already-tracked journal eligible for the worker.
        let cancellation = input.cancellation.clone().unwrap_or_default();
        let _recovery_owner = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                .with_operation(CommitState::NotCommitted, CleanupState::Complete)),
            guard = self.recovery_gate.read() => guard,
        };
        let relative = StorageService::normalize_relative(relative)?;
        let destination_key = object_key(&self.prefix, &relative)?;
        self.ensure_parent_directory(&relative).await?;
        let upload_id = input
            .operation_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let temporary_key = internal_key(&self.prefix, "uploads", &upload_id);
        let backup_key = internal_key(&self.prefix, "backups", &upload_id);
        let content_type = sanitize_content_type(content_type)?;
        let (intent_key, intent, intent_etag) = self
            .create_internal_upload_intent(&upload_id, content_length)
            .await?;
        let commit_owner = input.commit_owner.clone();
        let interrupted = input.cancellation.clone();
        let upload = if content_length >= S3_MULTIPART_THRESHOLD || input.direct.is_some() {
            self.multipart_upload(
                &temporary_key,
                input,
                content_length,
                content_type.as_deref(),
                &upload_id,
            )
            .await
        } else {
            let cancellation = input.cancellation.clone().unwrap_or_default();
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(AppError::Conflict("存储配置已变更，上传已中断，请重试".into())),
                result = self.single_upload(
                &temporary_key,
                input.body,
                content_length,
                content_type.as_deref(),
                &upload_id,
            ) => result,
            }
        };
        if let Err(error) = upload {
            tracing::warn!(%error, "S3 temporary upload failed");
            if interrupted
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
            {
                // No formal copy has started. The authenticated intent/session
                // remains durable; cleanup belongs to recovery, not this
                // cancelled transfer's administrator-edit barrier.
                return Err(
                    AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                        .with_operation(CommitState::NotCommitted, CleanupState::Pending),
                );
            }
            return Err(self
                .finish_uncommitted_internal_upload(
                    &intent_key,
                    intent_etag.as_deref(),
                    &intent,
                    if interrupted
                        .as_ref()
                        .is_some_and(|token| token.is_cancelled())
                    {
                        AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                    } else {
                        AppError::ServiceUnavailable("对象存储上传失败或请求体长度不一致".into())
                    },
                )
                .await);
        }

        let cancellation = interrupted.clone().unwrap_or_default();
        let cancelled = || {
            AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                .with_operation(CommitState::NotCommitted, CleanupState::Pending)
        };
        let temporary_probe = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(cancelled()),
            result = self.head_key(&temporary_key) => result,
        };
        let temporary = match temporary_probe
            .and_then(|metadata| validate_temporary_upload(metadata, content_length))
        {
            Ok(metadata) => metadata,
            Err(error) => {
                return Err(self
                    .finish_uncommitted_internal_upload(
                        &intent_key,
                        intent_etag.as_deref(),
                        &intent,
                        error,
                    )
                    .await);
            }
        };

        if let Some(owner) = commit_owner {
            let capacity = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(cancelled()),
                guard = self.acquire_capacity_mutation() => guard,
            };
            *owner.lock().await = Some(capacity);
        }
        let _mutation = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(cancelled()),
            guard = self.mutation_gate.lock() => guard,
        };
        if interrupted
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(
                AppError::Conflict("存储配置已变更，上传已中断，请重试".into())
                    .with_operation(CommitState::NotCommitted, CleanupState::Pending),
            );
        }
        if let Err(error) = self.ensure_parent_directory(&relative).await {
            return Err(self
                .finish_uncommitted_internal_upload(
                    &intent_key,
                    intent_etag.as_deref(),
                    &intent,
                    error,
                )
                .await);
        }
        let existing = match self.metadata(&relative).await {
            Ok(metadata) if metadata.is_dir => {
                return Err(self
                    .finish_uncommitted_internal_upload(
                        &intent_key,
                        intent_etag.as_deref(),
                        &intent,
                        AppError::Conflict("不能用文件覆盖目录".into()),
                    )
                    .await);
            }
            Ok(metadata) => Some(metadata),
            Err(AppError::NotFound) => None,
            Err(error) => {
                return Err(self
                    .finish_uncommitted_internal_upload(
                        &intent_key,
                        intent_etag.as_deref(),
                        &intent,
                        error,
                    )
                    .await);
            }
        };
        if let Err(error) = conditions.check(
            existing.is_some(),
            existing.as_ref().and_then(|value| value.etag.as_deref()),
            None,
        ) {
            return Err(self
                .finish_uncommitted_internal_upload(
                    &intent_key,
                    intent_etag.as_deref(),
                    &intent,
                    error,
                )
                .await);
        }
        if create_only && existing.is_some() {
            return Err(self
                .finish_uncommitted_internal_upload(
                    &intent_key,
                    intent_etag.as_deref(),
                    &intent,
                    AppError::Conflict("上传目标已被占用，不能覆盖已有文件".into()),
                )
                .await);
        }
        if temporary.etag.is_none()
            || existing
                .as_ref()
                .is_some_and(|metadata| metadata.etag.is_none())
        {
            return Err(self
                .finish_uncommitted_internal_upload(
                    &intent_key,
                    intent_etag.as_deref(),
                    &intent,
                    AppError::ServiceUnavailable(
                        "对象存储未返回数据对象 ETag，无法安全提交".into(),
                    ),
                )
                .await);
        }

        let journal_key = internal_key(&self.prefix, "transactions", &upload_id);
        let publication_guard = if conditions.is_conditional() {
            Some(match existing.as_ref() {
                None => super::S3UploadPublicationGuard::Absent,
                Some(value) => super::S3UploadPublicationGuard::Matches {
                    etag: value
                        .etag
                        .clone()
                        .expect("data ETags checked before publication"),
                },
            })
        } else {
            None
        };
        let mut transaction = S3UploadTransaction {
            schema_version: if publication_guard.is_some() {
                super::transaction_record::S3_CONDITIONAL_UPLOAD_SCHEMA_VERSION
            } else {
                S3_TRANSACTION_SCHEMA_VERSION
            },
            id: upload_id.clone(),
            relative: relative.clone(),
            stage: if publication_guard.is_some() {
                S3UploadStage::CheckingPublication
            } else {
                S3UploadStage::Prepared
            },
            temporary: S3ObjectSnapshot {
                size: temporary.size,
                etag: temporary.etag.clone(),
            },
            previous: existing.as_ref().map(|metadata| S3ObjectSnapshot {
                size: metadata.size,
                etag: metadata.etag.clone(),
            }),
            publication_guard,
        };
        let mut journal_etag = match self
            .write_upload_transaction(&journal_key, &transaction, None)
            .await
        {
            Ok(etag) => etag,
            Err(error) => {
                return Err(self
                    .finish_uncommitted_internal_upload(
                        &intent_key,
                        intent_etag.as_deref(),
                        &intent,
                        error,
                    )
                    .await);
            }
        };
        if let Err(error) = self
            .release_internal_upload_intent(&intent_key, intent_etag.as_deref())
            .await
        {
            tracing::warn!(%error, "S3 internal upload intent handoff remains pending");
        }

        let mut backup_created = false;
        let mut verified_backup_etag = None;
        if let Some(existing) = existing
            .as_ref()
            .filter(|_| transaction.publication_guard.is_none())
        {
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
            verified_backup_etag = Some(backup_etag);
            transaction.stage = S3UploadStage::BackupCreated;
            journal_etag = self
                .write_upload_transaction(&journal_key, &transaction, journal_etag.as_deref())
                .await?;
        }

        if transaction.publication_guard.is_some() {
            if let Err(error) = self
                .verify_conditional_publish(&transaction, &backup_key)
                .await
            {
                let cleanup = self
                    .cleanup_uncommitted_guarded_upload(
                        &journal_key,
                        journal_etag.as_deref(),
                        &transaction,
                        &intent_key,
                        intent_etag.as_deref(),
                    )
                    .await;
                return Err(error.with_operation(CommitState::NotCommitted, cleanup));
            }
            transaction.stage = S3UploadStage::PublicationStarted;
            journal_etag = self
                .write_upload_transaction(&journal_key, &transaction, journal_etag.as_deref())
                .await
                .map_err(|error| {
                    error.with_operation(CommitState::NotCommitted, CleanupState::Pending)
                })?;
        }
        let commit = self
            .copy_key_with_operation(
                &temporary_key,
                &destination_key,
                temporary.etag.as_deref(),
                create_only,
                Some(&upload_id),
                transaction.publication_guard.as_ref(),
            )
            .await;
        if transaction.publication_guard.is_some()
            && commit
                .as_ref()
                .err()
                .is_some_and(|error| error.status() == axum::http::StatusCode::PRECONDITION_FAILED)
        {
            // Persist rejection before reclaiming the journal. Restart must
            // never restore a backup over the winner of the failed condition.
            transaction.stage = S3UploadStage::PublicationRejected;
            let rejected = self
                .write_upload_transaction(&journal_key, &transaction, journal_etag.as_deref())
                .await;
            let cleanup = match rejected {
                Ok(etag) => {
                    self.cleanup_uncommitted_guarded_upload(
                        &journal_key,
                        etag.as_deref(),
                        &transaction,
                        &intent_key,
                        intent_etag.as_deref(),
                    )
                    .await
                }
                Err(_) => CleanupState::Pending,
            };
            return Err(
                AppError::PreconditionFailed.with_operation(CommitState::NotCommitted, cleanup)
            );
        }
        let committed = match commit {
            Ok(committed_etag) => self
                .head_key(&destination_key)
                .await
                .map_err(|error| error.with_operation(CommitState::Unknown, CleanupState::Pending))?
                .filter(|value| {
                    value.size == content_length
                        && value.etag.as_ref() == Some(&committed_etag)
                        && (transaction.publication_guard.is_none()
                            || value.operation_id.as_deref() == Some(upload_id.as_str()))
                }),
            Err(error) => {
                tracing::warn!(%error, "S3 upload commit returned an ambiguous failure");
                self.head_key(&destination_key)
                    .await
                    .map_err(|error| {
                        error.with_operation(CommitState::Unknown, CleanupState::Pending)
                    })?
                    .filter(|value| {
                        value.size == content_length
                            && (value.operation_id.as_deref() == Some(upload_id.as_str())
                                || (transaction.publication_guard.is_none()
                                    && temporary.etag.is_some()
                                    && value.etag == temporary.etag))
                    })
            }
        };

        let Some(committed) = committed else {
            if transaction.publication_guard.is_some() {
                // A missing marker or an unchanged destination does not prove
                // this upload never committed. Keep its evidence and never
                // publish the old backup over a newer formal object.
                return Err(AppError::ServiceUnavailable(
                    "条件上传提交结果尚未确认；已保留临时记录，未回滚或改写正式文件".into(),
                )
                .with_operation(CommitState::Unknown, CleanupState::Pending));
            }
            if backup_created {
                let restored_etag = match self
                    .copy_key(
                        &backup_key,
                        &destination_key,
                        verified_backup_etag.as_deref(),
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
                // A copy need not preserve the old object's ETag. Persist the
                // verified restored identity before leaving cleanup to recovery.
                transaction.previous = restored.map(|value| S3ObjectSnapshot {
                    size: value.size,
                    etag: value.etag,
                });
                journal_etag = self
                    .write_upload_transaction(&journal_key, &transaction, journal_etag.as_deref())
                    .await?;
            } else if self.head_key(&destination_key).await?.is_some() {
                // An unrecognized formal object is not evidence of a rollback.
                // Keep its journal; only the verified cases below may clean up.
                return Err(AppError::ServiceUnavailable(
                    "对象存储未能确认上传提交结果".into(),
                ));
            }
            let cleanup = self
                .finish_upload_and_intent(
                    &journal_key,
                    journal_etag.as_deref(),
                    &transaction,
                    &intent_key,
                    intent_etag.as_deref(),
                )
                .await;
            let cleanup = if cleanup.is_ok() {
                CleanupState::Complete
            } else {
                CleanupState::Pending
            };
            return Err(
                AppError::ServiceUnavailable("对象存储未能确认上传提交结果".into())
                    .with_operation(CommitState::NotCommitted, cleanup),
            );
        };

        transaction.stage = S3UploadStage::DestinationCommitted;
        // The pre-commit journal is already durable. Recovery proves commit
        // from the formal object's operation marker and size, independently
        // of the journal stage; another remote journal write adds no evidence
        // and must not hold a verified result hostage.

        // Success describes the verified formal object, not garbage
        // collection. Cleanup has a durable journal and a background worker;
        // a slow remote delete must not turn a successful upload into failure
        // or keep the administrator edit barrier occupied indefinitely.
        super::CompletionMode::Foreground
            .finish(self.finish_upload_and_intent(
                &journal_key,
                journal_etag.as_deref(),
                &transaction,
                &intent_key,
                intent_etag.as_deref(),
            ))
            .await?;
        Ok(S3UploadResult {
            relative,
            size: committed.size,
            previous_size: existing.as_ref().map_or(0, |metadata| metadata.size),
            created: existing.is_none(),
            etag: committed.etag,
        })
    }

    pub(super) async fn single_upload(
        &self,
        key: &str,
        body: Body,
        content_length: u64,
        content_type: Option<&str>,
        operation_id: &str,
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
            .metadata(S3_OPERATION_METADATA_KEY, operation_id)
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
}

fn validate_temporary_upload(
    metadata: Option<RawS3Metadata>,
    content_length: u64,
) -> AppResult<RawS3Metadata> {
    match metadata {
        Some(metadata) if metadata.size == content_length => Ok(metadata),
        Some(_) => Err(AppError::ServiceUnavailable(
            "对象存储暂存对象长度校验失败".into(),
        )),
        None => Err(AppError::ServiceUnavailable(
            "对象存储未保存上传的暂存对象".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_upload_requires_presence_and_exact_size_without_changing_identity() {
        for size in [0, 42] {
            let metadata = RawS3Metadata {
                size,
                etag: Some("temporary-etag".into()),
                content_type: Some("text/plain".into()),
                operation_id: Some("operation-id".into()),
            };
            assert_eq!(
                validate_temporary_upload(Some(metadata.clone()), size).unwrap(),
                metadata
            );
            let error = validate_temporary_upload(Some(metadata), size + 1).unwrap_err();
            assert!(error.to_string().contains("长度校验失败"));
        }
        let error = validate_temporary_upload(None, 0).unwrap_err();
        assert!(error.to_string().contains("未保存"));
    }
}
