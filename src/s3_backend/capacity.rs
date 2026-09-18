use super::{list_prefix, non_negative_size, S3Backend, S3_MAX_CAPACITY_SCAN_PAGES, S3_PAGE_SIZE};
use crate::error::{AppError, AppResult};

impl S3Backend {
    /// Sum the user namespace once when a backend is prepared. S3 has no
    /// portable bucket-capacity API, so logical quota accounting is scoped to
    /// Ycloud's configured prefix and excludes its reserved transaction area.
    pub async fn user_data_size(&self) -> AppResult<u64> {
        self.sum_object_bytes(&self.prefix, true, None).await
    }

    pub async fn path_size(&self, relative: &str) -> AppResult<u64> {
        let metadata = self.metadata(relative).await?;
        if !metadata.is_dir {
            return Ok(metadata.size);
        }
        let prefix = list_prefix(&self.prefix, relative)?;
        self.sum_object_bytes(&prefix, false, None).await
    }

    pub(crate) async fn directory_size(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<u64> {
        if !self.metadata(relative).await?.is_dir {
            return Err(AppError::NotFound);
        }
        let prefix = list_prefix(&self.prefix, relative)?;
        self.sum_object_bytes(&prefix, true, Some(max_entries))
            .await
    }

    async fn sum_object_bytes(
        &self,
        prefix: &str,
        exclude_internal: bool,
        max_entries: Option<usize>,
    ) -> AppResult<u64> {
        let internal_prefix = format!("{}.ycloud-system/", self.prefix);
        let mut continuation_token: Option<String> = None;
        let mut total = 0_u64;
        let mut count = 0_usize;
        for _ in 0..S3_MAX_CAPACITY_SCAN_PAGES {
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .max_keys(i32::try_from(S3_PAGE_SIZE).unwrap_or(1_000));
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 capacity scan failed"
                );
                AppError::ServiceUnavailable("无法统计对象存储已用容量".into())
            })?;
            drop(permit);

            for object in output.contents() {
                count += 1;
                if max_entries.is_some_and(|max| count > max) {
                    return Err(AppError::BadRequest(
                        "目录条目过多，请选择较小的子目录计算".into(),
                    ));
                }
                let key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储容量统计遇到无键名对象".into())
                })?;
                if exclude_internal && key.starts_with(&internal_prefix) {
                    continue;
                }
                total = total
                    .checked_add(non_negative_size(object.size())?)
                    .ok_or_else(|| AppError::internal("S3 storage usage exceeds u64"))?;
            }
            if !output.is_truncated().unwrap_or(false) {
                return Ok(total);
            }
            let next = output.next_continuation_token().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储容量统计分页结果无效".into())
            })?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储容量统计分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        Err(AppError::ServiceUnavailable(
            "对象存储对象数量超过 Ycloud 容量统计安全上限".into(),
        ))
    }
}
