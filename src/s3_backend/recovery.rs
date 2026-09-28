use std::{sync::atomic::Ordering, time::Duration};

use super::{
    internal_key, multipart_session_matches, object_key, snapshot_matches, S3Backend,
    S3MultipartAbortOutcome, S3MultipartPurpose, S3MultipartSession, S3RecoveryStatus,
    S3UploadTransaction, S3_MULTIPART_SESSION_SCHEMA_VERSION, S3_TRANSACTION_SCHEMA_VERSION,
};
use crate::error::{AppError, AppResult};

impl S3Backend {
    pub(crate) async fn quiesce_after_interrupt(
        &self,
    ) -> (
        tokio::sync::OwnedRwLockWriteGuard<()>,
        tokio::sync::OwnedMutexGuard<()>,
    ) {
        let recovery = self.recovery_gate.clone().write_owned().await;
        let capacity = self.capacity_gate.clone().lock_owned().await;
        (recovery, capacity)
    }
    pub(crate) async fn upload_committed(
        &self,
        relative: &str,
        size: u64,
        operation_id: &str,
    ) -> AppResult<bool> {
        let key = object_key(&self.prefix, relative)?;
        Ok(self.head_key(&key).await?.is_some_and(|object| {
            object.size == size && object.operation_id.as_deref() == Some(operation_id)
        }))
    }
    pub(crate) async fn quiesce_for_edit(
        &self,
    ) -> AppResult<(
        tokio::sync::OwnedRwLockWriteGuard<()>,
        tokio::sync::OwnedMutexGuard<()>,
    )> {
        let recovery = self
            .recovery_gate
            .clone()
            .try_write_owned()
            .map_err(|_| AppError::Conflict("存储正在执行或恢复操作，请稍后修改连接".into()))?;
        let capacity = self
            .capacity_gate
            .clone()
            .try_lock_owned()
            .map_err(|_| AppError::Conflict("存储正在核对容量，请稍后修改连接".into()))?;
        Ok((recovery, capacity))
    }
    /// Resolve upload journals left by an interrupted process. Recovery only
    /// accepts states that can be proven to be either the old object or the
    /// newly uploaded object. Anything else stops activation for inspection.
    pub async fn recover_transactions(&self) -> AppResult<usize> {
        let _recovery = self.recovery_gate.write().await;
        let _mutation = self.mutation_gate.lock().await;
        self.recover_transactions_locked().await
    }

    pub(crate) async fn recover_runtime_transactions(
        &self,
        capacity: &crate::capacity::CapacityTracker,
    ) -> AppResult<Option<usize>> {
        let _recovery = self.recovery_gate.write().await;
        let _mutation = self.mutation_gate.lock().await;
        if self.recovery_worker_stopped() {
            return Ok(None);
        }
        if !self.recovery_runtime.has_pending() {
            // The foreground owner may have settled its own journal while the
            // worker waited for the exclusive gate. End the in-progress state
            // without scanning a namespace that no longer needs recovery.
            self.recovery_runtime.recovery_succeeded();
            return Ok(None);
        }
        capacity.mark_uncertain();
        let recovered = self.recover_transactions_locked().await?;
        // No new journal may enter between this settlement and releasing the
        // recovery gate. A later operation keeps its own pending record.
        self.recovery_runtime.recovery_succeeded();
        Ok(Some(recovered))
    }

    pub(crate) async fn recover_quiesced_uploads(&self) -> AppResult<usize> {
        let _mutation = self.mutation_gate.lock().await;
        self.recover_transactions_locked().await
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
        // Authenticated v2 intents cannot send parts or issue signed URLs before
        // the provider ID is persisted. An absent ID therefore owns no payload.
        // Do not infer ownership from a key, or touch any destination object.
        if session.schema_version == S3_MULTIPART_SESSION_SCHEMA_VERSION
            && session.upload_id.is_none()
        {
            self.delete_key_confirmed(journal_key, Some(journal_etag))
                .await?;
            tracing::warn!(
                "S3 pre-transfer intent released; any empty remote multipart session requires bucket lifecycle cleanup"
            );
            return Ok(());
        }
        let object = self.head_key(&session.key).await?;
        if object
            .as_ref()
            .is_some_and(|metadata| multipart_session_matches(metadata, session))
        {
            let completed_directory_trash = if let Some(id) =
                super::directory_trash_transaction_id(&self.prefix, &session.key)
            {
                self.head_key(&internal_key(&self.prefix, "directory-transactions", id))
                    .await?
                    .is_none()
            } else {
                false
            };
            if session.purpose == Some(S3MultipartPurpose::Upload) || completed_directory_trash {
                self.delete_key_confirmed(
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
        }
        self.delete_key_confirmed(journal_key, Some(journal_etag))
            .await?;
        Ok(())
    }

    async fn recover_upload_transaction(
        &self,
        journal_key: &str,
        journal_etag: &str,
        transaction: &S3UploadTransaction,
    ) -> AppResult<()> {
        let destination_key = object_key(&self.prefix, &transaction.relative)?;
        let destination = self.head_key(&destination_key).await?;

        let committed = destination.as_ref().is_some_and(|value| {
            snapshot_matches(value, &transaction.temporary)
                || (value.size == transaction.temporary.size
                    && value.operation_id.as_deref() == Some(transaction.id.as_str()))
        });
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
