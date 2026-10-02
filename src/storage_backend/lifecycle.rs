//! Request ownership and administrator interruption for one storage instance.
//! This boundary controls admission and transfer cancellation, not file commit proof.

use axum::body::Body;
use futures_util::StreamExt;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use super::{StorageBackend, StorageBackendKind};
use crate::error::{AppError, AppResult};

// Detached HTTP waiters must not admit an unbounded number of mutation owners.
pub(super) const MAX_MUTATION_OWNERS: usize = 32;
const EDIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) type AdmissionLease = std::sync::Mutex<Option<tokio::sync::OwnedRwLockReadGuard<()>>>;

/// Storage interruption, with exclusive gates once drained. Policy publication
/// can drop an undrained guard; it retains one background drain owner instead
/// of making the administrator wait. Retirement still blocks user admission.
pub struct StorageEditGuard {
    backend: StorageBackend,
    _gate: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
    _owners: Option<tokio::sync::OwnedSemaphorePermit>,
    interrupted: bool,
    _remote: Option<(
        tokio::sync::OwnedRwLockWriteGuard<()>,
        tokio::sync::OwnedMutexGuard<()>,
    )>,
}

impl StorageEditGuard {
    pub fn retire(&self) {
        self.backend.retire();
    }

    async fn drain(&mut self) -> AppResult<()> {
        if self._owners.is_none() {
            self._owners = Some(
                self.backend
                    .active
                    .mutation_owners
                    .clone()
                    .acquire_many_owned(MAX_MUTATION_OWNERS as u32)
                    .await
                    .map_err(|_| AppError::ServiceUnavailable("存储操作已关闭".into()))?,
            );
        }
        if self._gate.is_none() {
            {
                let mut leases = self
                    .backend
                    .active
                    .leases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for lease in leases.drain(..).filter_map(|lease| lease.upgrade()) {
                    lease
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                }
            }
            self._gate = Some(self.backend.active.lifecycle.clone().write_owned().await);
        }
        if self._remote.is_none() {
            if let StorageBackendKind::S3(storage) = &self.backend.active.kind {
                self._remote = Some(storage.quiesce_after_interrupt().await);
            }
        }
        Ok(())
    }
}

impl Drop for StorageEditGuard {
    fn drop(&mut self) {
        if self.interrupted {
            let drained = self._gate.is_some()
                && (!matches!(&self.backend.active.kind, StorageBackendKind::S3(_))
                    || self._remote.is_some());
            if !drained {
                // Transfer the partially acquired gates to exactly one drain
                // owner. Do not reopen while old recovery still owns a gate.
                let mut pending = Self {
                    backend: self.backend.clone(),
                    _gate: self._gate.take(),
                    _owners: self._owners.take(),
                    interrupted: false,
                    _remote: self._remote.take(),
                };
                tokio::spawn(async move {
                    match pending.drain().await {
                        Ok(()) => reopen_after_interruption(&pending.backend),
                        Err(error) => {
                            tracing::error!(%error, "storage interruption remains closed")
                        }
                    }
                });
            } else {
                reopen_after_interruption(&self.backend);
            }
        }
    }
}

fn reopen_after_interruption(backend: &StorageBackend) {
    if let StorageBackendKind::S3(storage) = &backend.active.kind {
        // Retirement blocks user admission separately. Its original
        // connection must remain usable by deferred owned cleanup.
        storage.resume_maintenance();
    }
    *backend
        .active
        .transfer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        tokio_util::sync::CancellationToken::new();
    backend.active.editing.store(false, Ordering::Release);
}

impl StorageBackend {
    pub(crate) fn retire(&self) {
        self.active.retired.store(true, Ordering::Release);
        if let StorageBackendKind::S3(storage) = &self.active.kind {
            storage.stop_recovery_worker();
        }
    }

    pub(crate) fn interruption_pending(&self) -> bool {
        self.active.editing.load(Ordering::Acquire)
    }

    /// Called under the serialized storage-settings owner. An existing drain
    /// already keeps admission closed; do not create another drain task.
    pub(crate) fn interrupt_for_policy(&self) -> Option<StorageEditGuard> {
        self.begin_interruption()
    }

    pub(crate) async fn reenable_guard(&self) -> AppResult<StorageEditGuard> {
        let guard = self.edit_guard().await?;
        if self.active.retired.load(Ordering::Acquire)
            || self.active.mutation_owners.available_permits() != MAX_MUTATION_OWNERS
            || matches!(&self.active.kind, StorageBackendKind::S3(storage) if storage.recovery_has_pending())
        {
            return Err(AppError::Conflict(
                "存储仍有未完成的提交或恢复，请稍后启用".into(),
            ));
        }
        Ok(guard)
    }

