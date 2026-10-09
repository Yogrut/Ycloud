//! Administrative mutations own their execution through configuration
//! publication and session invalidation, even if the HTTP waiter disconnects.
use std::{future::Future, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::AppError;

pub(crate) async fn complete_mutation(
    State(gate): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let Ok(permit) = gate.try_acquire_owned() else {
        return AppError::TooManyRequests.into_response();
    };
    // Authentication/CSRF run outside this layer. Do not detach a partially
    // received request or an unbounded body from its HTTP connection.
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if std::error::Error::source(&error)
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                return AppError::PayloadTooLarge.into_response();
            }
            return AppError::BadRequest("Unable to read bounded administrative request".into())
                .into_response();
        }
    };
    complete_owned(
        permit,
        next.run(Request::from_parts(parts, Body::from(body))),
    )
    .await
}

async fn complete_owned(
    permit: OwnedSemaphorePermit,
    execution: impl Future<Output = Response> + Send + 'static,
) -> Response {
    match tokio::spawn(async move {
        let _permit = permit;
        execution.await
    })
    .await
    {
        Ok(response) => response,
        Err(error) => AppError::with_source(
            "administrative execution failed; inspect current state before retrying",
            error,
        )
        .with_operation(
            crate::error::CommitState::Unknown,
            crate::error::CleanupState::Unknown,
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn detached_waiter_does_not_cancel_execution_or_release_its_budget() {
        let gate = Arc::new(Semaphore::new(1));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let finished = Arc::new(Notify::new());
        let permit = gate.clone().acquire_owned().await.unwrap();
        let waiter = tokio::spawn(complete_owned(permit, {
            let started = started.clone();
            let release = release.clone();
            let finished = finished.clone();
            async move {
                started.notify_one();
                release.notified().await;
                finished.notify_one();
                axum::http::StatusCode::NO_CONTENT.into_response()
            }
        }));
        started.notified().await;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(gate.clone().try_acquire_owned().is_err());
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), finished.notified())
            .await
            .unwrap();
        let _permit = tokio::time::timeout(std::time::Duration::from_secs(2), gate.acquire())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn execution_failure_keeps_unknown_commit_evidence() {
        let gate = Arc::new(Semaphore::new(1));
        let permit = gate.clone().acquire_owned().await.unwrap();
        let response = complete_owned(permit, async { panic!("injected execution failure") }).await;
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["operation"]["commit"], "unknown");
        assert_eq!(payload["error"]["operation"]["cleanup"], "unknown");
        assert!(gate.try_acquire_owned().is_ok());
    }
}
