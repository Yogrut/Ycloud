use std::collections::BTreeMap;

use aws_smithy_types::byte_stream::ByteStream;
use axum::http::HeaderValue;
use serde::{Deserialize, Serialize};

use super::{
    authenticated_journal, capabilities, internal_key, list_prefix, non_negative_size,
    valid_transaction_id, CompletionMode, S3Backend, S3_MAX_LIST_PAGES,
    S3_MAX_PENDING_TRANSACTIONS, S3_PAGE_SIZE,
};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

const SCHEMA_VERSION: u32 = 2;
const JOURNAL_PURPOSE: &str = "directory-transaction:v2";
const MAX_OBJECTS: usize = 1_000;
const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;
// Keep directory transactions bounded. Objects above the backend's conservative
// single-copy limit use multipart copy, including authenticated deletion trash.
const MAX_SINGLE_COPY_BYTES: u64 = 5_000_000_000;
const CHECKPOINT_OBJECTS: usize = 16;
const JOURNAL_CATEGORY: &str = "directory-transactions";
const TRASH_CATEGORY: &str = "directory-trash";

fn directory_object_limit_error() -> AppError {
    AppError::Conflict(format!("目录包含超过 {MAX_OBJECTS} 个对象，超出单次安全变更上限").into())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Copy,
    Move,
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    CopyingTargets,
    CopyCompleted,
    DeletingSources,
    SourcesDeleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ObjectRecord {
    source_key: String,
    target_key: String,
    size: u64,
    source_etag: String,
    target_etag: Option<String>,
    source_deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
    schema_version: u32,
    id: String,
    operation: Operation,
    source_relative: String,
    destination_relative: Option<String>,
    stage: Stage,
    objects: Vec<ObjectRecord>,
    #[serde(default)]
    auth_tag: String,
}

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
    pub(super) async fn recover_directory_transactions(&self) -> AppResult<usize> {
        let keys = self.list_directory_transaction_keys().await?;
        let recovered = keys.len();
        for key in keys {
            let (mut transaction, journal_etag) = self.read_directory_transaction(&key).await?;
            self.execute_directory_transaction(
                &key,
                journal_etag,
                &mut transaction,
                CompletionMode::Recovery,
            )
            .await?;
        }
        Ok(recovered)
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

        let _mutation = self.mutation_gate.lock().await;
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

    async fn snapshot_objects(
        &self,
        id: &str,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
    ) -> AppResult<Vec<ObjectRecord>> {
        self.maintenance
            .read(self.snapshot_objects_uninterrupted(id, operation, source, destination))
            .await
    }

    async fn snapshot_objects_uninterrupted(
        &self,
        id: &str,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
    ) -> AppResult<Vec<ObjectRecord>> {
        let source_prefix = list_prefix(&self.prefix, source)?;
        let target_prefix = match operation {
            Operation::Copy | Operation::Move => list_prefix(
                &self.prefix,
                destination.ok_or_else(|| AppError::BadRequest("目录事务缺少目标路径".into()))?,
            )?,
            Operation::Delete => internal_key(&self.prefix, TRASH_CATEGORY, &format!("{id}/")),
        };
        let mut continuation_token: Option<String> = None;
        let mut objects = Vec::new();
        let mut listing_complete = false;
        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = MAX_OBJECTS.saturating_add(1).saturating_sub(objects.len());
            if remaining == 0 {
                return Err(directory_object_limit_error());
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 transaction list page size"))?;
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&source_prefix)
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory transaction listing failed"
                );
                AppError::ServiceUnavailable("无法读取待变更的对象存储目录".into())
            })?;
            drop(permit);

            for object in output.contents() {
                if objects.len() == MAX_OBJECTS {
                    return Err(directory_object_limit_error());
                }
                let source_key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录包含无键对象".into())
                })?;
                let suffix = source_key.strip_prefix(&source_prefix).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录列举结果越出源前缀".into())
                })?;
                let source_etag = object.e_tag().map(str::to_owned).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录对象缺少 ETag，无法安全变更".into())
                })?;
                let size = non_negative_size(object.size())?;
                if size > MAX_SINGLE_COPY_BYTES {
                    return Err(AppError::Conflict(
                        "目录包含超过 5 GB 的对象；当前安全目录事务尚不支持分段复制".into(),
                    ));
                }
                objects.push(ObjectRecord {
                    source_key: source_key.to_owned(),
                    target_key: format!("{target_prefix}{suffix}"),
                    size,
                    source_etag,
                    target_etag: None,
                    source_deleted: false,
                });
            }

            if !output.is_truncated().unwrap_or(false) {
                listing_complete = true;
                break;
            }
            let next = output.next_continuation_token().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储目录分页结果缺少继续令牌".into())
            })?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储目录分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        if !listing_complete {
            return Err(AppError::ServiceUnavailable(
                "对象存储目录分页超过安全页数上限".into(),
            ));
        }
        if objects.is_empty() {
            return Err(AppError::Conflict(
                "隐式空目录没有可安全变更的目录标记".into(),
            ));
        }
        Ok(objects)
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
        if transaction.stage != Stage::SourcesDeleted && transaction.stage != Stage::CopyCompleted {
            journal_etag = self
                .copy_missing_targets(journal_key, journal_etag, transaction)
                .await?;
        }

        if transaction.operation == Operation::Copy {
            return completion
                .finish(async {
                    if transaction.stage != Stage::CopyCompleted {
                        transaction.stage = Stage::CopyCompleted;
                        journal_etag = self
                            .write_directory_transaction(
                                journal_key,
                                transaction,
                                Some(&journal_etag),
                            )
                            .await?;
                    }
                    self.delete_key_confirmed(journal_key, Some(&journal_etag))
                        .await
                })
                .await;
        }

        if transaction.stage != Stage::SourcesDeleted {
            if transaction.stage == Stage::CopyingTargets {
                transaction.stage = Stage::DeletingSources;
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
            }
            journal_etag = self
                .delete_sources(journal_key, journal_etag, transaction)
                .await?;
            transaction.stage = Stage::SourcesDeleted;
        }

        completion
            .finish(async {
                // The formal change has been verified. Persisting its final stage
                // and removing owned trash are cleanup, not another file commit.
                journal_etag = self
                    .write_directory_transaction(journal_key, transaction, Some(&journal_etag))
                    .await?;
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
            if index + 1 < transaction.objects.len()
                && should_checkpoint(index, transaction.objects.len())
            {
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
            self.delete_key_confirmed(&object.target_key, object.target_etag.as_deref())
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
                    self.delete_key_confirmed(
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
            if index + 1 < transaction.objects.len()
                && should_checkpoint(index, transaction.objects.len())
            {
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

    async fn write_directory_transaction(
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

    async fn list_directory_transaction_keys(&self) -> AppResult<Vec<String>> {
        self.maintenance
            .read(self.list_directory_transaction_keys_uninterrupted())
            .await
    }

    async fn list_directory_transaction_keys_uninterrupted(&self) -> AppResult<Vec<String>> {
        let prefix = internal_key(&self.prefix, JOURNAL_CATEGORY, "");
        let mut continuation_token: Option<String> = None;
        let mut keys = Vec::new();
        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = S3_MAX_PENDING_TRANSACTIONS
                .saturating_add(1)
                .saturating_sub(keys.len());
            if remaining == 0 {
                return Err(AppError::ServiceUnavailable(
                    "待恢复对象存储目录事务超过安全上限".into(),
                ));
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 transaction list page size"))?;
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory transaction journal listing failed"
                );
                AppError::ServiceUnavailable("无法列举对象存储目录事务记录".into())
            })?;
            drop(permit);

            for object in output.contents() {
                if keys.len() == S3_MAX_PENDING_TRANSACTIONS {
                    return Err(AppError::ServiceUnavailable(
                        "待恢复对象存储目录事务超过安全上限".into(),
                    ));
                }
                let key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录事务记录缺少键名".into())
                })?;
                let id = key.strip_prefix(&prefix).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录事务记录越出保留前缀".into())
                })?;
                if !valid_transaction_id(id) {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储目录事务区包含无法识别的记录".into(),
                    ));
                }
                keys.push(key.to_owned());
            }
            if !output.is_truncated().unwrap_or(false) {
                return Ok(keys);
            }
            let next = output.next_continuation_token().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储目录事务分页结果无效".into())
            })?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储目录事务分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        Err(AppError::ServiceUnavailable(
            "对象存储目录事务分页超过安全页数上限".into(),
        ))
    }

    async fn read_directory_transaction(&self, key: &str) -> AppResult<(Transaction, String)> {
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
        if output.content_length().is_some_and(|length| {
            length < 0 || usize::try_from(length).map_or(true, |value| value > MAX_JOURNAL_BYTES)
        }) {
            return Err(AppError::ServiceUnavailable(
                "对象存储目录事务记录超过安全上限".into(),
            ));
        }
        let etag = output
            .e_tag()
            .map(str::to_owned)
            .ok_or_else(|| AppError::ServiceUnavailable("目录事务记录缺少 ETag".into()))?;
        let data = output.body.collect().await.map_err(|error| {
            AppError::with_source("failed to stream S3 directory transaction journal", error)
        })?;
        let data = data.into_bytes();
        if data.len() > MAX_JOURNAL_BYTES {
            return Err(AppError::ServiceUnavailable(
                "对象存储目录事务记录超过安全上限".into(),
            ));
        }
        let transaction: Transaction = serde_json::from_slice(&data)
            .map_err(|error| AppError::with_source("invalid S3 directory transaction", error))?;
        validate_transaction(&self.transaction_auth_key, &self.prefix, key, &transaction)?;
        Ok((transaction, etag))
    }
}

