use std::{borrow::Cow, fmt};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    BadRequest(Cow<'static, str>),
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict(Cow<'static, str>),
    PayloadTooLarge,
    InsufficientStorage,
    RequestTimeout,
    TooManyRequests,
    ServiceUnavailable(Cow<'static, str>),
    Internal {
        context: Cow<'static, str>,
        source: Option<anyhow::Error>,
    },
}

impl AppError {
    pub fn internal(context: impl Into<Cow<'static, str>>) -> Self {
        Self::Internal {
            context: context.into(),
            source: None,
        }
    }

    pub fn with_source(
        context: impl Into<Cow<'static, str>>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self::Internal {
            context: context.into(),
            source: Some(source.into()),
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
            Self::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) fn public_message(&self) -> Cow<'static, str> {
        match self {
            Self::BadRequest(message)
            | Self::Conflict(message)
            | Self::ServiceUnavailable(message) => message.clone(),
            Self::Unauthorized => "Authentication required".into(),
            Self::Forbidden => "Access denied".into(),
            Self::NotFound => "Resource not found".into(),
            Self::PayloadTooLarge => "Payload exceeds the configured limit".into(),
            Self::InsufficientStorage => {
                "Storage does not have enough free space for this upload".into()
            }
            Self::RequestTimeout => "Request exceeded the configured timeout".into(),
            Self::TooManyRequests => "Too many requests".into(),
            Self::Internal { .. } => "Internal server error".into(),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest(message)
            | Self::Conflict(message)
            | Self::ServiceUnavailable(message) => formatter.write_str(message),
            Self::Unauthorized => formatter.write_str("unauthorized"),
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::NotFound => formatter.write_str("not found"),
            Self::PayloadTooLarge => formatter.write_str("payload too large"),
            Self::InsufficientStorage => formatter.write_str("insufficient storage"),
            Self::RequestTimeout => formatter.write_str("request timeout"),
            Self::TooManyRequests => formatter.write_str("too many requests"),
            Self::Internal { context, source } => {
                write!(formatter, "{context}")?;
                if let Some(source) = source {
                    write!(formatter, ": {source}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for AppError {}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetails<'a>,
}

#[derive(Serialize)]
struct ErrorDetails<'a> {
    code: &'a str,
    message: Cow<'a, str>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }
        let code = match &self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::InsufficientStorage => "insufficient_storage",
            Self::RequestTimeout => "request_timeout",
            Self::TooManyRequests => "too_many_requests",
            Self::ServiceUnavailable(_) => "service_unavailable",
            Self::Internal { .. } => "internal_error",
        };
        let body = ErrorBody {
            error: ErrorDetails {
                code,
                message: self.public_message(),
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<StatusCode> for AppError {
    fn from(status: StatusCode) -> Self {
        match status {
            StatusCode::BAD_REQUEST => Self::BadRequest("Invalid request".into()),
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::FORBIDDEN => Self::Forbidden,
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::CONFLICT => Self::Conflict("Resource already exists".into()),
            StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            StatusCode::INSUFFICIENT_STORAGE => Self::InsufficientStorage,
            StatusCode::REQUEST_TIMEOUT => Self::RequestTimeout,
            StatusCode::TOO_MANY_REQUESTS => Self::TooManyRequests,
            StatusCode::SERVICE_UNAVAILABLE => {
                Self::ServiceUnavailable("Service temporarily unavailable".into())
            }
            _ => Self::internal("request failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppError;
    use axum::http::StatusCode;

    #[test]
    fn status_conversion_preserves_service_unavailable() {
        let error = AppError::from(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
