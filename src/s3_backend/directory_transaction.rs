use super::{internal_key, CompletionMode, S3Backend};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

mod journal;
mod record;
mod snapshot;

use record::{
    directory_copy_id, directory_multipart_target_matches, validate_transaction, ObjectRecord,
    Operation, Stage, Transaction, JOURNAL_CATEGORY, SCHEMA_VERSION,
};

const CHECKPOINT_OBJECTS: usize = 16;

impl S3Backend {
    pub(super) async fn validate_directory_multipart_target(
        &self,
        session: &super::S3MultipartSession,
    ) -> AppResult<()> {
        let Some(id) = super::directory_trash_transaction_id(&self.prefix, &session.key) else {
            return Ok(());
        };
        let key = internal_key(&self.prefix, JOURNAL_CATEGORY, id);
        let (transaction, _) = self.read_directory_transaction(&key).await?;
        if !directory_multipart_target_matches(&transaction, session) {
            return Err(ambiguous_target());
        }
        Ok(())
    }

    /// Recover bounded prefix transactions before S3 is allowed to serve
    /// requests. Any state that cannot be proven from object size and ETag is
    /// retained for later automatic recovery instead of being guessed.
    pub(super) async fn recover_directory_transaction(&self, key: &str) -> AppResult<()> {
        let (mut transaction, journal_etag) = self.read_directory_transaction(key).await?;
        self.execute_directory_transaction(
            key,
            journal_etag,
            &mut transaction,
            CompletionMode::Recovery,
        )
        .await
    }

    /// S3 has no atomic prefix rename. The operation is therefore bounded and
    /// journaled per object. A crash resumes from the last checkpoint.
    pub async fn copy_directory(&self, source: &str, destination: &str) -> AppResult<()> {
        self.mutate_directory(Operation::Copy, source, Some(destination), None)
            .await
            .map(|_| ())
    }

    pub async fn copy_directory_with_expected_size(
        &self,
        source: &str,
        destination: &str,
        expected_size: u64,
    ) -> AppResult<()> {
        self.mutate_directory(
            Operation::Copy,
            source,
            Some(destination),
            Some(expected_size),
        )
        .await
        .map(|_| ())
    }

    pub async fn move_directory(&self, source: &str, destination: &str) -> AppResult<()> {
        self.mutate_directory(Operation::Move, source, Some(destination), None)
            .await
            .map(|_| ())
    }

    /// Deletion first copies every source object to the reserved trash prefix.
    /// This is recoverable deletion, not physical secure erasure.
    pub async fn delete_directory(&self, source: &str) -> AppResult<u64> {
        self.mutate_directory(Operation::Delete, source, None, None)
            .await
    }

    async fn mutate_directory(
        &self,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
        expected_size: Option<u64>,
    ) -> AppResult<u64> {
        let backend = self.scoped_work(None, None);
        Box::pin(backend.mutate_directory_scoped(operation, source, destination, expected_size))
            .await
    }