fn validate_transaction(
    auth_key: &[u8; 32],
    prefix: &str,
    journal_key: &str,
    transaction: &Transaction,
) -> AppResult<()> {
    if transaction.schema_version == 1 && transaction.auth_tag.is_empty() {
        return Err(AppError::ServiceUnavailable(
            "检测到旧版未认证目录事务；已保留记录并拒绝自动执行".into(),
        ));
    }
    validate_transaction_structure(prefix, journal_key, transaction)?;
    verify_transaction_auth(auth_key, transaction)
}

fn validate_transaction_structure(
    prefix: &str,
    journal_key: &str,
    transaction: &Transaction,
) -> AppResult<()> {
    if transaction.schema_version != SCHEMA_VERSION
        || !valid_transaction_id(&transaction.id)
        || journal_key != internal_key(prefix, JOURNAL_CATEGORY, &transaction.id)
        || transaction.objects.is_empty()
        || transaction.objects.len() > MAX_OBJECTS
    {
        return Err(invalid_transaction());
    }
    let source = StorageService::normalize_relative(&transaction.source_relative)?;
    if source.is_empty() || source != transaction.source_relative {
        return Err(invalid_transaction());
    }
    let source_prefix = list_prefix(prefix, &source)?;
    let target_prefix = expected_target_prefix(prefix, transaction, &source)?;

    if transaction.operation == Operation::Copy
        && (!matches!(
            transaction.stage,
            Stage::CopyingTargets | Stage::CopyCompleted
        ) || transaction
            .objects
            .iter()
            .any(|object| object.source_deleted))
    {
        return Err(invalid_transaction());
    }
    if transaction.operation != Operation::Copy && transaction.stage == Stage::CopyCompleted {
        return Err(invalid_transaction());
    }
    if transaction.stage == Stage::SourcesDeleted
        && transaction
            .objects
            .iter()
            .any(|object| !object.source_deleted)
    {
        return Err(invalid_transaction());
    }
    if transaction.stage == Stage::CopyCompleted
        && transaction
            .objects
            .iter()
            .any(|object| object.target_etag.is_none())
    {
        return Err(invalid_transaction());
    }

    let mut source_keys = BTreeMap::new();
    let mut target_keys = BTreeMap::new();
    for object in &transaction.objects {
        let suffix = object
            .source_key
            .strip_prefix(&source_prefix)
            .ok_or_else(invalid_transaction)?;
        if object.source_key.len() > 1_024
            || object.target_key.len() > 1_024
            || object.source_etag.is_empty()
            || object.size > MAX_SINGLE_COPY_BYTES
            || object.source_etag.len() > 1_024
            || object
                .target_etag
                .as_ref()
                .is_some_and(|etag| etag.is_empty() || etag.len() > 1_024)
            || object.target_key != format!("{target_prefix}{suffix}")
            || object.source_key.chars().any(char::is_control)
            || object.target_key.chars().any(char::is_control)
            || (object.source_deleted && object.target_etag.is_none())
            || source_keys.insert(&object.source_key, ()).is_some()
            || target_keys.insert(&object.target_key, ()).is_some()
        {
            return Err(invalid_transaction());
        }
    }
    Ok(())
}

