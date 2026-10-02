//! Shared pre-transfer ownership boundary and exact-ID abort operations.
use std::time::Duration;

use aws_smithy_types::error::metadata::ProvideErrorMetadata;

use crate::{
    error::{AppError, AppResult, CleanupState, CommitState},
    s3_backend::{
        capabilities, S3Backend, S3MultipartAbortOutcome, S3MultipartPurpose, S3MultipartSession,
        S3_OPERATION_METADATA_KEY,
    },
};

const UNSTARTED_INTENT_CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);

/// Returned only after the provider ID has been written to the recovery journal.
/// Upload/copy callers cannot reach payload transfer before this returns.
pub(super) struct StartedMultipart {
    pub(super) journal_key: String,
    pub(super) journal_etag: Option<String>,
    pub(super) session: S3MultipartSession,
    pub(super) upload_id: String,
}

impl S3Backend {
    pub(super) async fn start_multipart_operation(
        &self,
        key: &str,
        purpose: S3MultipartPurpose,
        size: u64,
        content_type: Option<&str>,
        operation_id: Option<&str>,
    ) -> AppResult<StartedMultipart> {
        let (journal_key, mut session, journal_etag) = self
            .register_multipart_intent(key, purpose, size, operation_id)
            .await?;
        let upload_id_result = async {
            let _permit = self.acquire_request().await?;
            let mut create = self
                .client
                .create_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .metadata(S3_OPERATION_METADATA_KEY, &session.id);
            if let Some(content_type) = content_type {
                create = create.content_type(content_type);
            }
            create
                .send()
                .await
                .map_err(|_| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_CREATE,
                        match purpose {
                            S3MultipartPurpose::Upload => "无法创建对象存储分片上传",
                            S3MultipartPurpose::Copy => "无法创建对象存储分片复制",
                        },
                    )
                })?
                .upload_id()
                .map(str::to_owned)
                .ok_or_else(|| {
                    AppError::storage_capability(
                        capabilities::MULTIPART_CREATE,
                        match purpose {
                            S3MultipartPurpose::Upload => "对象存储未返回分片上传 ID",
                            S3MultipartPurpose::Copy => "对象存储未返回分片复制 ID",
                        },
                    )
                })
        }
        .await;
        let upload_id = match upload_id_result {
            Ok(upload_id) => upload_id,
            Err(error) => {
                // No payload has been released. Never infer an unknown provider
                // ID from the key; an empty remote session needs bucket lifecycle cleanup.
                let cleanup = self
                    .release_unstarted_multipart_intent(&journal_key, journal_etag.as_deref())
                    .await;
                return Err(error.with_operation(CommitState::NotCommitted, cleanup));
            }
        };
        session.upload_id = Some(upload_id.clone());
        let journal_etag = match self
            .write_multipart_session(&journal_key, &session, journal_etag.as_deref())
            .await
        {
            Ok(etag) => etag,
            Err(error) => {
                if self
                    .abort_multipart_operation(key, &upload_id)
                    .await
                    .is_ok()
                {
                    self.delete_transaction_best_effort(&journal_key, journal_etag.as_deref())
                        .await;
                }
                return Err(error);
            }
        };
        Ok(StartedMultipart {
            journal_key,
            journal_etag,
            session,
            upload_id,
        })
    }

    async fn release_unstarted_multipart_intent(
        &self,
        session_key: &str,
        session_etag: Option<&str>,
    ) -> CleanupState {
        // Never wait for a slow remote cleanup before reporting create failure.
        // The authenticated intent remains recoverable after error or timeout.
        match tokio::time::timeout(
            UNSTARTED_INTENT_CLEANUP_TIMEOUT,
            self.delete_key_confirmed(session_key, session_etag),
        )
        .await
        {
            Ok(Ok(())) => CleanupState::Complete,
            _ => CleanupState::Pending,
        }
    }

    pub(in crate::s3_backend) async fn abort_multipart_operation(
        &self,
        key: &str,
        upload_id: &str,
    ) -> AppResult<S3MultipartAbortOutcome> {
        self.maintenance
            .read(self.abort_multipart_operation_uninterrupted(key, upload_id))
            .await
    }

    async fn abort_multipart_operation_uninterrupted(
        &self,
        key: &str,
        upload_id: &str,
    ) -> AppResult<S3MultipartAbortOutcome> {
        let _permit = self.acquire_request().await?;
        let result = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await;
        match result {
            Ok(_) => Ok(S3MultipartAbortOutcome::Aborted),
            Err(error)
                if error
                    .as_service_error()
                    .and_then(ProvideErrorMetadata::code)
                    == Some("NoSuchUpload") =>
            {
                Ok(S3MultipartAbortOutcome::Missing)
            }
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 multipart abort failed"
                );
                Err(AppError::storage_capability(
                    capabilities::MULTIPART_ABORT,
                    "对象存储分片会话暂时无法终止",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{HeaderMap, Method, Response, Uri},
        routing::any,
        Router,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct ObservedSession {
        events: Vec<&'static str>,
        persisted: Option<S3MultipartSession>,
    }

    // A local protocol fixture, not a production fault-injection interface.
    async fn check_start(purpose: S3MultipartPurpose, scenario: &'static str) {
        const ID: &str = "0123456789abcdef0123456789abcdef";
        let state = Arc::new(Mutex::new(ObservedSession::default()));
        let observed = state.clone();
        let router = Router::new().route(
            "/{*key}",
            any(move |method: Method, uri: Uri, headers: HeaderMap, body: bytes::Bytes| {
                let observed = observed.clone();
                async move {
                    let mut state = observed.lock().unwrap();
                    let journal = uri.path().contains("/multipart-sessions/");
                    let query = uri.query().unwrap_or("");
                    let response = Response::builder();
                    if method == Method::PUT && journal {
                        let record: S3MultipartSession = crate::s3_backend::authenticated_journal::decode(
                            &[0x31; 32],
                            crate::s3_backend::S3_MULTIPART_SESSION_JOURNAL_PURPOSE,
                            &body,
                        ).unwrap();
                        assert_eq!(record.id, ID);
                        assert_eq!(record.purpose, Some(purpose));
                        assert_eq!(record.expected_size, Some(crate::s3_backend::S3_SINGLE_COPY_LIMIT + 1));
                        let binding = record.upload_id.is_some();
                        state.events.push(if binding { "bind_id" } else { "intent" });
                        if binding {
                            assert_eq!(record.upload_id.as_deref(), Some("provider-id"));
                            assert_eq!(headers.get("if-match").unwrap(), "\"journal-v1\"");
                        } else {
                            assert_eq!(headers.get("if-none-match").unwrap(), "*");
                        }
                        if (scenario == "intent_rejected" && !binding)
                            || (matches!(scenario, "binding_rejected" | "abort_rejected") && binding)
                        {
                            return response.status(403).body(Body::from("<Error><Code>AccessDenied</Code></Error>")).unwrap();
                        }
                        state.persisted = Some(record);
                        return response.header("etag", if binding { "\"journal-v2\"" } else { "\"journal-v1\"" }).body(Body::empty()).unwrap();
                    }
                    if method == Method::POST && (query == "uploads" || query.starts_with("uploads=")) {
                        state.events.push("create");
                        assert!(state.persisted.as_ref().unwrap().upload_id.is_none());
                        assert_eq!(headers.get("x-amz-meta-ycloud-operation").unwrap(), ID);
                        assert_eq!(headers.get("content-type").unwrap(), "application/test-binary");
                        if scenario == "create_rejected" {
                            return response.status(403).body(Body::from("<Error><Code>AccessDenied</Code></Error>")).unwrap();
                        }
                        let id = if scenario == "missing_id" { "" } else { "<UploadId>provider-id</UploadId>" };
                        return response.header("content-type", "application/xml")
                            .body(Body::from(format!("<InitiateMultipartUploadResult>{id}</InitiateMultipartUploadResult>"))).unwrap();
                    }
                    if method == Method::DELETE && query.contains("uploadId=") {
                        state.events.push("abort");
                        assert!(query.contains("uploadId=provider-id"));
                        if scenario == "abort_rejected" {
                            return response.status(403).body(Body::from("<Error><Code>AccessDenied</Code></Error>")).unwrap();
                        }
                        return response.status(204).body(Body::empty()).unwrap();
                    }
                    if method == Method::HEAD && journal {
                        return if state.persisted.is_some() {
                            response.header("content-length", "0").header("etag", "\"journal-v1\"").body(Body::empty()).unwrap()
                        } else {
                            response.status(404).body(Body::empty()).unwrap()
                        };
                    }
                    if method == Method::DELETE && journal {
                        state.events.push("retire_intent");
                        state.persisted = None;
                        return response.status(204).body(Body::empty()).unwrap();
                    }
                    state.events.push("unexpected_request");
                    response.status(400).body(Body::empty()).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = crate::s3_backend::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let target = match purpose {
            S3MultipartPurpose::Upload => format!("tenant/.ycloud-system/uploads/{ID}"),
            S3MultipartPurpose::Copy => "tenant/destination.bin".into(),
        };
        let result = backend
            .start_multipart_operation(
                &target,
                purpose,
                crate::s3_backend::S3_SINGLE_COPY_LIMIT + 1,
                Some("application/test-binary"),
                Some(ID),
            )
            .await;
        let state = state.lock().unwrap();
        let expected_events: &[&str] = match scenario {
            "ready" => {
                let started = result.unwrap();
                assert_eq!(started.upload_id, "provider-id");
                assert_eq!(started.session, *state.persisted.as_ref().unwrap());
                assert_eq!(started.journal_etag.as_deref(), Some("\"journal-v2\""));
                assert_eq!(
                    started.journal_key,
                    format!("tenant/.ycloud-system/multipart-sessions/{ID}")
                );
                &["intent", "create", "bind_id"]
            }
            "intent_rejected" => {
                assert!(result.is_err());
                assert!(state.persisted.is_none());
                &["intent"]
            }
            "create_rejected" | "missing_id" => {
                let error = result.err().unwrap();
                assert_eq!(error.operation().unwrap().commit, CommitState::NotCommitted);
                assert_eq!(error.operation().unwrap().cleanup, CleanupState::Complete);
                assert!(state.persisted.is_none());
                &["intent", "create", "retire_intent"]
            }
            "binding_rejected" => {
                assert!(result.is_err());
                assert!(state.persisted.is_none());
                &["intent", "create", "bind_id", "abort", "retire_intent"]
            }
            "abort_rejected" => {
                assert!(result.is_err());
                assert!(state.persisted.as_ref().unwrap().upload_id.is_none());
                &["intent", "create", "bind_id", "abort"]
            }
            _ => unreachable!(),
        };
        assert_eq!(state.events, expected_events, "{purpose:?}: {scenario}");
        server.abort();
    }

    #[tokio::test]
    async fn upload_and_copy_start_only_after_the_provider_id_is_durable() {
        for purpose in [S3MultipartPurpose::Upload, S3MultipartPurpose::Copy] {
            check_start(purpose, "ready").await;
        }
    }

    #[tokio::test]
    async fn failed_starts_retire_only_owned_intents_and_abort_only_known_ids() {
        for purpose in [S3MultipartPurpose::Upload, S3MultipartPurpose::Copy] {
            for scenario in [
                "intent_rejected",
                "create_rejected",
                "missing_id",
                "binding_rejected",
                "abort_rejected",
            ] {
                check_start(purpose, scenario).await;
            }
        }
    }
}