    pub(super) fn admitted(&self) -> AppResult<Self> {
        if self.active.editing.load(Ordering::Acquire) {
            return Err(AppError::Conflict("存储配置正在更新，请重试".into()));
        }
        let gate = self
            .active
            .lifecycle
            .clone()
            .try_read_owned()
            .map_err(|_| AppError::Conflict("存储连接正在调整，请稍后重试".into()))?;
        if self.active.retired.load(Ordering::Acquire) {
            return Err(AppError::Conflict("存储连接已更新，请重试".into()));
        }
        let mut leases = self
            .active
            .leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.active.editing.load(Ordering::Acquire) {
            return Err(AppError::Conflict("存储配置正在更新，请重试".into()));
        }
        leases.retain(|lease| lease.strong_count() > 0);
        let lease = Arc::new(std::sync::Mutex::new(Some(gate)));
        leases.push(Arc::downgrade(&lease));
        Ok(Self {
            active: self.active.clone(),
            lease: Some(lease),
            transfer: Some(self.transfer_token()),
        })
    }

    pub async fn edit_guard(&self) -> AppResult<StorageEditGuard> {
        if self.active.editing.load(Ordering::Acquire) {
            return Err(AppError::Conflict("存储配置正在更新".into()));
        }
        let gate = self
            .active
            .lifecycle
            .clone()
            .try_write_owned()
            .map_err(|_| {
                AppError::Conflict("存储仍有进行中的操作，请完成后再修改连接或删除".into())
            })?;
        let remote = match &self.active.kind {
            StorageBackendKind::S3(storage) => Some(storage.quiesce_for_edit().await?),
            StorageBackendKind::Local(_) => None,
        };
        Ok(StorageEditGuard {
            backend: self.clone(),
            _gate: Some(gate),
            _owners: None,
            interrupted: false,
            _remote: remote,
        })
    }

    pub(super) fn transfer_token(&self) -> tokio_util::sync::CancellationToken {
        self.transfer.clone().unwrap_or_else(|| {
            self.active
                .transfer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
    }

    /// Administrator edits stop transfer admission and cancel payload transfer,
    /// rather than requiring clients to voluntarily finish uploading.
    pub(crate) async fn interrupt_for_edit(&self) -> AppResult<StorageEditGuard> {
        tokio::time::timeout(EDIT_DRAIN_TIMEOUT, self.interrupt_for_edit_inner())
            .await
            .map_err(|_| {
                AppError::Conflict("存储提交未在 5 秒内结束；配置未修改，请稍后重试".into())
            })?
    }

    async fn interrupt_for_edit_inner(&self) -> AppResult<StorageEditGuard> {
        let mut guard = self
            .begin_interruption()
            .ok_or_else(|| AppError::Conflict("存储设置正在更新，请稍后重试".into()))?;
        guard.drain().await?;
        Ok(guard)
    }

    fn begin_interruption(&self) -> Option<StorageEditGuard> {
        self.active
            .editing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.transfer_token().cancel();
        if let StorageBackendKind::S3(storage) = &self.active.kind {
            storage.pause_maintenance();
        }
        Some(StorageEditGuard {
            backend: self.clone(),
            _gate: None,
            _owners: None,
            interrupted: true,
            _remote: None,
        })
    }

    pub(super) async fn owned_mutation<T: Send + 'static, F, Fut>(&self, work: F) -> AppResult<T>
    where
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = AppResult<T>> + Send + 'static,
    {
        let owner = self.clone();
        let permit = owner
            .active
            .mutation_owners
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::TooManyRequests)?;
        if owner.active.editing.load(Ordering::Acquire)
            || owner.active.retired.load(Ordering::Acquire)
            || owner.transfer_token().is_cancelled()
        {
            return Err(AppError::Conflict(
                "存储配置已变更，传输已中断，请重试".into(),
            ));
        }
        tokio::spawn(async move {
            let _permit = permit;
            let _lease = owner.lease.clone();
            let result = work(owner.clone()).await;
            owner.observe_result(&result);
            result
        })
        .await
        .map_err(|error| {
            AppError::with_source("storage mutation owner failed", error).with_operation(
                crate::error::CommitState::Unknown,
                crate::error::CleanupState::Unknown,
            )
        })?
    }
}

pub(super) fn interruptible_body(
    body: Body,
    cancellation: tokio_util::sync::CancellationToken,
) -> Body {
    Body::from_stream(futures_util::stream::try_unfold(
        (body.into_data_stream(), cancellation),
        |(mut stream, cancellation)| async move {
            let chunk = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted, "存储配置已变更，传输已中断，请重试")),
                chunk = stream.next() => chunk,
            };
            match chunk {
                Some(Ok(chunk)) => Ok(Some((chunk, (stream, cancellation)))),
                Some(Err(error)) => Err(std::io::Error::other(error)),
                None => Ok(None),
            }
        },
    ))
}

#[cfg(test)]
mod tests;
