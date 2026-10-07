use std::{sync::atomic::Ordering, time::Duration};

use super::{
    internal_key, multipart_session_matches, object_key, snapshot_matches, S3Backend,
    S3MultipartAbortOutcome, S3MultipartPurpose, S3MultipartSession, S3RecoveryStatus,
    S3UploadTransaction, S3_MULTIPART_SESSION_SCHEMA_VERSION, S3_TRANSACTION_SCHEMA_VERSION,
};
use crate::error::{AppError, AppResult};

// Preserve the namespace recovery order. In particular, upload transactions
// must settle before their internal-upload intent can release temporary data.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum JournalKind {
    ActivationProbe,
    Multipart,
    FileMove,
    Upload,
    Directory,
    InternalUpload,
}

impl JournalKind {
    const ORDERED: [Self; 6] = [
        Self::ActivationProbe,
        Self::Multipart,
        Self::FileMove,
        Self::Upload,
        Self::Directory,
        Self::InternalUpload,
    ];

    const fn category(self) -> &'static str {
        match self {
            Self::ActivationProbe => "activation-probe-intents",
            Self::Multipart => "multipart-sessions",
            Self::FileMove => "file-move-transactions",
            Self::Upload => "transactions",
            Self::Directory => "directory-transactions",
            Self::InternalUpload => "internal-upload-intents",
        }
    }
}

fn runtime_journal_kind(prefix: &str, key: &str) -> AppResult<JournalKind> {
    let reserved = format!("{prefix}.ycloud-system/");
    let (category, _) = key
        .strip_prefix(&reserved)
        .and_then(|relative| relative.split_once('/'))
        .filter(|(_, id)| super::valid_transaction_id(id))
        .ok_or_else(invalid_runtime_journal)?;
    JournalKind::ORDERED
        .into_iter()
        .find(|kind| kind.category() == category)
        .ok_or_else(invalid_runtime_journal)
}

