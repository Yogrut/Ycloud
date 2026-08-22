use std::{
    collections::BTreeMap,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use aws_sdk_s3::{
    config::{retry::RetryConfig, timeout::TimeoutConfig, BehaviorVersion, Credentials, Region},
    types::{CommonPrefix, Object},
    Client,
};
use aws_smithy_http_client::Builder as HttpClientBuilder;
use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
};
use bytes::Bytes;
use futures_util::Stream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;

use crate::{
    config::{
        validate_storage_backend, Config, S3AddressingStyle, S3StorageConfig, StorageBackendConfig,
    },
    error::{AppError, AppResult},
    storage::{
        attachment_header, content_type_for_mode, parse_range, FileResponseMode, StorageService,
    },
};

const S3_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const S3_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(8);
const S3_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const S3_MAX_ATTEMPTS: u32 = 2;
const S3_MAX_IDLE_CONNECTIONS_PER_HOST: usize = 8;
const S3_IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const S3_REQUEST_CONCURRENCY: usize = 8;
const S3_PAGE_SIZE: usize = 1_000;
const S3_MAX_LIST_ENTRIES: usize = 10_000;
const S3_MAX_LIST_PAGES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Entry {
    pub name: String,
    pub relative: String,
    pub is_dir: bool,
    pub size: u64,
    pub last_modified: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3ListResult {
    pub entries: Vec<S3Entry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3Metadata {
    pub relative: String,
    pub is_dir: bool,
    pub size: u64,
    pub last_modified: Option<i64>,
    pub content_type: Option<String>,
    etag: Option<String>,
}

/// One reusable S3 client and its confined namespace.
///
/// Provider presets are validated before construction and all object keys are
/// rooted below `prefix`. The client never retains a second plaintext copy of
/// credentials outside the AWS credential provider.
#[derive(Clone)]
pub struct S3Backend {
    client: Client,
    bucket: String,
    prefix: String,
    request_gate: std::sync::Arc<Semaphore>,
}

impl S3Backend {
    pub fn new(settings: &S3StorageConfig, runtime: &Config) -> AppResult<Self> {
        validate_storage_backend(&StorageBackendConfig::S3(settings.clone()))?;
        runtime.allows_storage_backend(&StorageBackendConfig::S3(settings.clone()))?;

        let credentials = Credentials::new(
            settings.access_key_id.clone(),
            settings.secret_access_key.clone(),
            None,
            None,
            "ycloud-static-configuration",
        );
        let timeouts = TimeoutConfig::builder()
            .connect_timeout(S3_CONNECT_TIMEOUT)
            .operation_attempt_timeout(S3_ATTEMPT_TIMEOUT)
            .operation_timeout(S3_OPERATION_TIMEOUT)
            .build();
        let mut sdk_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(Region::new(settings.region.clone()))
            .endpoint_url(settings.endpoint.clone())
            .force_path_style(settings.addressing_style == S3AddressingStyle::Path)
            .disable_multi_region_access_points(true)
            .disable_s3_express_session_auth(true)
            .retry_config(RetryConfig::standard().with_max_attempts(S3_MAX_ATTEMPTS))
            .timeout_config(timeouts);

        // Private MinIO/RustFS deployments commonly use HTTP behind an
        // encrypted EasyTier network. Such endpoints do not need a TLS stack
        // and must not depend on the developer machine's certificate store.
        // HTTPS endpoints continue to use the SDK's verified Rustls client.
        if settings.endpoint.starts_with("http://") {
            let http_client = HttpClientBuilder::new()
                .pool_idle_timeout(S3_IDLE_CONNECTION_TIMEOUT)
                .pool_max_idle_per_host(S3_MAX_IDLE_CONNECTIONS_PER_HOST)
                .build_http();
            sdk_config = sdk_config.http_client(http_client);
        }

        let sdk_config = sdk_config.build();

        Ok(Self {
            client: Client::from_conf(sdk_config),
            bucket: settings.bucket.clone(),
            prefix: settings.prefix.clone(),
            request_gate: std::sync::Arc::new(Semaphore::new(S3_REQUEST_CONCURRENCY)),
        })
    }

    /// Verify the minimum permission required by the file browser. The error
    /// returned to callers is intentionally generic so upstream SDK responses
    /// cannot leak credentials or signed request details into the UI.
    pub async fn probe(&self) -> AppResult<()> {
        let _permit = self.acquire_request().await?;
        self.client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&self.prefix)
            .max_keys(1)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(error_kind = %error.as_service_error().map_or("transport", |_| "service"), "S3 storage probe failed");
                AppError::ServiceUnavailable(
                    "无法连接对象存储或当前凭据缺少列举权限".into(),
                )
            })?;
        Ok(())
    }

    pub fn object_key(&self, relative: &str) -> AppResult<String> {
        object_key(&self.prefix, relative)
    }

    pub fn list_prefix(&self, relative: &str) -> AppResult<String> {
        list_prefix(&self.prefix, relative)
    }

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

    /// Resolve an object or an emulated directory prefix without allowing an
    /// ambiguous `name` object and `name/` directory to masquerade as one path.
    pub async fn metadata(&self, relative: &str) -> AppResult<S3Metadata> {
        let relative = StorageService::normalize_relative(relative)?;
        if relative.is_empty() {
            return Ok(directory_metadata(relative));
        }
        let key = object_key(&self.prefix, &relative)?;
        let directory_prefix = list_prefix(&self.prefix, &relative)?;
        let _permit = self.acquire_request().await?;

        let file = match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(output) => Some(S3Metadata {
                relative: relative.clone(),
                is_dir: false,
                size: non_negative_size(output.content_length())?,
                last_modified: output.last_modified().map(|value| value.secs()),
                content_type: output.content_type().map(str::to_owned),
                etag: output.e_tag().map(str::to_owned),
            }),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_not_found()) =>
            {
                None
            }
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 object metadata request failed"
                );
                return Err(AppError::ServiceUnavailable(
                    "无法读取对象存储元数据".into(),
                ));
            }
        };

        let directory_exists = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(directory_prefix)
            .max_keys(1)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 directory metadata request failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储元数据".into())
            })?
            .key_count()
            .unwrap_or(0)
            > 0;

        match (file, directory_exists) {
            (Some(_), true) => Err(AppError::Conflict(
                "对象存储中同一路径同时存在文件和目录前缀".into(),
            )),
            (Some(file), false) => Ok(file),
            (None, true) => Ok(directory_metadata(relative)),
            (None, false) => Err(AppError::NotFound),
        }
    }

    /// Stream one immutable view of an object. `If-Match` ties GET to the
    /// metadata used for Range validation, preventing a replacement between
    /// HEAD and GET from producing inconsistent length or range headers.
    pub async fn stream_file(
        &self,
        relative: &str,
        request_headers: &HeaderMap,
        mode: FileResponseMode,
    ) -> AppResult<Response<Body>> {
        let metadata = self.metadata(relative).await?;
        if metadata.is_dir {
            return Err(AppError::NotFound);
        }
        let range = parse_range(request_headers, metadata.size);
        let (start, length, status) = match range {
            Ok(Some(range)) => range,
            Ok(None) => (0, metadata.size, StatusCode::OK),
            Err(()) => return range_not_satisfiable(metadata.size),
        };
        let key = object_key(&self.prefix, &metadata.relative)?;
        let permit = self.acquire_request().await?;
        let mut request = self.client.get_object().bucket(&self.bucket).key(key);
        if let Some(etag) = metadata.etag.as_deref() {
            request = request.if_match(etag);
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            request = request.range(format!("bytes={start}-{end}"));
        }
        let output = request.send().await.map_err(|error| {
            tracing::warn!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 object download failed"
            );
            AppError::ServiceUnavailable("对象已发生变化、无权读取或对象存储暂不可用".into())
        })?;
        let response_length = non_negative_size(output.content_length())?;
        if response_length != length {
            return Err(AppError::ServiceUnavailable(
                "对象存储返回了不一致的内容长度".into(),
            ));
        }

        let reader = output.body.into_async_read();
        let stream = PermitStream {
            inner: ReaderStream::new(reader),
            _permit: permit,
        };
        let mut response = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
        let guessed = mime_guess::from_path(&metadata.relative).first_or_octet_stream();
        let (mut content_type, force_attachment) = content_type_for_mode(&guessed, mode);
        if matches!(
            mode,
            FileResponseMode::Attachment | FileResponseMode::WebDav
        ) {
            if let Some(value) = metadata
                .content_type
                .as_deref()
                .and_then(|value| HeaderValue::from_str(value).ok())
            {
                content_type = value;
            }
        }
        response = response.header(header::CONTENT_TYPE, content_type);
        if matches!(mode, FileResponseMode::Attachment) || force_attachment {
            response = response.header(
                header::CONTENT_DISPOSITION,
                attachment_header(Path::new(&metadata.relative)),
            );
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            response = response.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", metadata.size),
            );
        }
        response
            .body(Body::from_stream(stream))
            .map_err(|error| AppError::with_source("failed to build S3 response", error))
    }

    async fn acquire_request(&self) -> AppResult<OwnedSemaphorePermit> {
        self.request_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("对象存储正在关闭".into()))
    }
}

