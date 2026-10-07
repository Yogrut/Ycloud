//! Preserve shared safe error details without changing DAV commit evidence.
use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

use crate::error::AppError;

#[derive(Debug)]
pub enum DavError {
    Status(StatusCode),
    Backend(AppError),
    FiniteDepth,
}

impl From<StatusCode> for DavError {
    fn from(status: StatusCode) -> Self {
        Self::Status(status)
    }
}

impl From<AppError> for DavError {
    fn from(error: AppError) -> Self {
        Self::Backend(error)
    }
}

impl IntoResponse for DavError {
    fn into_response(self) -> Response {
        match self {
            Self::Backend(error) => error.into_response(),
            Self::Status(status) => (status, status.canonical_reason().unwrap_or("WebDAV request failed")).into_response(),
            Self::FiniteDepth => (StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
                r#"<?xml version="1.0" encoding="utf-8"?><D:error xmlns:D="DAV:"><D:propfind-finite-depth/></D:error>"#).into_response(),
        }
    }
}
