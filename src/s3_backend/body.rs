use std::{
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    body::Body,
    http::{header, Response, StatusCode},
};
use bytes::Bytes;
use futures_util::Stream;
use http_body::{Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;

use crate::error::{AppError, AppResult};

pub(super) fn range_not_satisfiable(total_length: u64) -> AppResult<Response<Body>> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{total_length}"))
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .map_err(|error| AppError::with_source("failed to build S3 range response", error))
}

pub(super) struct ExactLengthBody<S> {
    state: std::sync::Mutex<ExactLengthState<S>>,
}

struct ExactLengthState<S> {
    inner: Option<S>,
    remaining: u64,
}

impl<S> ExactLengthBody<S> {
    pub(super) fn new(inner: S, expected: u64) -> Self {
        Self {
            state: std::sync::Mutex::new(ExactLengthState {
                inner: Some(inner),
                remaining: expected,
            }),
        }
    }
}

impl<S, E> http_body::Body for ExactLengthBody<S>
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + 'static,
{
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let Ok(mut state) = self.state.lock() else {
            return Poll::Ready(Some(Err(std::io::Error::other(
                "upload body state is unavailable",
            ))));
        };
        let Some(inner) = state.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(inner).poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                let Ok(length) = u64::try_from(chunk.len()) else {
                    state.inner = None;
                    return Poll::Ready(Some(Err(std::io::Error::other(
                        "upload chunk length overflow",
                    ))));
                };
                if length > state.remaining {
                    state.inner = None;
                    return Poll::Ready(Some(Err(std::io::Error::other(
                        "upload body exceeds Content-Length",
                    ))));
                }
                state.remaining -= length;
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Some(Err(error))) => {
                state.inner = None;
                Poll::Ready(Some(Err(std::io::Error::other(error.to_string()))))
            }
            Poll::Ready(None) if state.remaining == 0 => {
                state.inner = None;
                Poll::Ready(None)
            }
            Poll::Ready(None) => {
                state.inner = None;
                Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "upload body is shorter than Content-Length",
                ))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |state| state.inner.is_none())
    }

    fn size_hint(&self) -> SizeHint {
        let remaining = self.state.lock().map_or(0, |state| state.remaining);
        SizeHint::with_exact(remaining)
    }
}

pub(super) struct PermitStream<S> {
    pub(super) inner: S,
    pub(super) _permit: OwnedSemaphorePermit,
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