fn invalid_runtime_journal() -> AppError {
    AppError::ServiceUnavailable("对象存储待恢复记录不属于当前日志命名空间；已保留记录".into())
}

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
        let backend = self.scoped_work(None, None);
        let _recovery = backend
            .maintenance
            .read(async { Ok(backend.recovery_gate.write().await) })
            .await?;
        let _mutation = backend
            .maintenance
            .read(async { Ok(backend.mutation_gate.lock().await) })
            .await?;
        backend.recover_transactions_locked().await
    }

    pub(crate) async fn recover_runtime_transactions(
        &self,
        capacity: &crate::capacity::CapacityTracker,
    ) -> AppResult<Option<usize>> {
        let backend = self.scoped_work(None, None);
        backend.recover_runtime_transactions_scoped(capacity).await
    }

    async fn recover_runtime_transactions_scoped(
        &self,
        capacity: &crate::capacity::CapacityTracker,
    ) -> AppResult<Option<usize>> {
        let _recovery = self
            .maintenance
            .read(async { Ok(self.recovery_gate.write().await) })
            .await?;
        let _mutation = self
            .maintenance
            .read(async { Ok(self.mutation_gate.lock().await) })
            .await?;
        if self.recovery_worker_stopped() {
            self.recovery_runtime.recovery_wait_stopped();
            return Ok(None);
        }
        let mut selected: Option<(JournalKind, String)> = None;
        // This is a pass-local snapshot, not a second durable work queue. Read
        // it only after all foreground recovery leases have left the gate.
        for key in self.recovery_runtime.pending_keys() {
            let candidate = (runtime_journal_kind(&self.prefix, &key)?, key);
            if selected.as_ref().is_none_or(|current| candidate < *current) {
                selected = Some(candidate);
            }
        }
        let Some((kind, key)) = selected else {
            // The foreground owner may have settled its own journal while the
            // worker waited for the exclusive gate. End the in-progress state
            // without scanning a namespace that no longer needs recovery.
            self.recovery_runtime.recovery_pass_succeeded();
            return Ok(None);
        };
        capacity.mark_uncertain();
        if self.head_key(&key).await?.is_none() {
            // A registered write may have failed before creating its journal.
            // This retires only the recovery entry, not an upload result.
            self.recovery_runtime.journal_settled(&key);
        } else {
            self.recover_journal(kind, &key).await?;
        }
        // Only inspect unclaimed resources after the known journals settle.
        // Inspection is read-only and remains cancellable by administrator edit.
        if !self.recovery_runtime.has_pending() {
            self.inspect_internal_orphans().await?;
        }
        self.recovery_runtime.recovery_pass_succeeded();
        Ok(Some(1))
    }

    pub(crate) async fn recover_quiesced_uploads(&self) -> AppResult<usize> {
        let backend = self.scoped_work(None, None);
        let _mutation = backend
            .maintenance
            .read(async { Ok(backend.mutation_gate.lock().await) })
            .await?;
        backend.recover_transactions_locked().await
    }

    async fn recover_transactions_locked(&self) -> AppResult<usize> {
        let mut recovered = 0_usize;
        for kind in JournalKind::ORDERED {
            recovered = recovered.saturating_add(self.recover_journal_category(kind).await?);
        }
        self.inspect_internal_orphans().await?;
        Ok(recovered)
    }

    /// Recover one bounded category while the caller holds the mutation gate.
    /// Used by full recovery and the activation probe's own-resource cleanup.
    pub(super) async fn recover_journal_category(&self, kind: JournalKind) -> AppResult<usize> {
        let keys = self.list_recovery_journal_keys(kind.category()).await?;
        for key in &keys {
            self.recover_journal(kind, key).await?;
        }
        Ok(keys.len())
    }

    // Both activation and online recovery dispatch through this boundary. Each
    // journal type still owns its authentication and object-identity algorithm.
    async fn recover_journal(&self, kind: JournalKind, key: &str) -> AppResult<()> {
        match kind {
            JournalKind::ActivationProbe => self.recover_activation_probe_intent(key).await,
            JournalKind::Multipart => {
                let (session, etag) = self.read_multipart_session(key).await?;
                self.recover_multipart_session(key, &etag, &session).await
            }
            JournalKind::FileMove => self.recover_file_move_transaction(key).await,
            JournalKind::Upload => {
                let (transaction, etag) = self.read_upload_transaction(key).await?;
                self.recover_upload_transaction(key, &etag, &transaction)
                    .await
            }
            JournalKind::Directory => self.recover_directory_transaction(key).await,
            JournalKind::InternalUpload => {
                let id = key.rsplit('/').next().expect("validated journal ID");
                if self
                    .head_key(&internal_key(&self.prefix, "transactions", id))
                    .await?
                    .is_some()
                {
                    return Err(AppError::ServiceUnavailable(
                        "内部上传仍有关联事务待恢复；已保留临时对象和记录".into(),
                    ));
                }
                self.recover_internal_upload_intent(key).await
            }
        }
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
        if transaction.publication_guard.is_some()
            && matches!(
                transaction.stage,
                super::S3UploadStage::PublicationRejected
                    | super::S3UploadStage::CheckingPublication
            )
        {
            return self
                .finish_upload_transaction(journal_key, Some(journal_etag), transaction)
                .await;
        }
        let destination_key = object_key(&self.prefix, &transaction.relative)?;
        let destination = self.head_key(&destination_key).await?;

        let committed = destination.as_ref().is_some_and(|value| {
            (transaction.publication_guard.is_none()
                && snapshot_matches(value, &transaction.temporary))
                || (value.size == transaction.temporary.size
                    && value.operation_id.as_deref() == Some(transaction.id.as_str()))
        });
        if transaction.publication_guard.is_some() {
            if committed {
                return self
                    .finish_upload_transaction(journal_key, Some(journal_etag), transaction)
                    .await;
            }
            return Err(AppError::ServiceUnavailable(
                "条件上传缺少本次操作的提交证明；已保留恢复记录且未改写正式文件".into(),
            ));
        }
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

#[cfg(test)]
mod tests;
