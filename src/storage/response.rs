use std::{
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    body::Body,
    http::{
        header::{self, HeaderMap, HeaderValue},
        Method, Response, StatusCode,
    },
};
use bytes::Bytes;
use futures_util::Stream;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::OwnedSemaphorePermit,
};
use tokio_util::io::ReaderStream;

use super::{FileValidators, ResolvedPath, StorageService};
use crate::error::{AppError, AppResult};

// A bounded read amortizes file/thread-pool and metering work at VPS speeds.
const DOWNLOAD_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum FileResponseMode {
    Attachment,
    Preview,
    WebDav,
}

impl StorageService {
    pub async fn stream_file(
        &self,
        path: &ResolvedPath,
        request_headers: &HeaderMap,
        mode: FileResponseMode,
        method: &Method,
    ) -> AppResult<Response<Body>> {
        let permit = self
            .stream_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;
        let file = self.open_file_for_read(path).await?;
        self.response_from_open_file(file, path, request_headers, mode, method, permit)
            .await
    }

    async fn response_from_open_file(
        &self,
        mut file: tokio::fs::File,
        path: &ResolvedPath,
        request_headers: &HeaderMap,
        mode: FileResponseMode,
        method: &Method,
        permit: OwnedSemaphorePermit,
    ) -> AppResult<Response<Body>> {
        let metadata = file.metadata().await.map_err(|error| {
            AppError::with_source("failed to inspect opened download file", error)
        })?;
        if !metadata.is_file() {
            return Err(AppError::NotFound);
        }

        let total_length = metadata.len();
        let validators = FileValidators::local(&metadata);
        if let Some(response) = validators.precondition_response(request_headers, method)? {
            return Ok(response);
        }
        let range = validators.select_range(request_headers, total_length, method);
        let (start, length, status) = match range {
            Ok(Some(range)) => range,
            Ok(None) => (0, total_length, StatusCode::OK),
            Err(()) => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total_length}"))
                    .header(header::CONTENT_LENGTH, 0)
                    .body(Body::empty())
                    .map_err(|error| {
                        AppError::with_source("failed to build range response", error)
                    });
            }
        };
        if start > 0 {
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|error| AppError::with_source("failed to seek file", error))?;
        }

        let mut response = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length);
        response = FileResponsePolicy::new(path.absolute(), None, mode).apply(response);
        response = validators.apply(response);
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            response = response.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total_length}"),
            );
        }

        let body = if method == Method::HEAD {
            Body::empty()
        } else {
            Body::from_stream(PermitStream {
                inner: ReaderStream::with_capacity(file.take(length), DOWNLOAD_BUFFER_BYTES),
                _permit: permit,
            })
        };
        response
            .body(body)
            .map_err(|error| AppError::with_source("failed to build file response", error))
    }
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

/// The complete policy for untrusted file bodies. Backends must not override
/// individual headers after applying it (including with S3 object metadata).
pub(crate) struct FileResponsePolicy {
    content_type: HeaderValue,
    disposition: Option<HeaderValue>,
}

impl FileResponsePolicy {
    pub(crate) fn new(path: &Path, stored_type: Option<&str>, mode: FileResponseMode) -> Self {
        let guessed = mime_guess::from_path(path).first_or_octet_stream();
        let (content_type, attachment) = match mode {
            // DAV clients still receive the original bytes and media type.
            // Browser navigation must treat these resources as downloads.
            FileResponseMode::Attachment | FileResponseMode::WebDav => (
                stored_type
                    .and_then(|value| value.parse::<mime_guess::Mime>().ok())
                    .and_then(|value| HeaderValue::from_str(value.as_ref()).ok())
                    .unwrap_or_else(|| {
                        HeaderValue::from_str(guessed.as_ref()).unwrap_or_else(|_| {
                            HeaderValue::from_static("application/octet-stream")
                        })
                    }),
                true,
            ),
            FileResponseMode::Preview => {
                let safe_inline = guessed.type_() == mime_guess::mime::IMAGE
                    && guessed.subtype() != mime_guess::mime::SVG
                    || guessed.type_() == mime_guess::mime::AUDIO
                    || guessed.type_() == mime_guess::mime::VIDEO
                    || guessed == mime_guess::mime::APPLICATION_PDF;
                if safe_inline {
                    (
                        HeaderValue::from_str(guessed.as_ref()).unwrap_or_else(|_| {
                            HeaderValue::from_static("application/octet-stream")
                        }),
                        false,
                    )
                } else if guessed.type_() == mime_guess::mime::TEXT {
                    (HeaderValue::from_static("text/plain; charset=utf-8"), false)
                } else {
                    (HeaderValue::from_static("application/octet-stream"), true)
                }
            }
        };
        Self {
            content_type,
            disposition: attachment.then(|| attachment_header(path)),
        }
    }

