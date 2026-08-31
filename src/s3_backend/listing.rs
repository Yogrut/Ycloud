use std::collections::BTreeMap;

use aws_sdk_s3::types::{CommonPrefix, Object};

use super::{
    list_prefix, non_negative_size, S3Backend, S3Entry, S3ListResult, S3_MAX_CAPACITY_SCAN_PAGES,
    S3_MAX_LIST_ENTRIES, S3_MAX_LIST_PAGES, S3_PAGE_SIZE,
};
use crate::{
    error::{AppError, AppResult},
    storage::StorageService,
};

impl S3Backend {
    /// List direct children only. Pagination, result count and concurrent S3
    /// requests are bounded so a hostile or unexpectedly large bucket cannot
    /// monopolize a small VPS.
    pub async fn list_directory(
        &self,
        relative: &str,
        max_entries: usize,
    ) -> AppResult<S3ListResult> {
        let relative = StorageService::normalize_relative(relative)?;
        let directory_prefix = list_prefix(&self.prefix, &relative)?;
        let limit = max_entries.clamp(1, S3_MAX_LIST_ENTRIES);
        let target = limit.saturating_add(1);
        let _permit = self.acquire_request().await?;
        let mut continuation_token = None;
        let mut entries = BTreeMap::new();
        let mut remote_truncated = false;

        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = target.saturating_sub(entries.len());
            if remaining == 0 {
                break;
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 list page size"))?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&directory_prefix)
                .delimiter("/")
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory listing failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储目录".into())
            })?;

            collect_page_entries(
                &relative,
                &directory_prefix,
                output.common_prefixes(),
                output.contents(),
                &mut entries,
            )?;
            remote_truncated = output.is_truncated().unwrap_or(false);
            if !remote_truncated {
                break;
            }
            let Some(next) = output.next_continuation_token() else {
                return Err(AppError::ServiceUnavailable(
                    "对象存储返回了无效的分页结果".into(),
                ));
            };
            continuation_token = Some(next.to_owned());
        }

        let truncated = remote_truncated || entries.len() > limit;
        let entries = entries.into_values().take(limit).collect();
        Ok(S3ListResult { entries, truncated })
    }

    /// Stream all direct children through a bounded consumer. The caller can
    /// implement exact sorting and cursor pagination without retaining the
    /// complete directory in memory.
    pub async fn scan_directory_entries<F>(&self, relative: &str, mut consume: F) -> AppResult<()>
    where
        F: FnMut(S3Entry),
    {
        let relative = StorageService::normalize_relative(relative)?;
        let directory_prefix = list_prefix(&self.prefix, &relative)?;
        let _permit = self.acquire_request().await?;
        let mut continuation_token: Option<String> = None;

        for _ in 0..S3_MAX_CAPACITY_SCAN_PAGES {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&directory_prefix)
                .delimiter("/")
                .max_keys(i32::try_from(S3_PAGE_SIZE).unwrap_or(1_000));
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory scan failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储目录".into())
            })?;
            let mut page = BTreeMap::new();
            collect_page_entries(
                &relative,
                &directory_prefix,
                output.common_prefixes(),
                output.contents(),
                &mut page,
            )?;
            page.into_values().for_each(&mut consume);

            if !output.is_truncated().unwrap_or(false) {
                return Ok(());
            }
            let next = output.next_continuation_token().ok_or_else(|| {
                AppError::ServiceUnavailable("对象存储返回了无效的分页结果".into())
            })?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储目录分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        Err(AppError::ServiceUnavailable(
            "对象存储目录超过单次安全扫描上限".into(),
        ))
    }
}

pub(super) fn collect_page_entries(
    parent: &str,
    directory_prefix: &str,
    common_prefixes: &[CommonPrefix],
    objects: &[Object],
    entries: &mut BTreeMap<String, S3Entry>,
) -> AppResult<()> {
    for prefix in common_prefixes {
        let Some(prefix) = prefix.prefix() else {
            continue;
        };
        let Some(name) = direct_child_name(directory_prefix, prefix, true) else {
            continue;
        };
        insert_safe_child(entries, parent, name, true, 0, None)?;
    }
    for object in objects {
        let Some(key) = object.key() else {
            continue;
        };
        if key == directory_prefix {
            continue;
        }
        let is_dir = key.ends_with('/');
        let Some(name) = direct_child_name(directory_prefix, key, is_dir) else {
            continue;
        };
        let size = if is_dir {
            0
        } else {
            non_negative_size(object.size())?
        };
        let modified = object.last_modified().map(|value| value.secs());
        insert_safe_child(entries, parent, name, is_dir, size, modified)?;
    }
    Ok(())
}

fn direct_child_name<'a>(directory_prefix: &str, key: &'a str, is_dir: bool) -> Option<&'a str> {
    let suffix = key.strip_prefix(directory_prefix)?;
    let suffix = if is_dir {
        suffix.strip_suffix('/')?
    } else {
        suffix
    };
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    Some(suffix)
}

fn child_entry(
    parent: &str,
    name: &str,
    is_dir: bool,
    size: u64,
    last_modified: Option<i64>,
) -> AppResult<S3Entry> {
    if matches!(name, "" | "." | "..") {
        return Err(AppError::BadRequest("Invalid storage path".into()));
    }
    let relative = if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    };
    let relative = StorageService::normalize_relative(&relative)?;
    Ok(S3Entry {
        name: name.to_owned(),
        relative,
        is_dir,
        size,
        last_modified,
    })
}

fn insert_safe_child(
    entries: &mut BTreeMap<String, S3Entry>,
    parent: &str,
    name: &str,
    is_dir: bool,
    size: u64,
    last_modified: Option<i64>,
) -> AppResult<()> {
    match child_entry(parent, name, is_dir, size, last_modified) {
        Ok(entry) => insert_entry(entries, entry),
        Err(AppError::BadRequest(_)) => {
            tracing::warn!("skipping invalid S3 object key below configured prefix");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn insert_entry(entries: &mut BTreeMap<String, S3Entry>, entry: S3Entry) -> AppResult<()> {
    if let Some(existing) = entries.get(&entry.name) {
        if existing.is_dir != entry.is_dir {
            return Err(AppError::Conflict(
                "对象存储中同一路径同时存在文件和目录前缀".into(),
            ));
        }
        return Ok(());
    }
    entries.insert(entry.name.clone(), entry);
    Ok(())
}