fn transaction_auth_bytes(transaction: &Transaction) -> AppResult<Vec<u8>> {
    let mut unsigned = transaction.clone();
    unsigned.auth_tag.clear();
    let encoded = serde_json::to_vec(&unsigned).map_err(|error| {
        AppError::with_source("failed to authenticate S3 directory transaction", error)
    })?;
    Ok(encoded)
}

fn sign_transaction(auth_key: &[u8; 32], transaction: &mut Transaction) -> AppResult<()> {
    let payload = transaction_auth_bytes(transaction)?;
    transaction.auth_tag = authenticated_journal::sign_payload(auth_key, JOURNAL_PURPOSE, &payload);
    Ok(())
}

fn verify_transaction_auth(auth_key: &[u8; 32], transaction: &Transaction) -> AppResult<()> {
    let payload = transaction_auth_bytes(transaction)?;
    authenticated_journal::verify_payload(
        auth_key,
        JOURNAL_PURPOSE,
        &payload,
        &transaction.auth_tag,
    )
    .map_err(|_| invalid_transaction())
}

fn expected_target_prefix(
    prefix: &str,
    transaction: &Transaction,
    source: &str,
) -> AppResult<String> {
    match transaction.operation {
        Operation::Copy | Operation::Move => {
            let destination = transaction
                .destination_relative
                .as_deref()
                .ok_or_else(invalid_transaction)?;
            let normalized = StorageService::normalize_relative(destination)?;
            if normalized.is_empty()
                || normalized != destination
                || normalized == source
                || normalized.starts_with(&format!("{source}/"))
            {
                return Err(invalid_transaction());
            }
            list_prefix(prefix, &normalized)
        }
        Operation::Delete => {
            if transaction.destination_relative.is_some() {
                return Err(invalid_transaction());
            }
            Ok(internal_key(
                prefix,
                TRASH_CATEGORY,
                &format!("{}/", transaction.id),
            ))
        }
    }
}

