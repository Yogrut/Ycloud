//! Read-only, bounded directory manifests prepared before any transaction write.

use super::record::{ObjectRecord, Operation, MAX_OBJECTS, MAX_SINGLE_COPY_BYTES, TRASH_CATEGORY};
use crate::{
    error::{AppError, AppResult},
    s3_backend::{
        internal_key, list_prefix, non_negative_size, S3Backend, S3_MAX_LIST_PAGES, S3_PAGE_SIZE,
    },
};

fn directory_object_limit_error() -> AppError {
    AppError::Conflict(format!("目录包含超过 {MAX_OBJECTS} 个对象，超出单次安全变更上限").into())
}

impl S3Backend {
    pub(super) async fn snapshot_objects(
        &self,
        id: &str,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
    ) -> AppResult<Vec<ObjectRecord>> {
        self.maintenance
            .read(self.snapshot_objects_uninterrupted(id, operation, source, destination))
            .await
    }

    async fn snapshot_objects_uninterrupted(
        &self,
        id: &str,
        operation: Operation,
        source: &str,
        destination: Option<&str>,
    ) -> AppResult<Vec<ObjectRecord>> {
        let source_prefix = list_prefix(&self.prefix, source)?;
        let target_prefix = match operation {
            Operation::Copy | Operation::Move => list_prefix(
                &self.prefix,
                destination.ok_or_else(|| AppError::BadRequest("目录事务缺少目标路径".into()))?,
            )?,
            Operation::Delete => internal_key(&self.prefix, TRASH_CATEGORY, &format!("{id}/")),
        };
        let mut continuation_token: Option<String> = None;
        let mut objects = Vec::new();
        let mut listing_complete = false;
        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = MAX_OBJECTS.saturating_add(1).saturating_sub(objects.len());
            if remaining == 0 {
                return Err(directory_object_limit_error());
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 transaction list page size"))?;
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&source_prefix)
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory transaction listing failed"
                );
                AppError::ServiceUnavailable("无法读取待变更的对象存储目录".into())
            })?;
            drop(permit);

            for object in output.contents() {
                if objects.len() == MAX_OBJECTS {
                    return Err(directory_object_limit_error());
                }
                let source_key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录包含无键对象".into())
                })?;
                let suffix = source_key.strip_prefix(&source_prefix).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录列举结果越出源前缀".into())
                })?;
                let source_etag = object.e_tag().map(str::to_owned).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储目录对象缺少 ETag，无法安全变更".into())
                })?;
                let size = non_negative_size(object.size())?;
                if size > MAX_SINGLE_COPY_BYTES {
                    return Err(AppError::Conflict(
                        "目录包含超过 5 GB 的对象；当前目录操作上限为单个对象 5 GB".into(),
                    ));
                }
                objects.push(ObjectRecord {
                    source_key: source_key.to_owned(),
                    target_key: format!("{target_prefix}{suffix}"),
                    size,
                    source_etag,
                    target_etag: None,
                    source_deleted: false,
                });
            }

            if !output.is_truncated().unwrap_or(false) {
                listing_complete = true;
                break;
            }
            let next = output.next_continuation_token().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储目录分页结果缺少继续令牌".into())
            })?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储目录分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        if !listing_complete {
            return Err(AppError::ServiceUnavailable(
                "对象存储目录分页超过安全页数上限".into(),
            ));
        }
        if objects.is_empty() {
            return Err(AppError::Conflict(
                "隐式空目录没有可安全变更的目录标记".into(),
            ));
        }
        Ok(objects)
    }
}

#[cfg(test)]
mod tests;
