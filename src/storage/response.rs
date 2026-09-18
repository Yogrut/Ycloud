use std::{
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    body::Body,
    http::{
        header::{self, HeaderMap, HeaderValue},
        Response, StatusCode,
    },
};
use bytes::Bytes;
use futures_util::Stream;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::OwnedSemaphorePermit,
};
use tokio_util::io::ReaderStream;

use super::{ResolvedPath, StorageService};
use crate::error::{AppError, AppResult};

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
    ) -> AppResult<Response<Body>> {
        let metadata = self.metadata(path).await?;
        if !metadata.is_file() {
            return Err(AppError::NotFound);
        }

        let total_length = metadata.len();
        let range = parse_range(request_headers, total_length);
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
        let mut file = self.open_file_for_read(path).await?;
        if start > 0 {
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|error| AppError::with_source("failed to seek file", error))?;
        }

        let permit = self
            .io_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::ServiceUnavailable("Storage is shutting down".into()))?;
        let reader = file.take(length);
        let stream = PermitStream {
            inner: ReaderStream::new(reader),
            _permit: permit,
        };
        let mut response = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length);
        response = FileResponsePolicy::new(path.absolute(), None, mode).apply(response);
        if status == StatusCode::PARTIAL_CONTENT {
            let end = start.saturating_add(length).saturating_sub(1);
            response = response.header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total_length}"),
            );
        }

        response
            .body(Body::from_stream(stream))
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

pub(crate) fn parse_range(
    headers: &HeaderMap,
    total_length: u64,
) -> Result<Option<(u64, u64, StatusCode)>, ()> {
    let Some(header_value) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    let raw = header_value.to_str().map_err(|_| ())?;
    let value = raw.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') || total_length == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let suffix = suffix.min(total_length);
        (total_length.saturating_sub(suffix), total_length - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= total_length {
            return Err(());
        }
        let end = if end.is_empty() {
            total_length - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total_length - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(Some((start, end - start + 1, StatusCode::PARTIAL_CONTENT)))
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
                .stream_file(&resolved, &HeaderMap::new(), mode)
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
            .stream_file(&resolved, &request, FileResponseMode::WebDav)
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
        let runtime = crate::test_support::runtime_config(directory.path());
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
                runtime,
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