fn directory_multipart_target_matches(
    transaction: &Transaction,
    session: &super::S3MultipartSession,
) -> bool {
    transaction.operation == Operation::Delete
        && transaction.objects.iter().any(|object| {
            object.target_key == session.key && Some(object.size) == session.expected_size
        })
}

fn directory_copy_id(transaction_id: &str, source_key: &str) -> String {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{transaction_id}:{source_key}").as_bytes(),
    );
    digest.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn should_checkpoint(index: usize, total: usize) -> bool {
    index.saturating_add(1) == total || index.saturating_add(1).is_multiple_of(CHECKPOINT_OBJECTS)
}

fn invalid_transaction() -> AppError {
    AppError::ServiceUnavailable("对象存储目录事务记录无法安全恢复".into())
}

fn ambiguous_target() -> AppError {
    AppError::ServiceUnavailable("目录事务目标状态不明确；已保留事务并停止写入".into())
}

#[cfg(test)]
mod tests {
    use super::{
        should_checkpoint, sign_transaction, validate_transaction, validate_transaction_structure,
        ObjectRecord, Operation, Stage, Transaction, JOURNAL_CATEGORY, SCHEMA_VERSION,
        TRASH_CATEGORY,
    };
    use crate::s3_backend::internal_key;

    #[tokio::test]
    async fn verified_directory_delete_does_not_fail_or_settle_when_trash_changed() {
        use axum::{
            body::Body,
            http::{Method, Response, Uri},
            routing::any,
            Router,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let deletions = Arc::new(AtomicUsize::new(0));
        let observed = deletions.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri| {
                let observed = observed.clone();
                async move {
                    let response = Response::builder();
                    if method == Method::PUT {
                        response
                            .header("etag", "\"journal-etag\"")
                            .body(Body::empty())
                    } else if method == Method::HEAD && uri.path().contains("/directory-trash/") {
                        // An external change to owned trash must not be deleted.
                        response
                            .header("content-length", "43")
                            .header("etag", "changed-etag")
                            .body(Body::empty())
                    } else {
                        if method == Method::DELETE {
                            observed.fetch_add(1, Ordering::SeqCst);
                        }
                        response.status(400).body(Body::empty())
                    }
                    .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let id = "fedcba9876543210fedcba9876543210";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Delete,
            source_relative: "source".into(),
            destination_relative: None,
            stage: Stage::SourcesDeleted,
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/file.bin")),
                size: 42,
                source_etag: "source-etag".into(),
                target_etag: Some("trash-etag".into()),
                source_deleted: true,
            }],
            auth_tag: String::new(),
        };
        sign_transaction(&[0x31; 32], &mut transaction).unwrap();
        backend
            .execute_directory_transaction(
                &key,
                "\"journal-etag\"".into(),
                &mut transaction,
                super::CompletionMode::Foreground,
            )
            .await
            .unwrap();
        assert!(backend.recovery_has_pending());
        let error = backend
            .execute_directory_transaction(
                &key,
                "\"journal-etag\"".into(),
                &mut transaction,
                super::CompletionMode::Recovery,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.operation().unwrap().commit,
            crate::error::CommitState::Committed
        );
        assert!(backend.recovery_has_pending());
        assert_eq!(deletions.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[test]
    fn authenticated_large_delete_manifest_binds_its_multipart_target() {
        let id = "0123456789abcdef0123456789abcdef";
        let size = super::super::S3_SINGLE_COPY_LIMIT + 1;
        let target = internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/file.bin"));
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Delete,
            source_relative: "source".into(),
            destination_relative: None,
            stage: Stage::CopyingTargets,
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: target.clone(),
                size,
                source_etag: "source-etag".into(),
                target_etag: None,
                source_deleted: false,
            }],
            auth_tag: String::new(),
        };
        sign_transaction(&[0x31; 32], &mut transaction).unwrap();
        validate_transaction(
            &[0x31; 32],
            "tenant/",
            &internal_key("tenant/", JOURNAL_CATEGORY, id),
            &transaction,
        )
        .unwrap();
        let session = super::super::S3MultipartSession {
            schema_version: super::super::S3_MULTIPART_SESSION_SCHEMA_VERSION,
            id: super::directory_copy_id(id, "tenant/source/file.bin"),
            key: target,
            purpose: Some(super::super::S3MultipartPurpose::Copy),
            expected_size: Some(size),
            upload_id: None,
        };
        assert!(super::directory_multipart_target_matches(
            &transaction,
            &session
        ));
        super::super::validate_multipart_session(
            "tenant/",
            &internal_key("tenant/", "multipart-sessions", &session.id),
            &session,
        )
        .unwrap();
        assert_eq!(
            session.id,
            super::directory_copy_id(id, "tenant/source/file.bin")
        );
    }

    #[test]
    fn copy_manifest_is_confined_to_exact_prefix_mapping() {
        let id = "0123456789abcdef0123456789abcdef";
        let transaction = copy_transaction(id);
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());

        let mut escaped = transaction.clone();
        escaped.objects[1].target_key = "tenant/outside/file.bin".into();
        assert!(validate_transaction_structure("tenant/", &key, &escaped).is_err());

        let mut duplicate = transaction.clone();
        duplicate.objects.push(duplicate.objects[0].clone());
        assert!(validate_transaction_structure("tenant/", &key, &duplicate).is_err());
    }

    #[test]
    fn delete_manifest_only_targets_internal_trash() {
        let id = "fedcba9876543210fedcba9876543210";
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Delete,
            source_relative: "source".into(),
            destination_relative: None,
            stage: Stage::SourcesDeleted,
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: format!("tenant/.ycloud-system/{TRASH_CATEGORY}/{id}/file.bin"),
                size: 42,
                source_etag: "source-etag".into(),
                target_etag: Some("trash-etag".into()),
                source_deleted: true,
            }],
            auth_tag: String::new(),
        };
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.objects[0].target_key = "tenant/source-backup/file.bin".into();
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
    }

    #[test]
    fn impossible_stage_and_duplicate_keys_are_rejected() {
        let id = "00112233445566778899aabbccddeeff";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = copy_transaction(id);
        transaction.operation = Operation::Move;
        transaction.stage = Stage::SourcesDeleted;
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());

        let mut oversized = copy_transaction(id);
        oversized.objects[1].size = super::MAX_SINGLE_COPY_BYTES + 1;
        assert!(validate_transaction_structure("tenant/", &key, &oversized).is_err());
    }

    #[test]
    fn completed_copy_checkpoint_requires_every_target_to_be_recorded() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = copy_transaction(id);
        transaction.stage = Stage::CopyCompleted;
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
        for object in &mut transaction.objects {
            object.target_etag = Some("copied-etag".into());
        }
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.operation = Operation::Move;
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
    }

    #[test]
    fn authenticated_manifest_rejects_tampering_wrong_installation_and_legacy_records() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let auth_key = [0x41; 32];
        let mut transaction = copy_transaction(id);
        sign_transaction(&auth_key, &mut transaction).unwrap();
        assert!(validate_transaction(&auth_key, "tenant/", &key, &transaction).is_ok());

        let mut tampered = transaction.clone();
        tampered.objects[1].size += 1;
        assert!(validate_transaction(&auth_key, "tenant/", &key, &tampered).is_err());
        assert!(validate_transaction(&[0x42; 32], "tenant/", &key, &transaction).is_err());

        let mut legacy = copy_transaction(id);
        legacy.schema_version = 1;
        assert!(validate_transaction(&auth_key, "tenant/", &key, &legacy).is_err());
    }

    #[test]
    fn progress_is_checkpointed_in_bounded_batches() {
        assert!(!should_checkpoint(0, 40));
        assert!(should_checkpoint(15, 40));
        assert!(should_checkpoint(31, 40));
        assert!(should_checkpoint(39, 40));
    }

    fn copy_transaction(id: &str) -> Transaction {
        Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Copy,
            source_relative: "source".into(),
            destination_relative: Some("destination".into()),
            stage: Stage::CopyingTargets,
            objects: vec![
                ObjectRecord {
                    source_key: "tenant/source/".into(),
                    target_key: "tenant/destination/".into(),
                    size: 0,
                    source_etag: "marker-etag".into(),
                    target_etag: Some("copied-marker-etag".into()),
                    source_deleted: false,
                },
                ObjectRecord {
                    source_key: "tenant/source/nested/file.bin".into(),
                    target_key: "tenant/destination/nested/file.bin".into(),
                    size: 42,
                    source_etag: "file-etag".into(),
                    target_etag: None,
                    source_deleted: false,
                },
            ],
            auth_tag: String::new(),
        }
    }
}
