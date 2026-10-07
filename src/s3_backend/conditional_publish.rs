//! First-use verification is confined to the guarded upload's owned empty backup.
//! The upload journal remains in CheckingPublication until this work finishes.
use std::sync::atomic::Ordering;

use aws_sdk_s3::types::MetadataDirective;
use axum::body::Body;

use super::{capabilities, copy_source, S3Backend, S3UploadTransaction, S3_OPERATION_METADATA_KEY};
use crate::error::{AppError, AppResult, CleanupState};

impl S3Backend {
    pub(super) async fn cleanup_uncommitted_guarded_upload(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        transaction: &S3UploadTransaction,
        intent_key: &str,
        intent_etag: Option<&str>,
    ) -> CleanupState {
        // Stage writes finish before this grace period starts. Only owned
        // temporary garbage may be deferred; recovery keeps its durable owner.
        match tokio::time::timeout(
            super::committed_cleanup::FOREGROUND_CLEANUP_BUDGET,
            self.finish_upload_and_intent(
                journal_key,
                journal_etag,
                transaction,
                intent_key,
                intent_etag,
            ),
        )
        .await
        {
            Ok(Ok(())) => CleanupState::Complete,
            _ => CleanupState::Pending,
        }
    }
    pub(super) async fn finish_guarded_upload_resources(
        &self,
        transaction: &S3UploadTransaction,
    ) -> AppResult<()> {
        for (category, size) in [("uploads", transaction.temporary.size), ("backups", 0)] {
            let key = super::internal_key(&self.prefix, category, &transaction.id);
            if let Some(object) = self.head_key(&key).await? {
                if object.size != size
                    || object.operation_id.as_deref() != Some(transaction.id.as_str())
                {
                    return Err(AppError::ServiceUnavailable(
                        "条件上传临时对象归属无法核验，已保留数据".into(),
                    ));
                }
                let etag = object.etag.as_deref().ok_or_else(unavailable)?;
                self.delete_key_confirmed(&key, Some(etag)).await?;
            }
        }
        Ok(())
    }
    // Caller holds mutation_gate; clones share this one client-lifetime result.
    pub(super) async fn verify_conditional_publish(
        &self,
        transaction: &S3UploadTransaction,
        key: &str,
    ) -> AppResult<()> {
        if self.conditional_publish_verified.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self.head_key(key).await?.is_some() {
            return Err(unavailable());
        }
        self.single_upload(key, Body::empty(), 0, None, &transaction.id)
            .await?;
        let object = self
            .head_key(key)
            .await?
            .filter(|value| {
                value.size == 0 && value.operation_id.as_deref() == Some(transaction.id.as_str())
            })
            .ok_or_else(unavailable)?;
        let etag = object.etag.as_deref().ok_or_else(unavailable)?;
        let candidate = format!("\"ycloud-condition-{}\"", transaction.id);
        let mismatch = if etag == candidate {
            "\"ycloud-condition-different\""
        } else {
            candidate.as_str()
        };
        for create in [false, true] {
            let _permit = self.acquire_request().await?;
            let request = self
                .client
                .copy_object()
                .bucket(&self.bucket)
                .copy_source(copy_source(&self.bucket, key))
                .key(key)
                .metadata_directive(MetadataDirective::Replace)
                .metadata(S3_OPERATION_METADATA_KEY, &transaction.id)
                .metadata("ycloud-conditional-probe", "verification");
            let result = if create {
                request.if_none_match("*")
            } else {
                request.if_match(mismatch)
            }
            .send()
            .await;
            match result {
                Err(error)
                    if error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 412) => {}
                _ => return Err(unavailable()),
            }
        }
        self.delete_key_confirmed(key, Some(etag)).await?;
        self.conditional_publish_verified
            .store(true, Ordering::Relaxed);
        Ok(())
    }
}

fn unavailable() -> AppError {
    AppError::storage_capability(
        capabilities::CONDITIONAL_FILE_PUBLISH,
        "对象存储未通过原子条件发布核验，已拒绝条件上传且未改写正式文件",
    )
}

#[cfg(test)]
mod tests;
