use super::{
    committed_cleanup::uncertain_transaction, internal_key, object_key, CompletionMode, S3Backend,
    S3_FILE_MOVE_JOURNAL_PURPOSE,
};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

mod record;

use record::{
    prepared_destination_matches, snapshot_matches, validate_transaction, ObjectSnapshot, Stage,
    Transaction, JOURNAL_CATEGORY, SCHEMA_VERSION,
};

impl S3Backend {
    /// Verify a copied destination before deleting the matching source.
    /// S3 moves are not atomic; unresolved journaled work remains recoverable.
    pub async fn move_file(&self, source: &str, destination: &str) -> AppResult<()> {
        let backend = self.scoped_work(None, None);
        Box::pin(backend.move_file_scoped(source, destination)).await
    }

    async fn move_file_scoped(&self, source: &str, destination: &str) -> AppResult<()> {
        let source = StorageService::normalize_relative(source)?;
        let destination = StorageService::normalize_relative(destination)?;
        if source.is_empty() || destination.is_empty() || source == destination {
            return Err(AppError::BadRequest("无效的文件移动路径".into()));
        }

        let _mutation = self
            .maintenance
            .read(async { Ok(self.mutation_gate.lock().await) })
            .await?;
        let source_metadata = self.metadata(&source).await?;
        if source_metadata.is_dir {
            return Err(AppError::Conflict("当前操作只接受普通文件".into()));
        }
        self.ensure_parent_directory(&destination).await?;
        match self.metadata(&destination).await {
            Ok(_) => return Err(AppError::Conflict("目标路径已经存在".into())),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let source_etag = source_metadata
            .etag
            .clone()
            .ok_or_else(|| AppError::ServiceUnavailable("源对象缺少 ETag，无法安全移动".into()))?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let journal_key = internal_key(&self.prefix, JOURNAL_CATEGORY, &id);
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id,
            source_relative: source,
            destination_relative: destination,
            stage: Stage::Prepared,
            source: ObjectSnapshot {
                size: source_metadata.size,
                etag: source_etag,
            },
            destination: None,
        };
        let journal_etag = self
            .write_file_move_transaction(&journal_key, &transaction, None)
            .await
            .map_err(|error| error.with_operation(CommitState::Unknown, CleanupState::Pending))?;
        self.execute_file_move_transaction(
            &journal_key,
            journal_etag,
            &mut transaction,
            CompletionMode::Foreground,
        )
        .await
        .map_err(uncertain_transaction)
    }

    pub(super) async fn recover_file_move_transaction(&self, key: &str) -> AppResult<()> {
        let (mut transaction, journal_etag) = self.read_file_move_transaction(key).await?;
        self.execute_file_move_transaction(
            key,
            Some(journal_etag),
            &mut transaction,
            CompletionMode::Recovery,
        )
        .await
    }

    async fn execute_file_move_transaction(
        &self,
        journal_key: &str,
        mut journal_etag: Option<String>,
        transaction: &mut Transaction,
        completion: CompletionMode,
    ) -> AppResult<()> {
        validate_transaction(&self.prefix, journal_key, transaction)?;
        let source_key = object_key(&self.prefix, &transaction.source_relative)?;
        let destination_key = object_key(&self.prefix, &transaction.destination_relative)?;
        if transaction.stage == Stage::Prepared {
            let source = self.head_key(&source_key).await?;
            if !source
                .as_ref()
                .is_some_and(|metadata| snapshot_matches(metadata, &transaction.source))
            {
                return Err(ambiguous_move(
                    "对象存储移动源在目标提交前已经变化；已保留事务并停止删除",
                ));
            }

            let destination = self.head_key(&destination_key).await?;
            let destination_etag = match destination.as_ref() {
                None => {
                    self.copy_key_with_operation(
                        &source_key,
                        &destination_key,
                        Some(&transaction.source.etag),
                        true,
                        Some(&transaction.id),
                    )
                    .await?
                }
                Some(metadata) if prepared_destination_matches(metadata, transaction) => metadata
                    .etag
                    .clone()
                    .ok_or_else(|| AppError::ServiceUnavailable("移动目标缺少 ETag".into()))?,
                Some(_) => {
                    return Err(ambiguous_move(
                        "对象存储移动目标无法归属于当前事务；已保留源和事务",
                    ));
                }
            };
            let destination = self.head_key(&destination_key).await?;
            let destination_metadata = destination.as_ref().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储移动目标在复制后不可见".into())
            })?;
            if destination_metadata.size != transaction.source.size
                || destination_metadata.etag.as_deref() != Some(destination_etag.as_str())
            {
                return Err(ambiguous_move(
                    "对象存储移动目标复制结果无法验证；已保留源和事务",
                ));
            }
            transaction.destination = Some(ObjectSnapshot {
                size: destination_metadata.size,
                etag: destination_etag,
            });
            transaction.stage = Stage::DestinationCopied;
            journal_etag = self
                .write_file_move_transaction(journal_key, transaction, journal_etag.as_deref())
                .await
                .map_err(|error| {
                    error.with_operation(CommitState::Unknown, CleanupState::Pending)
                })?;
        }

        let recorded_destination = transaction
            .destination
            .as_ref()
            .ok_or_else(|| AppError::ServiceUnavailable("对象存储移动事务缺少目标快照".into()))?;
        let destination = self.head_key(&destination_key).await?;
        if !destination
            .as_ref()
            .is_some_and(|metadata| snapshot_matches(metadata, recorded_destination))
        {
            return Err(ambiguous_move(
                "对象存储移动目标在删除源前已经变化；已保留源和事务",
            ));
        }

        let source = self.head_key(&source_key).await?;
        match source.as_ref() {
            None => {}
            Some(metadata) if snapshot_matches(metadata, &transaction.source) => {
                self.delete_key_after_identity_check(&source_key, Some(&transaction.source.etag))
                    .await
                    .map_err(uncertain_transaction)?;
            }
            Some(_) => {
                return Err(ambiguous_move(
                    "对象存储移动源在删除前已经变化；已保留新源和事务",
                ));
            }
        }

        completion
            .finish(self.delete_key_confirmed(journal_key, journal_etag.as_deref()))
            .await
    }

    async fn write_file_move_transaction(
        &self,
        key: &str,
        transaction: &Transaction,
        previous_etag: Option<&str>,
    ) -> AppResult<Option<String>> {
        validate_transaction(&self.prefix, key, transaction)?;
        self.write_authenticated_json_journal(
            key,
            S3_FILE_MOVE_JOURNAL_PURPOSE,
            transaction,
            previous_etag,
        )
        .await
    }

    async fn read_file_move_transaction(&self, key: &str) -> AppResult<(Transaction, String)> {
        let (transaction, etag) = self
            .read_authenticated_json_journal(key, S3_FILE_MOVE_JOURNAL_PURPOSE)
            .await?;
        validate_transaction(&self.prefix, key, &transaction)?;
        Ok((transaction, etag))
    }
}

fn ambiguous_move(message: &'static str) -> AppError {
    AppError::ServiceUnavailable(message.into())
        .with_operation(CommitState::Unknown, CleanupState::Pending)
}

#[cfg(test)]
mod tests;
