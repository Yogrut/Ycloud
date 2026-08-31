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
    fs::File,
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
        let mut file = File::open(path.absolute())
            .await
            .map_err(|error| AppError::with_source("failed to open file", error))?;
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
            .header(header::CONTENT_LENGTH, length)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");

        let guessed_mime = mime_guess::from_path(path.absolute()).first_or_octet_stream();
        let (content_type, force_attachment) = content_type_for_mode(&guessed_mime, mode);
        response = response.header(header::CONTENT_TYPE, content_type);
        if matches!(mode, FileResponseMode::Attachment) || force_attachment {
            response = response.header(
                header::CONTENT_DISPOSITION,
                attachment_header(path.absolute()),
            );
        }
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

pub(crate) fn content_type_for_mode(
    guessed: &mime_guess::Mime,
    mode: FileResponseMode,
) -> (HeaderValue, bool) {
    let guessed_string = guessed.to_string();
    match mode {
        FileResponseMode::Attachment | FileResponseMode::WebDav => (
            HeaderValue::from_str(&guessed_string)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            false,
        ),
        FileResponseMode::Preview => {
            let safe_inline = guessed.type_() == mime_guess::mime::IMAGE
                && guessed.subtype() != mime_guess::mime::SVG
                || guessed.type_() == mime_guess::mime::AUDIO
                || guessed.type_() == mime_guess::mime::VIDEO
                || *guessed == mime_guess::mime::APPLICATION_PDF;
            if safe_inline {
                (
                    HeaderValue::from_str(&guessed_string)
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                    false,
                )
            } else if guessed.type_() == mime_guess::mime::TEXT {
                (HeaderValue::from_static("text/plain; charset=utf-8"), false)
            } else {
                (HeaderValue::from_static("application/octet-stream"), true)
            }
        }
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
