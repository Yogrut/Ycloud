use crate::error::{AppError, AppResult};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(super) struct Maintenance {
    current: Arc<Mutex<CancellationToken>>,
    scope: Option<WorkScope>,
}

#[derive(Clone)]
struct WorkScope {
    generation: CancellationToken,
    stop: CancellationToken,
    deadline: Option<Instant>,
}

impl Maintenance {
    pub(super) fn new() -> Self {
        Self {
            current: Arc::new(Mutex::new(CancellationToken::new())),
            scope: None,
        }
    }
    fn token(&self) -> CancellationToken {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(super) fn pause(&self) {
        self.token().cancel();
    }
    pub(super) fn resume(&self) {
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = CancellationToken::new();
    }
    fn scoped(&self, deadline: Option<Instant>, stop: Option<CancellationToken>) -> Self {
        let mut scope = self.scope.clone().unwrap_or_else(|| WorkScope {
            generation: self.token(),
            stop: stop.clone().unwrap_or_default(),
            deadline,
        });
        // Nested operations retain their original generation and cannot extend
        // an enclosing deadline. Callers supply shutdown only at the outer pass.
        if let Some(deadline) = deadline {
            scope.deadline = Some(scope.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        Self {
            current: self.current.clone(),
            scope: Some(scope),
        }
    }
    /// Only read-only work and deletion of owned temporary objects may be
    /// dropped. Never wrap a formal PUT/COPY/DELETE in this cancellation boundary.
    pub(super) fn read<T>(
        &self,
        work: impl Future<Output = AppResult<T>>,
    ) -> impl Future<Output = AppResult<T>> {
        let token = self
            .scope
            .as_ref()
            .map_or_else(|| self.token(), |scope| scope.generation.clone());
        let stop = self
            .scope
            .as_ref()
            .map(|scope| scope.stop.clone())
            .unwrap_or_default();
        let deadline = self.scope.as_ref().and_then(|scope| scope.deadline);
        // Box before constructing the async state: SDK request futures are
        // large, and nested by-value wrappers overflow Windows debug stacks.
        let work = Box::pin(work);
        async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(AppError::Conflict("存储配置正在修改，后台核对已暂停".into())),
                _ = stop.cancelled() => Err(AppError::Conflict("存储恢复已停止，记录保留待下次自动处理".into())),
                _ = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => Err(AppError::ServiceUnavailable("存储处理达到本轮时限，记录保留待下次自动处理".into())),
                result = work => result,
            }
        }
    }
}

impl super::S3Backend {
    /// Share the client and gates, but freeze the cancellation generation for
    /// this operation. A deadline cancels reads/admission, never an issued write.
    pub(crate) fn scoped_work(
        &self,
        deadline: Option<Instant>,
        stop: Option<CancellationToken>,
    ) -> Self {
        let mut backend = self.clone();
        backend.maintenance = Arc::new(self.maintenance.scoped(deadline, stop));
        backend
    }
    pub(crate) fn pause_maintenance(&self) {
        self.maintenance.pause();
    }
    pub(crate) fn resume_maintenance(&self) {
        self.maintenance.resume();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resumed_generation_does_not_revive_old_or_nested_work() {
        let maintenance = Maintenance::new();
        let old = maintenance.scoped(None, None);
        maintenance.pause();
        maintenance.resume();

        let executed = std::sync::atomic::AtomicBool::new(false);
        let nested = old.scoped(None, None);
        for scope in [&old, &nested] {
            let error = scope
                .read(async {
                    executed.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                })
                .await
                .unwrap_err();
            assert_eq!(error.code(), "conflict");
        }
        assert!(!executed.load(std::sync::atomic::Ordering::Acquire));
        assert!(maintenance
            .scoped(None, None)
            .read(async { Ok(()) })
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn nested_scope_cannot_extend_deadline_or_remove_shutdown() {
        let maintenance = Maintenance::new();
        let expired = Instant::now() - std::time::Duration::from_secs(1);
        let stop = CancellationToken::new();
        let pass = maintenance.scoped(Some(expired), Some(stop.clone()));
        let nested = pass.scoped(
            Some(Instant::now() + std::time::Duration::from_secs(60)),
            None,
        );
        assert_eq!(nested.scope.as_ref().unwrap().deadline, Some(expired));
        let executed = std::sync::atomic::AtomicBool::new(false);
        let error = nested
            .read(async {
                executed.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), "service_unavailable");
        assert!(!executed.load(std::sync::atomic::Ordering::Acquire));

        stop.cancel();
        assert_eq!(
            nested.read(async { Ok(()) }).await.unwrap_err().code(),
            "conflict"
        );
        // The pass deadline and shutdown do not poison the live backend.
        assert!(maintenance.read(async { Ok(()) }).await.is_ok());
    }

    #[tokio::test]
    async fn pause_interrupts_reads_and_resume_admits_new_work() {
        let maintenance = std::sync::Arc::new(Maintenance::new());
        let worker = maintenance.clone();
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let task = tokio::spawn(async move {
            worker
                .read(async {
                    signal.notify_one();
                    std::future::pending::<AppResult<()>>().await
                })
                .await
        });
        started.notified().await;
        maintenance.pause();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(maintenance.read(async { Ok(()) }).await.is_err());
        maintenance.resume();
        assert!(maintenance.read(async { Ok(()) }).await.is_ok());
    }
}
