//! Conditional object deletion and confirmation of actual absence.

use super::S3Backend;
use crate::error::{AppError, AppResult, CleanupState, CommitState};

impl S3Backend {
    pub(super) async fn delete_key(&self, key: &str, etag: Option<&str>) -> AppResult<()> {
        if key.starts_with(&format!("{}.ycloud-system/", self.prefix)) {
            self.maintenance
                .read(self.delete_key_uninterrupted(key, etag))
                .await
        } else {
            self.delete_key_uninterrupted(key, etag).await
        }
    }

    async fn delete_key_uninterrupted(&self, key: &str, etag: Option<&str>) -> AppResult<()> {
        if self.is_alibaba_oss() {
            if let Some(expected_etag) = etag {
                let current = self.head_key(key).await?;
                if current
                    .as_ref()
                    .and_then(|metadata| metadata.etag.as_deref())
                    != Some(expected_etag)
                {
                    return Err(AppError::Conflict(
                        "对象在删除前已经发生变化，请刷新后重试".into(),
                    ));
                }
            }
        }
        let _permit = self.acquire_request().await?;
        let mut request = self.client.delete_object().bucket(&self.bucket).key(key);
        if !self.is_alibaba_oss() {
            if let Some(etag) = etag {
                request = request.if_match(etag);
            }
        }
        request.send().await.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 object deletion failed"
            );
            AppError::ServiceUnavailable("对象存储删除失败".into())
        })?;
        Ok(())
    }

    pub(super) async fn delete_key_confirmed(
        &self,
        key: &str,
        etag: Option<&str>,
    ) -> AppResult<()> {
        let current = self.head_key(key).await?;
        let Some(current) = current else {
            self.recovery_runtime.journal_settled(key);
            return Ok(());
        };
        if etag.is_some() && current.etag.as_deref() != etag {
            return Err(AppError::ServiceUnavailable(
                "对象存储待删除对象已发生变化，本次未删除".into(),
            ));
        }
        self.delete_key_after_identity_check(key, etag).await
    }

    /// The caller has just verified the object's required identity. Retain the
    /// conditional DELETE (or OSS recheck) and one post-delete absence proof,
    /// without repeating the same pre-delete HEAD in the generic entry point.
    pub(super) async fn delete_key_after_identity_check(
        &self,
        key: &str,
        etag: Option<&str>,
    ) -> AppResult<()> {
        let deletion = self.delete_key(key, etag).await;
        if let Some(current) = self
            .head_key(key)
            .await
            .map_err(|error| error.with_operation(CommitState::Unknown, CleanupState::Pending))?
        {
            let (error, commit) = match deletion {
                Err(error) => {
                    let commit = if etag.is_some() && current.etag.as_deref() == etag {
                        CommitState::NotCommitted
                    } else {
                        CommitState::Unknown
                    };
                    (error, commit)
                }
                Ok(()) => (
                    AppError::ServiceUnavailable("对象存储未能确认对象已经删除".into()),
                    CommitState::Unknown,
                ),
            };
            return Err(error.with_operation(commit, CleanupState::Pending));
        }
        self.recovery_runtime.journal_settled(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
