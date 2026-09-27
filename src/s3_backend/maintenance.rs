use crate::error::{AppError, AppResult};
use std::{future::Future, sync::Mutex};
use tokio_util::sync::CancellationToken;

pub(super) struct Maintenance(Mutex<CancellationToken>);

impl Maintenance {
    pub(super) fn new() -> Self {
        Self(Mutex::new(CancellationToken::new()))
    }
    fn token(&self) -> CancellationToken {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(super) fn pause(&self) {
        self.token().cancel();
    }
    pub(super) fn resume(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = CancellationToken::new();
    }
    /// Only read-only work and deletion of owned temporary objects may be
    /// dropped. Never wrap a formal PUT/COPY/DELETE in this cancellation boundary.
    pub(super) fn read<T>(
        &self,
        work: impl Future<Output = AppResult<T>>,
    ) -> impl Future<Output = AppResult<T>> {
        let token = self.token();
        // Box before constructing the async state: SDK request futures are
        // large, and nested by-value wrappers overflow Windows debug stacks.
        let work = Box::pin(work);
        async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(AppError::Conflict("存储配置正在修改，后台核对已暂停".into())),
                result = work => result,
            }
        }
    }
}

impl super::S3Backend {
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
