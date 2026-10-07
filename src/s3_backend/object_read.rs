use std::path::Path;

use axum::{
    body::Body,
    http::{header, HeaderMap, Method, Response, StatusCode},
};
use tokio_util::io::ReaderStream;

use super::{
    capabilities, directory_metadata, list_prefix, non_negative_size, object_key,
    range_not_satisfiable, PermitStream, S3Backend, S3Metadata,
};
use crate::{
    error::{AppError, AppResult},
    storage::{FileResponseMode, FileResponsePolicy, FileValidators, StorageService},
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
        method: &Method,
    ) -> AppResult<Response<Body>> {
        let metadata = self.metadata(relative).await?;
        if metadata.is_dir {
            return Err(AppError::NotFound);
        }
        let validators = FileValidators::object(metadata.etag.as_deref(), metadata.last_modified);
        let range = validators.select_range(request_headers, metadata.size, method);
        let (start, length, status) = match range {
            Ok(Some(range)) => range,
            Ok(None) => (0, metadata.size, StatusCode::OK),
            Err(()) => return range_not_satisfiable(metadata.size),
        };
        let mut response = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length);
        response = FileResponsePolicy::new(
            Path::new(&metadata.relative),
            metadata.content_type.as_deref(),
            mode,
        )
        .apply(response);
        response = validators.apply(response);
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            response = response.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", metadata.size),
            );
        }
        if method == Method::HEAD {
            return response
                .body(Body::empty())
                .map_err(|error| AppError::with_source("failed to build S3 response", error));
        }
        let key = object_key(&self.prefix, &metadata.relative)?;
        let stream_permit = self
            .stream_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;
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
            if status == StatusCode::PARTIAL_CONTENT {
                AppError::storage_capability(
                    capabilities::RANGE_READ,
                    "对象存储不支持当前范围读取或暂不可用",
                )
            } else {
                AppError::ServiceUnavailable("对象已发生变化、无权读取或对象存储暂不可用".into())
            }
        })?;
        let response_length = non_negative_size(output.content_length())?;
        if response_length != length {
            return Err(if status == StatusCode::PARTIAL_CONTENT {
                AppError::storage_capability(
                    capabilities::RANGE_READ,
                    "对象存储范围读取返回了不一致的内容长度",
                )
            } else {
                AppError::ServiceUnavailable("对象存储返回了不一致的内容长度".into())
            });
        }

        drop(permit);
        let reader = output.body.into_async_read();
        let stream = PermitStream {
            inner: ReaderStream::new(reader),
            _permit: stream_permit,
        };
        response
            .body(Body::from_stream(stream))
            .map_err(|error| AppError::with_source("failed to build S3 response", error))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        body::to_bytes,
        http::{HeaderValue, Uri},
        routing::any,
        Router,
    };
    use tokio::net::TcpListener;

    use super::*;
    use crate::s3_backend::protocol_tests::test_backend;

    #[tokio::test]
    async fn head_skips_object_body_and_conditional_ranges_pin_the_current_version() {
        let reads = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let observed = reads.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri, headers: HeaderMap| {
                let reads = observed.clone();
                async move {
                    if uri.path() == "/bucket" || uri.path() == "/bucket/" {
                        return Response::builder().header(header::CONTENT_TYPE, "application/xml")
                            .body(Body::from("<ListBucketResult><KeyCount>0</KeyCount><IsTruncated>false</IsTruncated></ListBucketResult>"))
                            .unwrap();
                    }
                    assert_eq!(uri.path(), "/bucket/tenant/file.bin");
                    let mut response = Response::builder()
                        .header(header::ETAG, "\"current\"")
                        .header(header::LAST_MODIFIED, "Tue, 14 Nov 2023 22:13:20 GMT");
                    if method == Method::HEAD {
                        return response.header(header::CONTENT_LENGTH, 10)
                            .body(Body::empty()).unwrap();
                    }
                    assert_eq!(method, Method::GET);
                    assert_eq!(headers[header::IF_MATCH], "\"current\"");
                    let range = headers.get(header::RANGE)
                        .map(|value| value.to_str().unwrap().to_owned());
                    reads.lock().unwrap().push(range.clone());
                    let payload = if let Some(range) = range {
                        assert_eq!(range, "bytes=2-4");
                        response = response.status(StatusCode::PARTIAL_CONTENT)
                            .header(header::CONTENT_RANGE, "bytes 2-4/10");
                        "234"
                    } else {
                        "0123456789"
                    };
                    response.header(header::CONTENT_LENGTH, payload.len())
                        .body(Body::from(payload)).unwrap()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = test_backend(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-4"));
        headers.insert(header::IF_RANGE, HeaderValue::from_static("\"current\""));
        let response = backend
            .stream_file(
                "file.bin",
                &headers,
                FileResponseMode::WebDav,
                &Method::HEAD,
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(response.headers()[header::ETAG], "\"current\"");
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        assert!(to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty());
        assert!(reads.lock().unwrap().is_empty());
        for (expected, status, payload) in [
            ("\"current\"", StatusCode::PARTIAL_CONTENT, "234"),
            ("\"previous\"", StatusCode::OK, "0123456789"),
            ("W/\"current\"", StatusCode::OK, "0123456789"),
            (
                "Tue, 14 Nov 2023 22:13:20 GMT",
                StatusCode::OK,
                "0123456789",
            ),
        ] {
            headers.insert(header::IF_RANGE, HeaderValue::from_str(expected).unwrap());
            let response = backend
                .stream_file("file.bin", &headers, FileResponseMode::WebDav, &Method::GET)
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                payload.len().to_string()
            );
            assert_eq!(response.headers()[header::ETAG], "\"current\"");
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
                payload.as_bytes()
            );
        }
        assert_eq!(
            *reads.lock().unwrap(),
            [Some("bytes=2-4".into()), None, None, None]
        );
        server.abort();
    }
}
