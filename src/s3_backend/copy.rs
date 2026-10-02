//! Single-file copy orchestration. Shared S3 request primitives remain on the backend.

use super::{committed_cleanup::uncertain_transaction, object_key, S3Backend};
use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    storage::StorageService,
};

impl S3Backend {
    /// Copy without replacing an existing destination. A failed verification
    /// after COPY is accepted returns an unknown outcome unless rollback is confirmed.
    pub async fn copy_file(&self, source: &str, destination: &str) -> AppResult<()> {
        self.copy_file_internal(source, destination, None).await
    }

    /// Also reject source-size changes before COPY; post-COPY outcomes follow `copy_file`.
    pub async fn copy_file_with_expected_size(
        &self,
        source: &str,
        destination: &str,
        expected_size: u64,
    ) -> AppResult<()> {
        self.copy_file_internal(source, destination, Some(expected_size))
            .await
    }

    async fn copy_file_internal(
        &self,
        source: &str,
        destination: &str,
        expected_size: Option<u64>,
    ) -> AppResult<()> {
        let source = StorageService::normalize_relative(source)?;
        let destination = StorageService::normalize_relative(destination)?;
        if source.is_empty() || destination.is_empty() || source == destination {
            return Err(AppError::BadRequest("无效的文件复制路径".into()));
        }
        let backend = self.scoped_work(None, None);
        let _mutation = backend
            .maintenance
            .read(async { Ok(backend.mutation_gate.lock().await) })
            .await?;
        backend
            .copy_file_locked(&source, &destination, expected_size)
            .await
    }

    async fn copy_file_locked(
        &self,
        source: &str,
        destination: &str,
        expected_size: Option<u64>,
    ) -> AppResult<()> {
        let source_metadata = self.metadata(source).await?;
        if source_metadata.is_dir {
            return Err(AppError::Conflict("当前操作只接受普通文件".into()));
        }
        if expected_size.is_some_and(|size| size != source_metadata.size) {
            return Err(AppError::Conflict(
                "Source changed while preparing the copy".into(),
            ));
        }
        self.ensure_parent_directory(destination).await?;
        match self.metadata(destination).await {
            Ok(_) => return Err(AppError::Conflict("目标路径已经存在".into())),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }

        let source_key = object_key(&self.prefix, source)?;
        let destination_key = object_key(&self.prefix, destination)?;
        let copied_etag = self
            .copy_key(
                &source_key,
                &destination_key,
                source_metadata.etag.as_deref(),
                true,
            )
            .await?;

        // Once COPY is accepted, a failed read (including a maintenance pause)
        // cannot prove that the destination was never written.
        let destination_metadata = self
            .metadata(destination)
            .await
            .map_err(uncertain_transaction)?;
        let is_our_copy = destination_metadata.etag.as_ref() == Some(&copied_etag);
        if !destination_metadata.is_dir
            && destination_metadata.size == source_metadata.size
            && is_our_copy
        {
            return Ok(());
        }

        let verification_error = AppError::ServiceUnavailable("对象存储复制结果校验失败".into());
        if !is_our_copy {
            return Err(uncertain_transaction(verification_error));
        }

        // Roll back only the COPY response's ETag and confirm its absence.
        // A failed DELETE is a sub-step failure, not a failed COPY proof.
        self.delete_key_confirmed(&destination_key, Some(&copied_etag))
            .await
            .map_err(uncertain_transaction)?;
        Err(verification_error.with_operation(CommitState::NotCommitted, CleanupState::Complete))
    }
}
