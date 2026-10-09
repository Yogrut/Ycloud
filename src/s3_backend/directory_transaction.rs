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
    #[cfg(test)]
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
        Box::pin(backend.mutate_path_scoped(
            operation,
            source,
            destination,
            expected_size,
            false,
            false,
        ))
        .await
        .map(|result| result.size)
    }

    pub(crate) async fn transfer_path(
        &self,
        source: &str,
        destination: &str,
        kind: crate::storage_backend::TransferKind,
        overwrite: bool,
        expected_size: u64,
    ) -> AppResult<crate::storage::AtomicWriteResult> {
        let backend = self.scoped_work(None, None);
        Box::pin(backend.mutate_path_scoped(
            match kind {
                crate::storage_backend::TransferKind::Copy => Operation::Copy,
                crate::storage_backend::TransferKind::CopyCollection => Operation::CopyCollection,
                crate::storage_backend::TransferKind::Move => Operation::Move,
            },
            source,
            Some(destination),
            Some(expected_size),
            overwrite,
            true,
        ))
        .await
    }

    async fn mutate_path_scoped(
        &self,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
        expected_size: Option<u64>,
        overwrite: bool,
        allow_files: bool,
    ) -> AppResult<crate::storage::AtomicWriteResult> {
        let source = StorageService::normalize_relative(source)?;
        if source.is_empty() {
            return Err(AppError::BadRequest("不能变更存储根目录".into()));
        }
        let destination = destination
            .map(StorageService::normalize_relative)
            .transpose()?;
        if matches!(
            operation,
            Operation::Copy | Operation::CopyCollection | Operation::Move
        ) {
            let destination = destination
                .as_deref()
                .ok_or_else(|| AppError::BadRequest("目录复制或移动缺少目标路径".into()))?;
            if destination.is_empty()
                || destination == source
                || destination.starts_with(&format!("{source}/"))
                || source.starts_with(&format!("{destination}/"))
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
        let operation = if operation == Operation::CopyCollection && !source_metadata.is_dir {
            Operation::Copy
        } else {
            operation
        };
        if !source_metadata.is_dir && !allow_files {
            return Err(AppError::Conflict("源路径不是目录".into()));
        }
        let existing = if let Some(destination) = destination.as_deref() {
            self.ensure_parent_directory(destination).await?;
            match self.metadata(destination).await {
                Ok(_) if !overwrite => {
                    return Err(if allow_files {
                        AppError::PreconditionFailed
                    } else {
                        AppError::Conflict("目标路径已经存在".into())
                    })
                }
                Ok(metadata) => Some(metadata),
                Err(AppError::NotFound) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };

        let id = uuid::Uuid::new_v4().simple().to_string();
        let objects = if operation == Operation::CopyCollection {
            vec![ObjectRecord {
                source_key: super::list_prefix(&self.prefix, &source)?,
                target_key: super::list_prefix(
                    &self.prefix,
                    destination.as_deref().ok_or(AppError::Forbidden)?,
                )?,
                size: 0,
                source_etag: String::new(),
                target_etag: None,
                source_deleted: false,
            }]
        } else {
            self.snapshot_resource(
                &id,
                operation,
                &source,
                destination.as_deref(),
                source_metadata.is_dir,
            )
            .await?
        };
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
        let replacement = if let Some(metadata) = &existing {
            let old_target = destination
                .as_deref()
                .expect("replacement has a destination");
            let objects = self
                .snapshot_resource(&id, Operation::Delete, old_target, None, metadata.is_dir)
                .await?;
            Some(Box::new(Transaction {
                schema_version: SCHEMA_VERSION,
                id: id.clone(),
                operation: Operation::Delete,
                source_relative: old_target.to_owned(),
                destination_relative: None,
                stage: Stage::CopyingTargets,
                objects,
                source_is_file: !metadata.is_dir,
                replacement: None,
                auth_tag: String::new(),
            }))
        } else {
            None
        };
        let previous_size = replacement.as_ref().map_or(Ok(0), |replacement| {
            replacement.objects.iter().try_fold(0_u64, |total, object| {
                total
                    .checked_add(object.size)
                    .ok_or_else(|| AppError::internal("replacement size overflow"))
            })
        })?;
        let journal_key = internal_key(&self.prefix, JOURNAL_CATEGORY, &id);
        let mut transaction = Transaction {
            schema_version: if operation == Operation::CopyCollection {
                4
            } else {
                SCHEMA_VERSION
            },
            id,
            operation,
            source_relative: source,
            destination_relative: destination,
            stage: Stage::CopyingTargets,
            objects,
            source_is_file: !source_metadata.is_dir,
            replacement,
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
        Ok(crate::storage::AtomicWriteResult {
            size: snapshot_size,
            previous_size,
            created: existing.is_none(),
        })
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
        if transaction
            .replacement
            .as_ref()
            .is_some_and(|replacement| replacement.stage != Stage::SourcesDeleted)
        {
            journal_etag = self
                .replace_destination(journal_key, journal_etag, transaction)
                .await?;
        }
        let final_stage = transaction.operation.completed_stage();
        if transaction.stage != final_stage {
            journal_etag = self
                .copy_missing_targets(journal_key, journal_etag, transaction)
                .await?;
        }

        if !matches!(
            transaction.operation,
            Operation::Copy | Operation::CopyCollection
        ) && transaction.stage != Stage::SourcesDeleted
        {
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
                if let Some(replacement) = &transaction.replacement {
                    self.cleanup_delete_trash(replacement).await?;
                }
                self.delete_key_confirmed(journal_key, Some(&journal_etag))
                    .await
            })
            .await
    }

    async fn replace_destination(
        &self,
        journal_key: &str,
        mut journal_etag: String,
        transaction: &mut Transaction,
    ) -> AppResult<String> {
        let replacement = transaction
            .replacement
            .as_ref()
            .expect("replacement exists");
        if replacement.stage == Stage::CopyingTargets {
            for index in 0..replacement.objects.len() {
                let replacement = transaction
                    .replacement
                    .as_mut()
                    .expect("replacement exists");
                self.copy_one_target(&transaction.id, &mut replacement.objects[index])
                    .await?;
                if should_checkpoint_progress(index, replacement.objects.len()) {
                    journal_etag = self
                        .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                        .await?;
                }
            }
            transaction
                .replacement
                .as_mut()
                .expect("replacement exists")
                .stage = Stage::DeletingSources;
            journal_etag = self
                .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                .await?;
        }
        let total = transaction
            .replacement
            .as_ref()
            .expect("replacement exists")
            .objects
            .len();
        for index in 0..total {
            let replacement = transaction
                .replacement
                .as_mut()
                .expect("replacement exists");
            self.verify_recorded_target(&replacement.objects[index])
                .await?;
            self.delete_one_source(&mut replacement.objects[index])
                .await?;
            if should_checkpoint_progress(index, total) {
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
            }
        }
        transaction
            .replacement
            .as_mut()
            .expect("replacement exists")
            .stage = Stage::SourcesDeleted;
        self.write_directory_transaction(journal_key, transaction, Some(&journal_etag))
            .await
    }

    async fn copy_missing_targets(
        &self,
        journal_key: &str,
        mut journal_etag: String,
        transaction: &mut Transaction,
    ) -> AppResult<String> {
        for index in 0..transaction.objects.len() {
            if transaction.operation == Operation::CopyCollection {
                self.create_collection_target(&transaction.id, &mut transaction.objects[index])
                    .await?;
            } else {
                self.copy_one_target(&transaction.id, &mut transaction.objects[index])
                    .await?;
            }
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

    async fn create_collection_target(
        &self,
        transaction_id: &str,
        object: &mut ObjectRecord,
    ) -> AppResult<()> {
        if object.target_etag.is_some() {
            return self.verify_recorded_target(object).await;
        }
        let operation_id = directory_copy_id(transaction_id, &object.source_key);
        if self.head_key(&object.target_key).await?.is_none() {
            self.put_directory_marker(&object.target_key, Some(&operation_id))
                .await?;
        }
        let target = self
            .head_key(&object.target_key)
            .await?
            .ok_or_else(ambiguous_target)?;
        if target.size != 0 || target.operation_id.as_deref() != Some(operation_id.as_str()) {
            return Err(ambiguous_target());
        }
        object.target_etag = Some(target.etag.ok_or_else(ambiguous_target)?);
        Ok(())
    }

    async fn copy_one_target(
        &self,
        transaction_id: &str,
        object: &mut ObjectRecord,
    ) -> AppResult<()> {
        if object.target_etag.is_some() {
            self.verify_recorded_target(object).await?;
            return Ok(());
        }
        let source = self.head_key(&object.source_key).await?;
        if !source.as_ref().is_some_and(|metadata| {
            metadata.size == object.size
                && metadata.etag.as_deref() == Some(object.source_etag.as_str())
        }) {
            return Err(AppError::ServiceUnavailable(
                "目录事务源对象已经变化；已保留事务并停止写入".into(),
            ));
        }

        let existing_target = self.head_key(&object.target_key).await?;
        let target_etag = match existing_target {
            None => {
                self.copy_key_with_operation(
                    &object.source_key,
                    &object.target_key,
                    Some(&object.source_etag),
                    true,
                    Some(&directory_copy_id(transaction_id, &object.source_key)),
                    None,
                )
                .await?
            }
            Some(metadata)
                if metadata.size == object.size
                    && (metadata.etag.as_deref() == Some(object.source_etag.as_str())
                        || metadata.operation_id.as_deref()
                            == Some(
                                directory_copy_id(transaction_id, &object.source_key).as_str(),
                            )) =>
            {
                metadata
                    .etag
                    .ok_or_else(|| AppError::ServiceUnavailable("目录事务目标缺少 ETag".into()))?
            }
            Some(_) => return Err(ambiguous_target()),
        };
        let target = self.head_key(&object.target_key).await?;
        if !target.as_ref().is_some_and(|metadata| {
            metadata.size == object.size && metadata.etag.as_deref() == Some(target_etag.as_str())
        }) {
            return Err(AppError::ServiceUnavailable(
                "目录事务复制结果无法验证；已保留事务并停止写入".into(),
            ));
        }
        object.target_etag = Some(target_etag);
        Ok(())
    }

    async fn delete_sources(
        &self,
        journal_key: &str,
        mut journal_etag: String,
        transaction: &mut Transaction,
    ) -> AppResult<String> {
        for index in 0..transaction.objects.len() {
            self.delete_one_source(&mut transaction.objects[index])
                .await?;
            if should_checkpoint_progress(index, transaction.objects.len()) {
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
            }
        }
        Ok(journal_etag)
    }

    async fn delete_one_source(&self, object: &mut ObjectRecord) -> AppResult<()> {
        if object.source_deleted {
            return Ok(());
        }
        match self.head_key(&object.source_key).await? {
            None => {}
            Some(metadata)
                if metadata.size == object.size
                    && metadata.etag.as_deref() == Some(object.source_etag.as_str()) =>
            {
                self.delete_key_after_identity_check(&object.source_key, Some(&object.source_etag))
                    .await?;
            }
            Some(_) => {
                return Err(AppError::ServiceUnavailable(
                    "目录事务源对象在删除前发生变化；已停止删除".into(),
                ));
            }
        }
        object.source_deleted = true;
        Ok(())
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
mod overwrite_tests;
#[cfg(test)]
mod tests;