    async fn mutate_directory_scoped(
        &self,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
        expected_size: Option<u64>,
    ) -> AppResult<u64> {
        let source = StorageService::normalize_relative(source)?;
        if source.is_empty() {
            return Err(AppError::BadRequest("不能变更存储根目录".into()));
        }
        let destination = destination
            .map(StorageService::normalize_relative)
            .transpose()?;
        if matches!(operation, Operation::Copy | Operation::Move) {
            let destination = destination
                .as_deref()
                .ok_or_else(|| AppError::BadRequest("目录复制或移动缺少目标路径".into()))?;
            if destination.is_empty()
                || destination == source
                || destination.starts_with(&format!("{source}/"))
            {
                return Err(AppError::BadRequest("无效的目录目标路径".into()));
            }
        } else if destination.is_some() {
            return Err(AppError::BadRequest("目录删除不能包含目标路径".into()));
        }

        let _mutation = self
            .maintenance
            .read(async { Ok(self.mutation_gate.lock().await) })
            .await?;
        let source_metadata = self.metadata(&source).await?;
        if !source_metadata.is_dir {
            return Err(AppError::Conflict("源路径不是目录".into()));
        }
        if let Some(destination) = destination.as_deref() {
            self.ensure_parent_directory(destination).await?;
            match self.metadata(destination).await {
                Ok(_) => return Err(AppError::Conflict("目标路径已经存在".into())),
                Err(AppError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        let objects = self
            .snapshot_objects(&id, operation, &source, destination.as_deref())
            .await?;
        let snapshot_size = objects.iter().try_fold(0_u64, |total, object| {
            total
                .checked_add(object.size)
                .ok_or_else(|| AppError::internal("S3 directory size exceeds u64"))
        })?;
        if let Some(expected_size) = expected_size {
            if snapshot_size != expected_size {
                return Err(AppError::Conflict(
                    "Source changed while preparing the copy".into(),
                ));
            }
        }
        let journal_key = internal_key(&self.prefix, JOURNAL_CATEGORY, &id);
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id,
            operation,
            source_relative: source,
            destination_relative: destination,
            stage: Stage::CopyingTargets,
            objects,
            auth_tag: String::new(),
        };
        let journal_etag = self
            .write_directory_transaction(&journal_key, &mut transaction, None)
            .await
            .map_err(|error| error.with_operation(CommitState::Unknown, CleanupState::Pending))?;
        self.execute_directory_transaction(
            &journal_key,
            journal_etag,
            &mut transaction,
            CompletionMode::Foreground,
        )
        .await
        .map_err(super::committed_cleanup::uncertain_transaction)?;
        Ok(snapshot_size)
    }

    async fn execute_directory_transaction(
        &self,
        journal_key: &str,
        mut journal_etag: String,
        transaction: &mut Transaction,
        completion: CompletionMode,
    ) -> AppResult<()> {
        validate_transaction(
            &self.transaction_auth_key,
            &self.prefix,
            journal_key,
            transaction,
        )?;
        let final_stage = transaction.operation.completed_stage();
        if transaction.stage != final_stage {
            journal_etag = self
                .copy_missing_targets(journal_key, journal_etag, transaction)
                .await?;
        }

        if transaction.operation != Operation::Copy && transaction.stage != Stage::SourcesDeleted {
            if transaction.stage == Stage::CopyingTargets {
                transaction.stage = Stage::DeletingSources;
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
            }
            journal_etag = self
                .delete_sources(journal_key, journal_etag, transaction)
                .await?;
        }

        // Formal changes are verified, so a failed final record cannot turn
        // them into failure. But an issued journal PUT must still return before
        // the owner exits; only owned garbage deletion has the cleanup timeout.
        if transaction.stage != final_stage {
            transaction.stage = final_stage;
            journal_etag = match self
                .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                .await
            {
                Ok(etag) => etag,
                Err(error) => return completion.finish(async { Err(error) }).await,
            };
        }
        completion
            .finish(async {
                if transaction.operation == Operation::Delete {
                    self.cleanup_delete_trash(transaction).await?;
                }
                self.delete_key_confirmed(journal_key, Some(&journal_etag))
                    .await
            })
            .await
    }

    async fn copy_missing_targets(
        &self,
        journal_key: &str,
        mut journal_etag: String,
        transaction: &mut Transaction,
    ) -> AppResult<String> {
        for index in 0..transaction.objects.len() {
            if transaction.objects[index].target_etag.is_some() {
                self.verify_recorded_target(&transaction.objects[index])
                    .await?;
                continue;
            }
            let source = self
                .head_key(&transaction.objects[index].source_key)
                .await?;
            if !source.as_ref().is_some_and(|metadata| {
                metadata.size == transaction.objects[index].size
                    && metadata.etag.as_deref()
                        == Some(transaction.objects[index].source_etag.as_str())
            }) {
                return Err(AppError::ServiceUnavailable(
                    "目录事务源对象已经变化；已保留事务并停止写入".into(),
                ));
            }

            let existing_target = self
                .head_key(&transaction.objects[index].target_key)
                .await?;
            let target_etag = match existing_target {
                None => {
                    self.copy_key_with_operation(
                        &transaction.objects[index].source_key,
                        &transaction.objects[index].target_key,
                        Some(&transaction.objects[index].source_etag),
                        true,
                        Some(&directory_copy_id(
                            &transaction.id,
                            &transaction.objects[index].source_key,
                        )),
                    )
                    .await?
                }
                Some(metadata)
                    if metadata.size == transaction.objects[index].size
                        && (metadata.etag.as_deref()
                            == Some(transaction.objects[index].source_etag.as_str())
                            || metadata.operation_id.as_deref()
                                == Some(
                                    directory_copy_id(
                                        &transaction.id,
                                        &transaction.objects[index].source_key,
                                    )
                                    .as_str(),
                                )) =>
                {
                    metadata.etag.ok_or_else(|| {
                        AppError::ServiceUnavailable("目录事务目标缺少 ETag".into())
                    })?
                }
                Some(_) => return Err(ambiguous_target()),
            };
            let target = self
                .head_key(&transaction.objects[index].target_key)
                .await?;
            if !target.as_ref().is_some_and(|metadata| {
                metadata.size == transaction.objects[index].size
                    && metadata.etag.as_deref() == Some(target_etag.as_str())
            }) {
                return Err(AppError::ServiceUnavailable(
                    "目录事务复制结果无法验证；已保留事务并停止写入".into(),
                ));
            }
            transaction.objects[index].target_etag = Some(target_etag);
            if should_checkpoint_progress(index, transaction.objects.len()) {
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
            }
        }
        Ok(journal_etag)
    }

    async fn cleanup_delete_trash(&self, transaction: &Transaction) -> AppResult<()> {
        for object in &transaction.objects {
            let Some(target) = self.head_key(&object.target_key).await? else {
                continue;
            };
            if target.size != object.size || target.etag != object.target_etag {
                return Err(AppError::ServiceUnavailable(
                    "目录删除暂存对象发生外部变化；已停止清理".into(),
                ));
            }
            self.delete_key_after_identity_check(&object.target_key, object.target_etag.as_deref())
                .await?;
        }
        Ok(())
    }

    async fn delete_sources(
        &self,
        journal_key: &str,
        mut journal_etag: String,
        transaction: &mut Transaction,
    ) -> AppResult<String> {
        for index in 0..transaction.objects.len() {
            if transaction.objects[index].source_deleted {
                continue;
            }
            match self
                .head_key(&transaction.objects[index].source_key)
                .await?
            {
                None => {}
                Some(metadata)
                    if metadata.size == transaction.objects[index].size
                        && metadata.etag.as_deref()
                            == Some(transaction.objects[index].source_etag.as_str()) =>
                {
                    self.delete_key_after_identity_check(
                        &transaction.objects[index].source_key,
                        Some(&transaction.objects[index].source_etag),
                    )
                    .await?;
                }
                Some(_) => {
                    return Err(AppError::ServiceUnavailable(
                        "目录事务源对象在删除前发生变化；已停止删除".into(),
                    ));
                }
            }
            transaction.objects[index].source_deleted = true;
            if should_checkpoint_progress(index, transaction.objects.len()) {
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
            }
        }
        Ok(journal_etag)
    }

    async fn verify_recorded_target(&self, object: &ObjectRecord) -> AppResult<()> {
        let target = self.head_key(&object.target_key).await?;
        if target.as_ref().is_some_and(|metadata| {
            metadata.size == object.size && metadata.etag == object.target_etag
        }) {
            Ok(())
        } else {
            Err(ambiguous_target())
        }
    }
}

// The final object is persisted with the stage transition, not a redundant
// progress write that could fail after the formal change has completed.
fn should_checkpoint_progress(index: usize, total: usize) -> bool {
    let completed = index.saturating_add(1);
    completed < total && completed.is_multiple_of(CHECKPOINT_OBJECTS)
}

fn ambiguous_target() -> AppError {
    AppError::ServiceUnavailable("目录事务目标状态不明确；已保留事务并停止写入".into())
}

#[cfg(test)]
mod tests;