    pub(crate) fn apply(
        self,
        builder: axum::http::response::Builder,
    ) -> axum::http::response::Builder {
        let mut builder = builder
            .header(header::CONTENT_TYPE, self.content_type)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
            .header(header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; img-src 'self' data: blob:; media-src 'self' blob:");
        if let Some(disposition) = self.disposition {
            builder = builder.header(header::CONTENT_DISPOSITION, disposition);
        }
        builder
    }
}

pub(crate) fn attachment_header(path: &Path) -> HeaderValue {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let ascii_name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let encoded_name = name
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric()
                || matches!(
                    *byte,
                    b'!' | b'#'
                        | b'$'
                        | b'&'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
            {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{encoded_name}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"download\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn opened_download_keeps_its_own_metadata_when_the_path_is_replaced() {
        let directory = TestDirectory::new("download-opened-identity");
        let original = b"original bytes";
        std::fs::write(directory.path().join("file.bin"), original).unwrap();
        std::fs::write(
            directory.path().join("replacement.bin"),
            b"different longer replacement",
        )
        .unwrap();
        let storage = StorageService::new(directory.path().into(), 1024, 1, 100, 0)
            .await
            .unwrap();
        let path = storage.resolve_existing("file.bin").await.unwrap();
        let permit = storage.stream_gate.clone().acquire_owned().await.unwrap();
        let file = storage.open_file_for_read(&path).await.unwrap();
        tokio::fs::rename(
            directory.path().join("file.bin"),
            directory.path().join("previous.bin"),
        )
        .await
        .unwrap();
        tokio::fs::rename(
            directory.path().join("replacement.bin"),
            directory.path().join("file.bin"),
        )
        .await
        .unwrap();
        let response = storage
            .response_from_open_file(
                file,
                &path,
                &HeaderMap::new(),
                FileResponseMode::Attachment,
                &Method::GET,
                permit,
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            original.len().to_string()
        );
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .as_ref(),
            original
        );
        assert_eq!(storage.stream_gate.available_permits(), 1);
    }

    #[tokio::test]
    async fn local_conditional_resume_falls_back_to_complete_bytes_without_strong_proof() {
        let directory = TestDirectory::new("download-conditional-local");
        let payload = b"complete file";
        std::fs::write(directory.path().join("file.bin"), payload).unwrap();
        let storage = StorageService::new(directory.path().into(), 1024, 1, 100, 0)
            .await
            .unwrap();
        let path = storage.resolve_existing("file.bin").await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=3-5"));
        headers.insert(
            header::IF_RANGE,
            HeaderValue::from_static("\"previous-version\""),
        );
        let response = storage
            .stream_file(&path, &headers, FileResponseMode::WebDav, &Method::GET)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        assert!(!response.headers().contains_key(header::ETAG));
        assert!(response.headers().contains_key(header::LAST_MODIFIED));
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            payload.len().to_string()
        );
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .as_ref(),
            payload
        );
        assert_eq!(storage.stream_gate.available_permits(), 1);
    }

    #[tokio::test]
    async fn local_download_uses_bounded_large_reads_and_preserves_ranges() {
        use futures_util::StreamExt;

        let directory = TestDirectory::new("download-buffer");
        let payload = vec![0x5a; DOWNLOAD_BUFFER_BYTES * 3 + 11];
        std::fs::write(directory.path().join("file.bin"), &payload).unwrap();
        let storage = StorageService::new(directory.path().into(), 1024 * 1024, 1, 100, 0)
            .await
            .unwrap();
        let path = storage.resolve_existing("file.bin").await.unwrap();
        for range in [None, Some("bytes=7-262157")] {
            let mut headers = HeaderMap::new();
            if let Some(range) = range {
                headers.insert(header::RANGE, HeaderValue::from_static(range));
            }
            let response = storage
                .stream_file(&path, &headers, FileResponseMode::Attachment, &Method::GET)
                .await
                .unwrap();
            let mut body = response.into_body().into_data_stream();
            let mut received = Vec::new();
            let mut chunks = 0;
            while let Some(chunk) = body.next().await {
                let chunk = chunk.unwrap();
                assert!(chunk.len() <= DOWNLOAD_BUFFER_BYTES);
                received.extend_from_slice(&chunk);
                chunks += 1;
            }
            drop(body);
            if range.is_some() {
                assert_eq!(received, payload[7..=262157]);
            } else {
                assert_eq!(received, payload);
                assert_eq!(chunks, 4);
            }
            assert_eq!(storage.stream_gate.available_permits(), 1);
        }
    }

    #[test]
    fn all_download_modes_share_the_complete_isolation_policy() {
        for mode in [FileResponseMode::Attachment, FileResponseMode::WebDav] {
            for name in [
                "report.txt",
                "report.html",
                "drawing.svg",
                "report.pdf",
                "image.png",
            ] {
                for stored in [None, Some("application/pdf")] {
                    let response = FileResponsePolicy::new(Path::new(name), stored, mode)
                        .apply(Response::builder())
                        .body(())
                        .unwrap();
                    assert!(response.headers()[header::CONTENT_DISPOSITION]
                        .to_str()
                        .unwrap()
                        .starts_with("attachment;"));
                    assert_eq!(
                        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                        "nosniff"
                    );
                    assert!(response.headers()[header::CONTENT_SECURITY_POLICY]
                        .to_str()
                        .unwrap()
                        .starts_with("sandbox;"));
                }
            }
        }
    }

    #[test]
    fn preview_policy_ignores_object_metadata_and_keeps_supported_media_inline() {
        for (name, expected, attachment) in [
            ("report.txt", "text/plain; charset=utf-8", false),
            ("image.png", "image/png", false),
            ("report.pdf", "application/pdf", false),
            ("drawing.svg", "application/octet-stream", true),
        ] {
            let response = FileResponsePolicy::new(
                Path::new(name),
                Some("application/pdf"),
                FileResponseMode::Preview,
            )
            .apply(Response::builder())
            .body(())
            .unwrap();
            assert_eq!(response.headers()[header::CONTENT_TYPE], expected);
            assert_eq!(
                response.headers().contains_key(header::CONTENT_DISPOSITION),
                attachment
            );
        }
    }

    #[tokio::test]
    async fn local_file_response_preserves_bytes_and_range_under_the_shared_policy() {
        let directory = TestDirectory::new("file-response");
        let storage = StorageService::new(directory.path().join("storage"), 1024, 1, 100, 0)
            .await
            .unwrap();
        let path = directory.path().join("storage").join("notes.txt");
        tokio::fs::write(&path, b"ordinary file data")
            .await
            .unwrap();
        let resolved = storage.resolve_existing("notes.txt").await.unwrap();
        for mode in [
            FileResponseMode::Attachment,
            FileResponseMode::Preview,
            FileResponseMode::WebDav,
        ] {
            let response = storage
                .stream_file(&resolved, &HeaderMap::new(), mode, &Method::GET)
                .await
                .unwrap();
            assert_eq!(response.headers()[header::CONTENT_LENGTH], "18");
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(&bytes[..], b"ordinary file data");
        }
        let mut request = HeaderMap::new();
        request.insert(header::RANGE, HeaderValue::from_static("bytes=0-3"));
        let response = storage
            .stream_file(&resolved, &request, FileResponseMode::WebDav, &Method::GET)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 0-3/18");
        assert!(response.headers().contains_key(header::CONTENT_DISPOSITION));
        assert_eq!(
            &axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()[..],
            b"ordi"
        );
    }

    #[tokio::test]
    async fn global_security_headers_preserve_file_isolation() {
        use axum::{middleware, routing::get, Router};
        use tower::ServiceExt;
        let directory = TestDirectory::new("file-security-middleware");
        let state =
            crate::test_support::app_state(&directory, crate::config::ConfigFile::default()).await;
        let router = Router::new()
            .route(
                "/file",
                get(|| async {
                    FileResponsePolicy::new(Path::new("notes.txt"), None, FileResponseMode::WebDav)
                        .apply(Response::builder())
                        .body(Body::from("ordinary data"))
                        .unwrap()
                }),
            )
            .layer(middleware::from_fn_with_state(
                state,
                crate::security::security_headers_middleware,
            ));
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/file")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let policies: Vec<_> = response
            .headers()
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(policies.len(), 2);
        assert!(policies.iter().any(|policy| policy.starts_with("sandbox;")));
        assert!(policies
            .iter()
            .any(|policy| policy.contains("script-src 'self'")));
        assert!(response.headers().contains_key(header::CONTENT_DISPOSITION));
    }
}
