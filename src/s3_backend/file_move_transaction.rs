use serde::{Deserialize, Serialize};

use super::{
    internal_key, object_key, valid_transaction_id, RawS3Metadata, S3Backend,
    S3_FILE_MOVE_JOURNAL_PURPOSE,
};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

const SCHEMA_VERSION: u32 = 1;
const JOURNAL_CATEGORY: &str = "file-move-transactions";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Prepared,
    DestinationCopied,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ObjectSnapshot {
    size: u64,
    etag: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
    schema_version: u32,
    id: String,
    source_relative: String,
    destination_relative: String,
    stage: Stage,
    source: ObjectSnapshot,
    destination: Option<ObjectSnapshot>,
}

impl S3Backend {
    pub async fn move_file(&self, source: &str, destination: &str) -> AppResult<()> {
        let source = StorageService::normalize_relative(source)?;
        let destination = StorageService::normalize_relative(destination)?;
        if source.is_empty() || destination.is_empty() || source == destination {
            return Err(AppError::BadRequest("无效的文件移动路径".into()));
        }

        let _mutation = self.mutation_gate.lock().await;
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
        self.execute_file_move_transaction(&journal_key, journal_etag, &mut transaction)
            .await
    }

    pub(super) async fn recover_file_move_transactions(&self) -> AppResult<usize> {
        let keys = self.list_recovery_journal_keys(JOURNAL_CATEGORY).await?;
        let recovered = keys.len();
        for key in keys {
            let (mut transaction, journal_etag) = self.read_file_move_transaction(&key).await?;
            self.execute_file_move_transaction(&key, Some(journal_etag), &mut transaction)
                .await?;
        }
        Ok(recovered)
    }

    async fn execute_file_move_transaction(
        &self,
        journal_key: &str,
        mut journal_etag: Option<String>,
        transaction: &mut Transaction,
    ) -> AppResult<()> {
        validate_transaction(&self.prefix, journal_key, transaction)?;
        let source_key = object_key(&self.prefix, &transaction.source_relative)?;
        let destination_key = object_key(&self.prefix, &transaction.destination_relative)?;
        let mut source = self.head_key(&source_key).await?;
        let mut destination = self.head_key(&destination_key).await?;

        if transaction.stage == Stage::Prepared {
            if !source
                .as_ref()
                .is_some_and(|metadata| snapshot_matches(metadata, &transaction.source))
            {
                return Err(ambiguous_move(
                    "对象存储移动源在目标提交前已经变化；已保留事务并停止删除",
                ));
            }

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
            destination = self.head_key(&destination_key).await?;
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
        destination = self.head_key(&destination_key).await?;
        if !destination
            .as_ref()
            .is_some_and(|metadata| snapshot_matches(metadata, recorded_destination))
        {
            return Err(ambiguous_move(
                "对象存储移动目标在删除源前已经变化；已保留源和事务",
            ));
        }

        source = self.head_key(&source_key).await?;
        match source.as_ref() {
            None => {}
            Some(metadata) if snapshot_matches(metadata, &transaction.source) => {
                if let Err(error) = self
                    .delete_key(&source_key, Some(&transaction.source.etag))
                    .await
                {
                    if !matches!(self.head_key(&source_key).await, Ok(None)) {
                        return Err(
                            error.with_operation(CommitState::Unknown, CleanupState::Pending)
                        );
                    }
                }
                if self.head_key(&source_key).await?.is_some() {
                    return Err(ambiguous_move(
                        "对象存储未能确认移动源已经删除；已保留事务供恢复核对",
                    ));
                }
            }
            Some(_) => {
                return Err(ambiguous_move(
                    "对象存储移动源在删除前已经变化；已保留新源和事务",
                ));
            }
        }

        self.delete_key_confirmed(journal_key, journal_etag.as_deref())
            .await
            .map_err(|error| error.with_operation(CommitState::Committed, CleanupState::Pending))?;
        Ok(())
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

fn snapshot_matches(metadata: &RawS3Metadata, snapshot: &ObjectSnapshot) -> bool {
    metadata.size == snapshot.size && metadata.etag.as_deref() == Some(snapshot.etag.as_str())
}

fn prepared_destination_matches(metadata: &RawS3Metadata, transaction: &Transaction) -> bool {
    metadata.size == transaction.source.size
        && metadata.etag.is_some()
        && (metadata.etag.as_deref() == Some(transaction.source.etag.as_str())
            || metadata.operation_id.as_deref() == Some(transaction.id.as_str()))
}

fn validate_transaction(
    prefix: &str,
    journal_key: &str,
    transaction: &Transaction,
) -> AppResult<()> {
    let source = StorageService::normalize_relative(&transaction.source_relative)?;
    let destination = StorageService::normalize_relative(&transaction.destination_relative)?;
    let valid_destination = match transaction.stage {
        Stage::Prepared => transaction.destination.is_none(),
        Stage::DestinationCopied => transaction.destination.as_ref().is_some_and(valid_snapshot),
    };
    if transaction.schema_version != SCHEMA_VERSION
        || !valid_transaction_id(&transaction.id)
        || journal_key != internal_key(prefix, JOURNAL_CATEGORY, &transaction.id)
        || source.is_empty()
        || destination.is_empty()
        || source != transaction.source_relative
        || destination != transaction.destination_relative
        || source == destination
        || !valid_snapshot(&transaction.source)
        || !valid_destination
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储单文件移动记录无法安全处理".into(),
        ));
    }
    Ok(())
}

