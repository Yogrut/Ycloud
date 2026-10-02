use std::{future::Future, time::Duration};

use crate::error::{AppError, AppResult, CleanupState, CommitState};

/// A failed sub-step is not proof that the whole durable transaction did
/// nothing. Recovery may already own a partially copied/deleted transaction.
pub(super) fn uncertain_transaction(error: AppError) -> AppError {
    let error = match error {
        AppError::Operation { outcome, cause } if outcome.commit == CommitState::NotCommitted => {
            *cause
        }
        other => other,
    };
    error.with_operation(CommitState::Unknown, CleanupState::Pending)
}

/// Foreground callers have already verified the formal file change. Recovery
/// must instead finish every cleanup step before it may settle the journal.
#[derive(Clone, Copy)]
pub(super) enum CompletionMode {
    Foreground,
    Recovery,
}

impl CompletionMode {
    pub(super) async fn finish(
        self,
        cleanup: impl Future<Output = AppResult<()>>,
    ) -> AppResult<()> {
        match self {
            Self::Recovery => cleanup.await.map_err(|error| {
                // The nested outcome describes cleanup, not the formal change
                // that this caller has already verified as committed.
                let error = match error {
                    AppError::Operation { cause, .. } => *cause,
                    other => other,
                };
                error.with_operation(CommitState::Committed, CleanupState::Pending)
            }),
            Self::Foreground => {
                match tokio::time::timeout(Duration::from_millis(500), cleanup).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "verified file change retained cleanup for recovery");
                    }
                    Err(_) => {
                        tracing::warn!("verified file change deferred slow cleanup to recovery");
                    }
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    #[tokio::test]
    async fn verified_changes_succeed_but_recovery_does_not_settle_failed_cleanup() {
        assert!(CompletionMode::Foreground
            .finish(async { Err(AppError::ServiceUnavailable("cleanup unavailable".into())) })
            .await
            .is_ok());
        let error = CompletionMode::Recovery
            .finish(async { Err(AppError::ServiceUnavailable("cleanup unavailable".into())) })
            .await
            .unwrap_err();
        assert_eq!(error.operation().unwrap().commit, CommitState::Committed);
        assert_eq!(error.operation().unwrap().cleanup, CleanupState::Pending);
    }

    #[tokio::test]
    async fn cleanup_substep_outcomes_cannot_reclassify_a_verified_formal_commit() {
        for commit in [
            CommitState::NotCommitted,
            CommitState::Unknown,
            CommitState::Committed,
        ] {
            let error = CompletionMode::Recovery
                .finish(async {
                    Err(
                        AppError::ServiceUnavailable("owned trash still exists".into())
                            .with_operation(commit, CleanupState::Pending),
                    )
                })
                .await
                .unwrap_err();
            assert_eq!(error.operation().unwrap().commit, CommitState::Committed);
            assert_eq!(error.operation().unwrap().cleanup, CleanupState::Pending);
            assert!(error.blocks_retry());
            let AppError::Operation { cause, .. } = error else {
                panic!("cleanup error must carry the verified operation outcome");
            };
            assert_eq!(cause.to_string(), "owned trash still exists");
        }
    }

    #[tokio::test]
    async fn foreground_does_not_wait_for_slow_cleanup() {
        let started = tokio::time::Instant::now();
        CompletionMode::Foreground
            .finish(async {
                tokio::time::sleep(Duration::from_secs(20)).await;
                Ok(())
            })
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_failed_substep_does_not_make_the_whole_transaction_retryable() {
        let error = AppError::ServiceUnavailable("one source still exists".into())
            .with_operation(CommitState::NotCommitted, CleanupState::Pending);
        assert_eq!(
            uncertain_transaction(error).operation().unwrap().commit,
            CommitState::Unknown
        );
    }
}
