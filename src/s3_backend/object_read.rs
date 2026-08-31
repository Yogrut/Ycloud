use std::path::Path;

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
};
use tokio_util::io::ReaderStream;

use super::{
    directory_metadata, list_prefix, non_negative_size, object_key, range_not_satisfiable,
    PermitStream, S3Backend, S3Metadata,
};
use crate::{
    error::{AppError, AppResult},
    storage::{
        attachment_header, content_type_for_mode, parse_range, FileResponseMode, StorageService,
    },
};

impl S3Backend {
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
}