fn valid_snapshot(snapshot: &ObjectSnapshot) -> bool {
    !snapshot.etag.is_empty()
        && snapshot.etag.len() <= 4_096
        && !snapshot.etag.chars().any(char::is_control)
}

fn ambiguous_move(message: &'static str) -> AppError {
    AppError::ServiceUnavailable(message.into())
        .with_operation(CommitState::Unknown, CleanupState::Pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transaction(stage: Stage) -> Transaction {
        Transaction {
            schema_version: SCHEMA_VERSION,
            id: "0123456789abcdef0123456789abcdef".into(),
            source_relative: "source.bin".into(),
            destination_relative: "folder/destination.bin".into(),
            stage,
            source: ObjectSnapshot {
                size: 42,
                etag: "source-etag".into(),
            },
            destination: None,
        }
    }

    #[test]
    fn move_records_require_exact_stage_snapshots_and_namespace() {
        let mut value = transaction(Stage::Prepared);
        let key = internal_key("tenant/", JOURNAL_CATEGORY, &value.id);
        assert!(validate_transaction("tenant/", &key, &value).is_ok());

        value.stage = Stage::DestinationCopied;
        assert!(validate_transaction("tenant/", &key, &value).is_err());
        value.destination = Some(ObjectSnapshot {
            size: 42,
            etag: "destination-etag".into(),
        });
        assert!(validate_transaction("tenant/", &key, &value).is_ok());

        value.source_relative = "../outside".into();
        assert!(validate_transaction("tenant/", &key, &value).is_err());
        value.source_relative = "source.bin".into();
        assert!(validate_transaction("tenant/", "other/key", &value).is_err());
    }

    #[test]
    fn prepared_move_accepts_only_source_etag_or_operation_marker() {
        let value = transaction(Stage::Prepared);
        let mut metadata = RawS3Metadata {
            size: 42,
            etag: Some("source-etag".into()),
            content_type: None,
            operation_id: None,
        };
        assert!(prepared_destination_matches(&metadata, &value));

        metadata.etag = Some("multipart-etag".into());
        assert!(!prepared_destination_matches(&metadata, &value));
        metadata.operation_id = Some(value.id.clone());
        assert!(prepared_destination_matches(&metadata, &value));
        metadata.size = 41;
        assert!(!prepared_destination_matches(&metadata, &value));
    }
}
