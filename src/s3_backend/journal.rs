use aws_smithy_types::byte_stream::ByteStream;
use axum::http::HeaderValue;
use serde::{de::DeserializeOwned, Serialize};

use super::{
    authenticated_journal, capabilities, internal_key, valid_transaction_id,
    validate_multipart_session, validate_upload_transaction, S3Backend, S3MultipartPurpose,
    S3MultipartSession, S3UploadTransaction, S3_MAX_LIST_PAGES, S3_MAX_PENDING_TRANSACTIONS,
    S3_MULTIPART_SESSION_JOURNAL_PURPOSE, S3_MULTIPART_SESSION_SCHEMA_VERSION, S3_PAGE_SIZE,
    S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE,
};
use crate::error::{AppError, AppResult};

/// Collect only bounded recovery records, never file payloads. Header checks
/// precede body polling; actual chunks are checked before appending to the buffer.
pub(super) async fn read_journal_payload(
    output: aws_sdk_s3::operation::get_object::GetObjectOutput,
    max_bytes: usize,
) -> AppResult<(Vec<u8>, String)> {
    let declared_length = output
        .content_length()
        .map(usize::try_from)
        .transpose()
        .map_err(|_| oversized_journal())?;
    if declared_length.is_some_and(|length| length > max_bytes) {
        return Err(oversized_journal());
    }
    let etag = output
        .e_tag()
        .map(str::to_owned)
        .ok_or_else(|| AppError::ServiceUnavailable("恢复记录缺少 ETag".into()))?;
    let mut body = output.body;
    let mut data = Vec::with_capacity(declared_length.unwrap_or(0));
    while let Some(chunk) = body
        .try_next()
        .await
        .map_err(|error| AppError::with_source("failed to stream S3 recovery journal", error))?
    {
        // data.len() never exceeds max_bytes, so subtraction cannot underflow.
        if chunk.len() > max_bytes - data.len() {
            return Err(oversized_journal());
        }
        data.extend_from_slice(&chunk);
    }
    Ok((data, etag))
}

fn oversized_journal() -> AppError {
    AppError::ServiceUnavailable("对象存储恢复记录超过安全上限".into())
}