fn collect_page_entries(
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

fn directory_metadata(relative: String) -> S3Metadata {
    S3Metadata {
        relative,
        is_dir: true,
        size: 0,
        last_modified: None,
        content_type: None,
        etag: None,
    }
}

fn non_negative_size(size: Option<i64>) -> AppResult<u64> {
    size.ok_or_else(|| AppError::ServiceUnavailable("对象存储未返回内容长度".into()))?
        .try_into()
        .map_err(|_| AppError::ServiceUnavailable("对象存储返回了无效的内容长度".into()))
}

fn range_not_satisfiable(total_length: u64) -> AppResult<Response<Body>> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{total_length}"))
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .map_err(|error| AppError::with_source("failed to build S3 range response", error))
}

struct PermitStream<S> {
    inner: S,
    _permit: OwnedSemaphorePermit,
}

impl<S> Stream for PermitStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(context)
    }
}

fn object_key(prefix: &str, relative: &str) -> AppResult<String> {
    let relative = StorageService::normalize_relative(relative)?;
    if relative.is_empty() {
        return Err(AppError::BadRequest("对象路径不能是存储根目录".into()));
    }
    Ok(format!("{prefix}{relative}"))
}

fn list_prefix(prefix: &str, relative: &str) -> AppResult<String> {
    let relative = StorageService::normalize_relative(relative)?;
    if relative.is_empty() {
        return Ok(prefix.to_owned());
    }
    Ok(format!("{prefix}{relative}/"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use aws_sdk_s3::types::{CommonPrefix, Object};

    use super::{collect_page_entries, list_prefix, object_key};

    #[test]
    fn object_keys_are_confined_below_the_configured_prefix() {
        let prefix = "users/yogrut/";
        assert_eq!(
            object_key(prefix, "documents/report.pdf").unwrap(),
            "users/yogrut/documents/report.pdf"
        );
        assert_eq!(
            list_prefix(prefix, "documents").unwrap(),
            "users/yogrut/documents/"
        );
        assert_eq!(list_prefix(prefix, "/").unwrap(), "users/yogrut/");
        assert!(object_key(prefix, "../outside").is_err());
        assert!(object_key(prefix, ".ycloud-system/journal").is_err());
    }

    #[test]
    fn listing_only_exposes_direct_children_below_the_prefix() {
        let prefixes = vec![CommonPrefix::builder()
            .prefix("users/yogrut/photos/")
            .build()];
        let objects = vec![
            Object::builder()
                .key("users/yogrut/report.pdf")
                .size(42)
                .build(),
            Object::builder()
                .key("users/yogrut/nested/hidden.txt")
                .size(8)
                .build(),
            Object::builder().key("outside/secret.txt").size(12).build(),
        ];
        let mut entries = BTreeMap::new();
        collect_page_entries("", "users/yogrut/", &prefixes, &objects, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.get("photos").unwrap().is_dir);
        assert_eq!(entries.get("report.pdf").unwrap().size, 42);
        assert!(!entries.contains_key("secret.txt"));
        assert!(!entries.contains_key("hidden.txt"));
    }

    #[test]
    fn listing_rejects_file_and_directory_name_collisions() {
        let prefixes = vec![CommonPrefix::builder()
            .prefix("users/yogrut/archive/")
            .build()];
        let objects = vec![Object::builder()
            .key("users/yogrut/archive")
            .size(1)
            .build()];
        let mut entries = BTreeMap::new();
        assert!(
            collect_page_entries("", "users/yogrut/", &prefixes, &objects, &mut entries,).is_err()
        );
    }

    #[test]
    fn listing_hides_reserved_system_names() {
        let prefixes = vec![CommonPrefix::builder()
            .prefix("users/yogrut/.ycloud-system/")
            .build()];
        let mut entries = BTreeMap::new();
        collect_page_entries("", "users/yogrut/", &prefixes, &[], &mut entries).unwrap();
        assert!(entries.is_empty());
    }
}
