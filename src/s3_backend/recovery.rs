use std::{sync::atomic::Ordering, time::Duration};

use super::{
    append_exact_multipart_matches, capabilities, is_owned_internal_multipart_key,
    multipart_session_matches, object_key, snapshot_matches, S3Backend, S3MultipartAbortOutcome,
    S3MultipartPurpose, S3MultipartSession, S3RecoveryStatus, S3UploadTransaction,
    S3_MAX_LIST_PAGES, S3_PAGE_SIZE, S3_TRANSACTION_SCHEMA_VERSION,
};
use crate::error::{AppError, AppResult};

impl S3Backend {
    /// Resolve upload journals left by an interrupted process. Recovery only
    /// accepts states that can be proven to be either the old object or the
    /// newly uploaded object. Anything else stops activation for inspection.
    pub async fn recover_transactions(&self) -> AppResult<usize> {
        let _recovery = self.recovery_gate.write().await;
        let _mutation = self.mutation_gate.lock().await;
        self.recover_transactions_locked().await
    }

    pub(crate) async fn recover_runtime_transactions(&self) -> AppResult<Option<usize>> {
        let _recovery = self.recovery_gate.write().await;
        let _mutation = self.mutation_gate.lock().await;
        if !self.recovery_runtime.has_pending() {
            return Ok(None);
        }
        self.recover_transactions_locked().await.map(Some)
    }

    async fn recover_transactions_locked(&self) -> AppResult<usize> {
        let mut recovered = self.recover_activation_probe_intents().await?;
        let multipart_sessions = self.list_multipart_session_keys().await?;
        recovered = recovered.saturating_add(multipart_sessions.len());
        for key in multipart_sessions {
            let (session, journal_etag) = self.read_multipart_session(&key).await?;
            self.recover_multipart_session(&key, &journal_etag, &session)
                .await?;
        }
        recovered = recovered.saturating_add(self.recover_file_move_transactions().await?);
        let transaction_keys = self.list_transaction_keys().await?;
        recovered = recovered.saturating_add(transaction_keys.len());
        for key in transaction_keys {
            let (transaction, journal_etag) = self.read_upload_transaction(&key).await?;
            self.recover_upload_transaction(&key, &journal_etag, &transaction)
                .await?;
        }
        recovered = recovered.saturating_add(self.recover_directory_transactions().await?);
        recovered = recovered.saturating_add(self.recover_internal_upload_intents().await?);
        self.inspect_internal_orphans().await?;
        Ok(recovered)
    }

    pub(crate) fn recovery_has_pending(&self) -> bool {
        self.recovery_runtime.has_pending()
    }

    pub(crate) fn begin_recovery_worker(&self) -> bool {
        self.recovery_runtime.begin_worker()
    }

    pub(crate) fn stop_recovery_worker(&self) {
        self.recovery_runtime.stop_worker();
    }

    pub(crate) fn recovery_worker_stopped(&self) -> bool {
        self.recovery_runtime.is_stopped()
    }

    pub(crate) async fn wait_for_recovery_work(&self) {
        self.recovery_runtime.wait_for_work().await;
    }

    pub(crate) async fn wait_for_recovery_shutdown(&self) {
        self.recovery_runtime.wait_for_shutdown().await;
    }

    pub(crate) fn runtime_recovery_started(&self) {
        self.recovery_runtime.recovery_started();
    }

    pub(crate) fn runtime_recovery_succeeded(&self) {
        self.recovery_runtime.recovery_succeeded();
    }

    pub(crate) fn runtime_recovery_failed(&self, message: &str, retry_after: Duration) {
        self.recovery_runtime.recovery_failed(message, retry_after);
    }

    async fn inspect_internal_orphans(&self) -> AppResult<()> {
        let uploads = self.list_recovery_journal_keys("uploads").await?.len();
        let backups = self.list_recovery_journal_keys("backups").await?.len();
        self.orphan_uploads.store(uploads, Ordering::Relaxed);
        self.orphan_backups.store(backups, Ordering::Relaxed);
        if uploads > 0 || backups > 0 {
            tracing::warn!(
                uploads,
                backups,
                "S3 internal orphan objects require attention"
            );
        }
        Ok(())
    }

    pub fn recovery_status(&self) -> S3RecoveryStatus {
        self.recovery_runtime.status(
            self.orphan_uploads.load(Ordering::Relaxed),
            self.orphan_backups.load(Ordering::Relaxed),
        )
    }