#[cfg(test)]
mod transport_tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        body::{to_bytes, Body},
        http::{header, HeaderMap, Method, Response},
        routing::any,
        Router,
    };
    use tokio::net::TcpListener;

    use crate::{config::S3Provider, s3_backend::protocol_tests::test_backend};

    #[tokio::test]
    async fn encoded_journals_share_provider_conditions_without_reencoding_bytes() {
        for oss in [false, true] {
            let writes = Arc::new(Mutex::new(Vec::new()));
            let observed = writes.clone();
            let router = Router::new().route(
                "/{*key}",
                any(move |method: Method, headers: HeaderMap, body: Body| {
                    let writes = observed.clone();
                    async move {
                        if method == Method::HEAD {
                            assert!(oss);
                            return Response::builder()
                                .header(header::ETAG, "\"old\"")
                                .header(header::CONTENT_LENGTH, 0)
                                .body(Body::empty())
                                .unwrap();
                        }
                        assert_eq!(method, Method::PUT);
                        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
                        let bytes = to_bytes(body, 1024).await.unwrap();
                        let create = writes.lock().unwrap().is_empty();
                        if oss {
                            assert!(!headers.contains_key(header::IF_MATCH));
                            assert!(!headers.contains_key(header::IF_NONE_MATCH));
                            assert_eq!(
                                headers
                                    .get("x-oss-forbid-overwrite")
                                    .map(|v| v.to_str().unwrap()),
                                create.then_some("true")
                            );
                        } else if create {
                            assert_eq!(headers[header::IF_NONE_MATCH], "*");
                            assert!(!headers.contains_key(header::IF_MATCH));
                        } else {
                            assert_eq!(headers[header::IF_MATCH], "\"old\"");
                            assert!(!headers.contains_key(header::IF_NONE_MATCH));
                        }
                        writes.lock().unwrap().push(bytes.to_vec());
                        Response::builder()
                            .header(header::ETAG, "\"written\"")
                            .body(Body::empty())
                            .unwrap()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut backend = test_backend(&format!("http://{}", listener.local_addr().unwrap()));
            if oss {
                backend.provider = S3Provider::AlibabaOss;
            }
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let payload = br#"{"signed":"record bytes"}"#.to_vec();
            let key = "tenant/.ycloud-system/transactions/fixture";
            for previous in [None, Some("\"old\"")] {
                assert_eq!(
                    backend
                        .write_encoded_journal(key, payload.clone(), previous)
                        .await
                        .unwrap(),
                    "\"written\""
                );
            }
            if oss {
                let error = backend
                    .write_encoded_journal(key, payload.clone(), Some("\"different\""))
                    .await
                    .unwrap_err();
                assert_eq!(error.capability(), Some("conditional_journal_update"));
            }
            assert_eq!(*writes.lock().unwrap(), [payload.clone(), payload]);
            server.abort();
        }
    }
}

impl S3Backend {
    pub(super) async fn write_upload_transaction(
        &self,
        key: &str,
        transaction: &S3UploadTransaction,
        previous_etag: Option<&str>,
    ) -> AppResult<Option<String>> {
        self.write_authenticated_json_journal(
            key,
            S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE,
            transaction,
            previous_etag,
        )
        .await
    }

    pub(super) async fn finish_upload_transaction(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        transaction: &S3UploadTransaction,
    ) -> AppResult<()> {
        if transaction.publication_guard.is_some() {
            self.finish_guarded_upload_resources(transaction).await?;
            return self.delete_key_confirmed(journal_key, journal_etag).await;
        }
        let temporary_key = internal_key(&self.prefix, "uploads", &transaction.id);
        let backup_key = internal_key(&self.prefix, "backups", &transaction.id);
        self.delete_key_confirmed(&temporary_key, transaction.temporary.etag.as_deref())
            .await?;
        self.delete_key_confirmed(&backup_key, None).await?;
        self.delete_key_confirmed(journal_key, journal_etag).await
    }

    pub(super) async fn finish_upload_and_intent(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        transaction: &S3UploadTransaction,
        intent_key: &str,
        intent_etag: Option<&str>,
    ) -> AppResult<()> {
        self.finish_upload_transaction(journal_key, journal_etag, transaction)
            .await?;
        self.release_internal_upload_intent(intent_key, intent_etag)
            .await
    }

    pub(super) async fn write_authenticated_json_journal<T: Serialize>(
        &self,
        key: &str,
        purpose: &str,
        value: &T,
        previous_etag: Option<&str>,
    ) -> AppResult<Option<String>> {
        let data = authenticated_journal::encode(&self.transaction_auth_key, purpose, value)?;
        self.write_encoded_journal(key, data, previous_etag)
            .await
            .map(Some)
    }

    /// Publish already encoded journal bytes. Authentication, schema validation
    /// and record-specific bounds belong to the caller, not this transport.
    /// OSS version checking preserves the single-writer compatibility path;
    /// it is not an atomic compare-and-swap against external writers.
    pub(super) async fn write_encoded_journal(
        &self,
        key: &str,
        data: Vec<u8>,
        previous_etag: Option<&str>,
    ) -> AppResult<String> {
        let content_length = i64::try_from(data.len())
            .map_err(|_| AppError::ServiceUnavailable("对象存储事务记录过大".into()))?;
        if self.is_alibaba_oss() {
            if let Some(expected_etag) = previous_etag {
                let current = self.head_key(key).await.map_err(|_| {
                    AppError::storage_capability(
                        capabilities::CONDITIONAL_JOURNAL_UPDATE,
                        "对象存储无法核对事务记录版本",
                    )
                })?;
                if current
                    .as_ref()
                    .and_then(|metadata| metadata.etag.as_deref())
                    != Some(expected_etag)
                {
                    return Err(AppError::storage_capability(
                        capabilities::CONDITIONAL_JOURNAL_UPDATE,
                        "对象存储事务记录在更新前发生变化",
                    ));
                }
            }
        }
        self.recovery_runtime.journal_write_started(key);
        let _permit = self.acquire_request().await?;
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_length(content_length)
            .content_type("application/json")
            .body(ByteStream::from(data));
        if !self.is_alibaba_oss() {
            request = if let Some(etag) = previous_etag {
                request.if_match(etag)
            } else {
                request.if_none_match("*")
            };
        }
        let result = if self.is_alibaba_oss() && previous_etag.is_none() {
            request
                .customize()
                .mutate_request(|request| {
                    request
                        .headers_mut()
                        .insert("x-oss-forbid-overwrite", HeaderValue::from_static("true"));
                })
                .send()
                .await
        } else {
            request.send().await
        };
        let capability = if previous_etag.is_some() {
            capabilities::CONDITIONAL_JOURNAL_UPDATE
        } else {
            capabilities::CONDITIONAL_JOURNAL
        };
        let output = result.map_err(|error| {
            tracing::error!(
                error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                "S3 transaction journal write failed"
            );
            AppError::storage_capability(capability, "无法持久化对象存储事务状态")
        })?;
        let etag = output.e_tag().map(str::to_owned).ok_or_else(|| {
            AppError::storage_capability(capability, "对象存储未返回事务记录 ETag")
        })?;
        Ok(etag)
    }

    pub(super) async fn register_multipart_intent(
        &self,
        key: &str,
        purpose: S3MultipartPurpose,
        expected_size: u64,
        operation_id: Option<&str>,
    ) -> AppResult<(String, S3MultipartSession, Option<String>)> {
        let id =
            operation_id.map_or_else(|| uuid::Uuid::new_v4().simple().to_string(), str::to_owned);
        let journal_key = internal_key(&self.prefix, "multipart-sessions", &id);
        let session = S3MultipartSession {
            schema_version: S3_MULTIPART_SESSION_SCHEMA_VERSION,
            id,
            key: key.to_owned(),
            upload_id: None,
            purpose: Some(purpose),
            expected_size: Some(expected_size),
        };
        let etag = self
            .write_multipart_session(&journal_key, &session, None)
            .await?;
        Ok((journal_key, session, etag))
    }

    pub(super) async fn write_multipart_session(
        &self,
        journal_key: &str,
        session: &S3MultipartSession,
        previous_etag: Option<&str>,
    ) -> AppResult<Option<String>> {
        validate_multipart_session(&self.prefix, journal_key, session)?;
        self.validate_directory_multipart_target(session).await?;
        self.write_authenticated_json_journal(
            journal_key,
            S3_MULTIPART_SESSION_JOURNAL_PURPOSE,
            session,
            previous_etag,
        )
        .await
    }

    pub(super) async fn delete_transaction_best_effort(&self, key: &str, etag: Option<&str>) {
        if let Err(error) = self.delete_key_confirmed(key, etag).await {
            tracing::warn!(%error, "failed to clean up an S3 transaction journal");
        }
    }

    pub(super) async fn list_recovery_journal_keys(&self, area: &str) -> AppResult<Vec<String>> {
        self.maintenance
            .read(self.list_recovery_journal_keys_uninterrupted(area))
            .await
    }

    async fn list_recovery_journal_keys_uninterrupted(&self, area: &str) -> AppResult<Vec<String>> {
        let prefix = internal_key(&self.prefix, area, "");
        let mut continuation_token: Option<String> = None;
        let mut keys = Vec::new();
        for _ in 0..S3_MAX_LIST_PAGES {
            let remaining = S3_MAX_PENDING_TRANSACTIONS
                .saturating_add(1)
                .saturating_sub(keys.len());
            if remaining == 0 {
                return Err(AppError::ServiceUnavailable(
                    "待恢复对象存储记录超过安全上限".into(),
                ));
            }
            let max_keys = i32::try_from(remaining.min(S3_PAGE_SIZE))
                .map_err(|_| AppError::internal("invalid S3 recovery list page size"))?;
            let permit = self.acquire_request().await?;
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .max_keys(max_keys);
            if let Some(token) = continuation_token.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request.send().await.map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 recovery journal listing failed"
                );
                AppError::ServiceUnavailable("无法列举对象存储恢复记录".into())
            })?;
            drop(permit);

            for object in output.contents() {
                if keys.len() == S3_MAX_PENDING_TRANSACTIONS {
                    return Err(AppError::ServiceUnavailable(
                        "待恢复对象存储记录超过安全上限".into(),
                    ));
                }
                let key = object.key().ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储恢复记录缺少键名".into())
                })?;
                let id = key.strip_prefix(&prefix).ok_or_else(|| {
                    AppError::ServiceUnavailable("对象存储恢复记录越出保留前缀".into())
                })?;
                if !valid_transaction_id(id) {
                    return Err(AppError::ServiceUnavailable(
                        "对象存储恢复区包含无法识别的记录".into(),
                    ));
                }
                keys.push(key.to_owned());
            }
            if !output.is_truncated().unwrap_or(false) {
                return Ok(keys);
            }
            let next = output
                .next_continuation_token()
                .ok_or_else(|| AppError::ServiceUnavailable("对象存储恢复分页结果无效".into()))?;
            if continuation_token.as_deref() == Some(next) {
                return Err(AppError::ServiceUnavailable(
                    "对象存储恢复分页未前进".into(),
                ));
            }
            continuation_token = Some(next.to_owned());
        }
        Err(AppError::ServiceUnavailable(
            "对象存储恢复分页超过安全页数上限".into(),
        ))
    }

    pub(super) async fn read_upload_transaction(
        &self,
        key: &str,
    ) -> AppResult<(S3UploadTransaction, String)> {
        let (transaction, etag) = self
            .read_authenticated_json_journal(key, S3_UPLOAD_TRANSACTION_JOURNAL_PURPOSE)
            .await?;
        validate_upload_transaction(&self.prefix, key, &transaction)?;
        Ok((transaction, etag))
    }

    pub(super) async fn read_multipart_session(
        &self,
        key: &str,
    ) -> AppResult<(S3MultipartSession, String)> {
        let (session, etag) = self
            .read_authenticated_json_journal(key, S3_MULTIPART_SESSION_JOURNAL_PURPOSE)
            .await?;
        validate_multipart_session(&self.prefix, key, &session)?;
        // The authenticated child record remains an ownership proof after its
        // completed parent has been removed. New writes still require a parent.
        if let Some(id) = super::directory_trash_transaction_id(&self.prefix, &session.key) {
            let parent_key = internal_key(&self.prefix, "directory-transactions", id);
            if self.head_key(&parent_key).await?.is_some() {
                self.validate_directory_multipart_target(&session).await?;
            }
        } else {
            self.validate_directory_multipart_target(&session).await?;
        }
        Ok((session, etag))
    }

    pub(super) async fn read_authenticated_json_journal<T: DeserializeOwned>(
        &self,
        key: &str,
        purpose: &str,
    ) -> AppResult<(T, String)> {
        self.maintenance
            .read(self.read_authenticated_json_journal_uninterrupted(key, purpose))
            .await
    }

    async fn read_authenticated_json_journal_uninterrupted<T: DeserializeOwned>(
        &self,
        key: &str,
        purpose: &str,
    ) -> AppResult<(T, String)> {
        let _permit = self.acquire_request().await?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                tracing::error!(
                    error_kind = %error.as_service_error().map_or("transport", |_| "service"),
                    "S3 recovery journal read failed"
                );
                AppError::ServiceUnavailable("无法读取对象存储恢复记录".into())
            })?;
        let (data, etag) =
            read_journal_payload(output, authenticated_journal::MAX_ENVELOPE_BYTES).await?;
        let value = authenticated_journal::decode(&self.transaction_auth_key, purpose, &data)?;
        Ok((value, etag))
    }
}

#[cfg(test)]
mod tests;
