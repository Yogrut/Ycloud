//! Pure single-item rules. Authorization, locks and durable updates belong to
//! the batch store; deciding to start here does not mutate an item.
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult, CleanupState, CommitState, OperationOutcome};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Pending,
    InProgress,
    Complete,
    Failed,
    Unknown,
    Cancelled,
}

impl UploadStatus {
    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Cancelled)
    }

    pub(super) fn needs_recovery(self) -> bool {
        matches!(self, Self::InProgress | Self::Unknown)
    }

    pub(super) fn can_cancel(self) -> bool {
        matches!(self, Self::Pending | Self::Failed)
    }

    pub(super) fn after_restart(self) -> Self {
        match self {
            Self::Pending => Self::Cancelled,
            Self::InProgress => Self::Unknown,
            other => other,
        }
    }
}

pub(super) struct UploadItem {
    pub(super) request_path: String,
    pub(super) size: u64,
    pub(super) status: UploadStatus,
    pub(super) operation: Option<OperationOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadBegin {
    Start,
    AlreadyComplete(Option<OperationOutcome>),
}

impl UploadItem {
    // The store checks identity and size before calling this, then persists
    // InProgress before mutating memory. Completed results retain their proof.
    pub(super) fn begin_decision(&self) -> AppResult<UploadBegin> {
        match self.status {
            UploadStatus::Pending | UploadStatus::Failed => Ok(UploadBegin::Start),
            UploadStatus::Complete => Ok(UploadBegin::AlreadyComplete(self.operation)),
            UploadStatus::InProgress => Err(AppError::Conflict("上传目标正在执行".into())),
            UploadStatus::Unknown => Err(AppError::Conflict(
                "上传结果尚未确认，请先查询任务状态，不要直接重试".into(),
            )
            .with_operation(
                self.operation
                    .map_or(CommitState::Unknown, |value| value.commit),
                self.operation
                    .map_or(CleanupState::Unknown, |value| value.cleanup),
            )),
            UploadStatus::Cancelled => Err(AppError::Conflict("上传目标已取消".into())),
        }
    }
}

pub(super) fn classify_upload_result<T>(
    result: &AppResult<T>,
) -> (UploadStatus, Option<OperationOutcome>) {
    match result {
        Ok(_) => (UploadStatus::Complete, None),
        Err(error) => {
            let operation = error.operation();
            let status = match operation {
                Some(outcome) if outcome.commit == CommitState::Committed => UploadStatus::Complete,
                // Pending cleanup is not safe to reopen even when publication
                // is known not to have happened. Keep the recovery reservation.
                Some(outcome)
                    if outcome.commit == CommitState::Unknown
                        || outcome.cleanup != CleanupState::Complete =>
                {
                    UploadStatus::Unknown
                }
                _ => UploadStatus::Failed,
            };
            (status, operation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUSES: [UploadStatus; 6] = [
        UploadStatus::Pending,
        UploadStatus::InProgress,
        UploadStatus::Complete,
        UploadStatus::Failed,
        UploadStatus::Unknown,
        UploadStatus::Cancelled,
    ];

    #[test]
    fn status_facts_distinguish_completion_cancellation_and_recovery() {
        let expected = [
            (false, false, true),
            (false, true, false),
            (true, false, false),
            (true, false, true),
            (false, true, false),
            (true, false, false),
        ];
        for (status, facts) in STATUSES.into_iter().zip(expected) {
            assert_eq!(
                (
                    status.is_terminal(),
                    status.needs_recovery(),
                    status.can_cancel()
                ),
                facts,
                "{status:?}"
            );
        }
    }

    #[test]
    fn status_wire_names_remain_compatible() {
        for (status, name) in STATUSES.into_iter().zip([
            "pending",
            "in_progress",
            "complete",
            "failed",
            "unknown",
            "cancelled",
        ]) {
            let encoded = format!("\"{name}\"");
            assert_eq!(serde_json::to_string(&status).unwrap(), encoded);
            assert_eq!(
                serde_json::from_str::<UploadStatus>(&encoded).unwrap(),
                status
            );
        }
    }

    #[test]
    fn restart_never_reopens_old_work_or_discards_known_results() {
        for (before, after) in STATUSES.into_iter().zip([
            UploadStatus::Cancelled,
            UploadStatus::Unknown,
            UploadStatus::Complete,
            UploadStatus::Failed,
            UploadStatus::Unknown,
            UploadStatus::Cancelled,
        ]) {
            assert_eq!(before.after_restart(), after);
            assert_eq!(after.after_restart(), after);
        }
    }

    #[test]
    fn ordinary_results_keep_existing_success_and_failure_behavior() {
        assert_eq!(
            classify_upload_result(&Ok(42)),
            (UploadStatus::Complete, None)
        );
        assert_eq!(
            classify_upload_result::<()>(&Err(AppError::BadRequest("invalid input".into()))),
            (UploadStatus::Failed, None)
        );
    }

    #[test]
    fn result_classification_preserves_every_commit_and_cleanup_combination() {
        for commit in [
            CommitState::NotCommitted,
            CommitState::Committed,
            CommitState::Unknown,
        ] {
            for cleanup in [
                CleanupState::Complete,
                CleanupState::Pending,
                CleanupState::Unknown,
            ] {
                let error = AppError::internal("upload result").with_operation(commit, cleanup);
                let outcome = error.operation();
                let expected = match (commit, cleanup) {
                    (CommitState::Committed, _) => UploadStatus::Complete,
                    (CommitState::NotCommitted, CleanupState::Complete) => UploadStatus::Failed,
                    _ => UploadStatus::Unknown,
                };
                assert_eq!(
                    classify_upload_result::<()>(&Err(error)),
                    (expected, outcome)
                );
            }
        }
    }

    #[test]
    fn begin_is_a_pure_decision_and_never_reopens_completed_or_uncertain_work() {
        for status in STATUSES {
            let outcome = match status {
                UploadStatus::Complete => Some(OperationOutcome::new(
                    CommitState::Committed,
                    CleanupState::Pending,
                )),
                UploadStatus::Failed => Some(OperationOutcome::new(
                    CommitState::NotCommitted,
                    CleanupState::Complete,
                )),
                // A non-commit proof with pending cleanup still cannot retry.
                UploadStatus::Unknown => Some(OperationOutcome::new(
                    CommitState::NotCommitted,
                    CleanupState::Pending,
                )),
                _ => None,
            };
            let item = UploadItem {
                request_path: "file.txt".into(),
                size: 4,
                status,
                operation: outcome,
            };
            let decision = item.begin_decision();
            match status {
                UploadStatus::Pending | UploadStatus::Failed => {
                    assert_eq!(decision.unwrap(), UploadBegin::Start)
                }
                UploadStatus::Complete => {
                    assert_eq!(decision.unwrap(), UploadBegin::AlreadyComplete(outcome))
                }
                UploadStatus::Unknown => assert_eq!(decision.unwrap_err().operation(), outcome),
                UploadStatus::InProgress | UploadStatus::Cancelled => {
                    assert!(matches!(decision, Err(AppError::Conflict(_))))
                }
            }
            assert_eq!(item.status, status);
            assert_eq!(item.operation, outcome);
            assert_eq!(item.request_path, "file.txt");
            assert_eq!(item.size, 4);
        }
    }

    #[test]
    fn unknown_begin_without_proof_reports_unknown_instead_of_a_retryable_failure() {
        let item = UploadItem {
            request_path: "file.txt".into(),
            size: 4,
            status: UploadStatus::Unknown,
            operation: None,
        };
        let error = item.begin_decision().unwrap_err();
        assert_eq!(
            error.operation(),
            Some(OperationOutcome::new(
                CommitState::Unknown,
                CleanupState::Unknown
            ))
        );
        assert_eq!(error.code(), "operation_result_unknown");
    }
}