    pub(super) async fn recover_multipart_session(
        &self,
        journal_key: &str,
        journal_etag: &str,
        session: &S3MultipartSession,
    ) -> AppResult<()> {
        let object = self.head_key(&session.key).await?;
        if object
            .as_ref()
            .is_some_and(|metadata| multipart_session_matches(metadata, session))
        {
            if session.purpose == Some(S3MultipartPurpose::Upload) {
                self.delete_key(
                    &session.key,
                    object
                        .as_ref()
                        .and_then(|metadata| metadata.etag.as_deref()),
                )
                .await?;
            }
            self.delete_key_confirmed(journal_key, Some(journal_etag))
                .await?;
            return Ok(());
        }

        if session.schema_version == S3_TRANSACTION_SCHEMA_VERSION {
            self.abort_multipart_operation(
                &session.key,
                session
                    .upload_id
                    .as_deref()
                    .expect("version 1 session validation requires an upload ID"),
            )
            .await?;
            self.delete_key_confirmed(journal_key, Some(journal_etag))
                .await?;
            return Ok(());
        }

        if let Some(upload_id) = session.upload_id.as_deref() {
            match self
                .abort_multipart_operation(&session.key, upload_id)
                .await?
            {
                S3MultipartAbortOutcome::Aborted => {}
                S3MultipartAbortOutcome::Missing if object.is_none() => {}
                S3MultipartAbortOutcome::Missing => {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储分片会话已消失但目标对象不匹配；已保留恢复记录并拒绝继续写入"
                            .into(),
                    ));
                }
            }
        } else {
            // A version 2 intent without a provider ID means the process may
            // have stopped after CreateMultipartUpload reached S3 but before
            // its response was durably linked. Only exact-key matches inside
            // the recorded namespace are eligible; a truncated or malformed
            // listing fails closed and preserves the intent.
            let upload_ids = self.multipart_upload_ids_for_key(&session.key).await?;
            if !is_owned_internal_multipart_key(&self.prefix, session) && !upload_ids.is_empty() {
                return Err(AppError::ServiceUnavailable(
                    "对象存储分片复制已创建但缺少可验证的会话 ID；已保留恢复意图并拒绝自动终止"
                        .into(),
                ));
            }
            for upload_id in upload_ids {
                self.abort_multipart_operation(&session.key, &upload_id)
                    .await?;
            }
            if object.is_some() {
                return Err(AppError::ServiceUnavailable(
                    "对象存储分片意图对应的目标对象不匹配；已保留恢复记录并拒绝继续写入".into(),
                ));
            }
        }
        self.delete_key_confirmed(journal_key, Some(journal_etag))
            .await?;
        Ok(())
    }

    async fn multipart_upload_ids_for_key(&self, key: &str) -> AppResult<Vec<String>> {
        let mut key_marker: Option<String> = None;
        let mut upload_id_marker: Option<String> = None;
        let mut matches = Vec::new();
        for _ in 0..S3_MAX_LIST_PAGES {
            let _permit = self.acquire_request().await?;
            let mut request =
                self.client
                    .list_multipart_uploads()
                    .bucket(&self.bucket)
                    .prefix(key)
                    .max_uploads(i32::try_from(S3_PAGE_SIZE).map_err(|_| {
                        AppError::internal("invalid S3 multipart recovery page size")
                    })?);
            if let Some(marker) = key_marker.as_deref() {
                request = request.key_marker(marker);
            }
            if let Some(marker) = upload_id_marker.as_deref() {
                request = request.upload_id_marker(marker);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 multipart intent enumeration failed"
                );
                AppError::storage_capability(
                    capabilities::MULTIPART_LIST_EXACT_KEY,
                    "无法列举对象存储分片会话；已保留恢复意图",
                )
            })?;
            append_exact_multipart_matches(key, output.uploads(), &mut matches)?;
            if !output.is_truncated().unwrap_or(false) {
                return Ok(matches);
            }
            let next_key = output.next_key_marker().ok_or_else(|| {
                AppError::storage_capability(
                    capabilities::MULTIPART_LIST_EXACT_KEY,
                    "对象存储分片恢复分页结果无效",
                )
            })?;
            let next_upload = output.next_upload_id_marker().map(str::to_owned);
            if key_marker.as_deref() == Some(next_key)
                && upload_id_marker.as_deref() == next_upload.as_deref()
            {
                return Err(AppError::storage_capability(
                    capabilities::MULTIPART_LIST_EXACT_KEY,
                    "对象存储分片恢复分页未前进",
                ));
            }
            key_marker = Some(next_key.to_owned());
            upload_id_marker = next_upload;
        }
        Err(AppError::storage_capability(
            capabilities::MULTIPART_LIST_EXACT_KEY,
            "对象存储分片恢复分页超过安全页数上限",
        ))
    }

    async fn recover_upload_transaction(
        &self,
        journal_key: &str,
        journal_etag: &str,
        transaction: &S3UploadTransaction,
    ) -> AppResult<()> {
        let destination_key = object_key(&self.prefix, &transaction.relative)?;
        let destination = self.head_key(&destination_key).await?;

        let committed = destination
            .as_ref()
            .is_some_and(|value| snapshot_matches(value, &transaction.temporary));
        let rolled_back = match transaction.previous.as_ref() {
            Some(previous) => destination
                .as_ref()
                .is_some_and(|value| snapshot_matches(value, previous)),
            None => destination.is_none(),
        };
        if !committed && !rolled_back {
            tracing::error!(
                transaction_id = %transaction.id,
                stage = ?transaction.stage,
                "S3 transaction recovery found an ambiguous destination"
            );
            return Err(AppError::ServiceUnavailable(
                "对象存储事务目标状态不明确；已保留恢复数据并拒绝继续写入".into(),
            ));
        }

        self.finish_upload_transaction(journal_key, Some(journal_etag), transaction)
            .await
    }
}
