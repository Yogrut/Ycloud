//! Browser multipart control channel. Only payload bytes bypass Ycloud.
use std::time::Duration;

use aws_sdk_s3::{
    presigning::PresigningConfig,
    types::{CompletedMultipartUpload, CompletedPart},
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use super::{multipart_part_size, S3Backend};
use crate::error::{AppError, AppResult};

pub(crate) struct UploadInput {
    pub body: axum::body::Body,
    pub direct: Option<DirectChannel>,
    pub operation_id: Option<String>,
    pub cancellation: Option<tokio_util::sync::CancellationToken>,
    pub commit_owner:
        Option<std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::OwnedMutexGuard<()>>>>>,
}

impl UploadInput {
    pub fn relay(body: axum::body::Body) -> Self {
        Self {
            body,
            direct: None,
            operation_id: None,
            cancellation: None,
            commit_owner: None,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct DirectDescriptor {
    pub part_size: usize,
    pub part_count: u64,
    pub concurrency: usize,
}

#[derive(Serialize)]
pub(crate) struct SignedPart {
    pub url: String,
}

pub(crate) enum DirectCommand {
    Sign(u64, oneshot::Sender<AppResult<SignedPart>>),
    Complete,
    Cancel,
}

pub(crate) struct DirectChannel {
    pub ready: oneshot::Sender<AppResult<DirectDescriptor>>,
    pub commands: mpsc::Receiver<DirectCommand>,
}

impl S3Backend {
    pub(super) async fn receive_direct_parts(
        &self,
        key: &str,
        upload_id: &str,
        size: u64,
        channel: DirectChannel,
    ) -> AppResult<()> {
        let part_size = multipart_part_size(size)?;
        let count = size.max(1).div_ceil(part_size as u64);
        let _ = channel.ready.send(Ok(DirectDescriptor {
            part_size,
            part_count: count,
            concurrency: 4,
        }));
        let mut commands = channel.commands;
        loop {
            // A vanished browser does not retain a live upload forever. A
            // complete part set can be committed even if its final command was lost.
            let command = match tokio::time::timeout(self.upload_timeout, commands.recv()).await {
                Ok(Some(command)) => command,
                Ok(None) | Err(_) => DirectCommand::Complete,
            };
            match command {
                DirectCommand::Cancel => return Err(AppError::ClientClosedRequest),
                DirectCommand::Sign(number, reply) => {
                    let result = async {
                        let length = direct_part_length(size, part_size as u64, number)?;
                        let request = self
                            .client
                            .upload_part()
                            .bucket(&self.bucket)
                            .key(key)
                            .upload_id(upload_id)
                            .part_number(number as i32)
                            .content_length(length as i64)
                            .presigned(
                                PresigningConfig::expires_in(Duration::from_secs(300)).map_err(
                                    |_| AppError::ServiceUnavailable("无法签发上传地址".into()),
                                )?,
                            )
                            .await
                            .map_err(|_| AppError::ServiceUnavailable("无法签发上传地址".into()))?;
                        Ok(SignedPart {
                            url: request.uri().to_owned(),
                        })
                    }
                    .await;
                    let _ = reply.send(result);
                }
                DirectCommand::Complete => {
                    // Never accept client-reported size/ETags as proof of storage contents.
                    let mut marker = None;
                    let mut completed = Vec::new();
                    loop {
                        let _permit = self.acquire_request().await?;
                        let page = self
                            .client
                            .list_parts()
                            .bucket(&self.bucket)
                            .key(key)
                            .upload_id(upload_id)
                            .set_part_number_marker(marker.clone())
                            .send()
                            .await
                            .map_err(|_| AppError::ServiceUnavailable("无法核验直传分片".into()))?;
                        for part in page.parts() {
                            let expected_number = completed.len() as u64 + 1;
                            let expected_size =
                                direct_part_length(size, part_size as u64, expected_number)?;
                            if part.part_number() != Some(expected_number as i32)
                                || part.size() != Some(expected_size as i64)
                                || part.e_tag().is_none()
                            {
                                return Err(AppError::BadRequest(
                                    "直传分片数量、顺序或大小不符合上传声明".into(),
                                ));
                            }
                            completed.push(
                                CompletedPart::builder()
                                    .part_number(expected_number as i32)
                                    .e_tag(part.e_tag().unwrap())
                                    .build(),
                            );
                        }
                        if !page.is_truncated().unwrap_or(false) {
                            break;
                        }
                        let next = page
                            .next_part_number_marker()
                            .map(str::to_owned)
                            .ok_or_else(|| {
                                AppError::ServiceUnavailable("分片分页响应无效".into())
                            })?;
                        if marker.as_ref() == Some(&next) {
                            return Err(AppError::ServiceUnavailable("分片分页未前进".into()));
                        }
                        marker = Some(next);
                    }
                    if completed.len() as u64 != count {
                        return Err(AppError::BadRequest("直传分片尚未全部上传".into()));
                    }
                    let _permit = self.acquire_request().await?;
                    self.client
                        .complete_multipart_upload()
                        .bucket(&self.bucket)
                        .key(key)
                        .upload_id(upload_id)
                        .multipart_upload(
                            CompletedMultipartUpload::builder()
                                .set_parts(Some(completed))
                                .build(),
                        )
                        .send()
                        .await
                        .map_err(|_| AppError::ServiceUnavailable("直传分片提交失败".into()))?;
                    return Ok(());
                }
            }
        }
    }
}

fn direct_part_length(size: u64, part_size: u64, number: u64) -> AppResult<u64> {
    let count = size.max(1).div_ceil(part_size);
    if number == 0 || number > count {
        return Err(AppError::BadRequest("分片编号无效".into()));
    }
    Ok(size.saturating_sub((number - 1) * part_size).min(part_size))
}

#[cfg(test)]
mod tests {
    use super::direct_part_length;
    #[test]
    fn declared_parts_have_exact_lengths() {
        assert_eq!(direct_part_length(130, 64, 1).unwrap(), 64);
        assert_eq!(direct_part_length(130, 64, 3).unwrap(), 2);
        assert_eq!(direct_part_length(0, 64, 1).unwrap(), 0);
        assert!(direct_part_length(130, 64, 0).is_err());
        assert!(direct_part_length(130, 64, 4).is_err());
    }
    #[tokio::test]
    async fn administrator_cancellation_aborts_direct_multipart_without_completing_it() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let aborted = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let observed_abort = aborted.clone();
        let observed_complete = completed.clone();
        let router = axum::Router::new().route("/{*key}", axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri| {
                let aborted = observed_abort.clone();
                let completed = observed_complete.clone();
                async move {
                    let query = uri.query().unwrap_or("");
                    let response = axum::http::Response::builder().header("etag", "\"journal\"");
                    if method == axum::http::Method::POST && query.contains("uploads") {
                        response.header("content-type", "application/xml").body(axum::body::Body::from(
                            "<InitiateMultipartUploadResult><UploadId>test-upload</UploadId></InitiateMultipartUploadResult>"))
                    } else if method == axum::http::Method::POST {
                        completed.fetch_add(1, Ordering::SeqCst);
                        response.body(axum::body::Body::empty())
                    } else if method == axum::http::Method::HEAD {
                        response.status(404).body(axum::body::Body::empty())
                    } else if method == axum::http::Method::DELETE {
                        if query.contains("uploadId") { aborted.fetch_add(1, Ordering::SeqCst); }
                        response.status(204).body(axum::body::Body::empty())
                    } else {
                        response.body(axum::body::Body::empty())
                    }.unwrap()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = super::super::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let cancellation = tokio_util::sync::CancellationToken::new();
        let signal = cancellation.clone();
        let (ready, descriptor) = tokio::sync::oneshot::channel();
        let (_sender, commands) = tokio::sync::mpsc::channel(4);
        let mut input = super::UploadInput::relay(axum::body::Body::empty());
        input.direct = Some(super::DirectChannel { ready, commands });
        input.cancellation = Some(cancellation);
        let worker = tokio::spawn(async move {
            backend
                .multipart_upload(
                    "tenant/.ycloud-system/uploads/0123456789abcdef0123456789abcdef",
                    input,
                    128 * 1024 * 1024,
                    None,
                    "0123456789abcdef0123456789abcdef",
                )
                .await
        });
        if descriptor.await.is_err() {
            panic!("multipart setup failed: {:?}", worker.await.unwrap());
        }
        signal.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(aborted.load(Ordering::SeqCst), 1);
        assert_eq!(completed.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn control_channel_signs_parts_and_exits_on_cancellation_without_reading_payloads() {
        let backend = super::super::protocol_tests::test_backend("http://127.0.0.1:1");
        let (ready, descriptor) = tokio::sync::oneshot::channel();
        let (sender, commands) = tokio::sync::mpsc::channel(4);
        let task = tokio::spawn(async move {
            backend
                .receive_direct_parts(
                    "tenant/.ycloud-system/uploads/session",
                    "upload-id",
                    128 * 1024 * 1024,
                    super::DirectChannel { ready, commands },
                )
                .await
        });
        let descriptor = descriptor.await.unwrap().unwrap();
        assert_eq!(descriptor.concurrency, 4);
        assert_eq!(descriptor.part_count, 2);
        let (reply, signed) = tokio::sync::oneshot::channel();
        sender
            .send(super::DirectCommand::Sign(1, reply))
            .await
            .unwrap();
        let signed = signed.await.unwrap().unwrap();
        assert!(signed.url.contains("X-Amz-Signature="));
        assert!(signed.url.contains("partNumber=1"));
        sender.send(super::DirectCommand::Cancel).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn completion_reads_provider_parts_before_submitting_multipart() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let completed = Arc::new(AtomicUsize::new(0));
        let observed = completed.clone();
        let router = axum::Router::new().route("/{*key}", axum::routing::get(|| async {
            ([ (axum::http::header::CONTENT_TYPE, "application/xml") ],
                "<ListPartsResult><IsTruncated>false</IsTruncated><Part><PartNumber>1</PartNumber><ETag>part-etag</ETag><Size>4</Size></Part></ListPartsResult>")
        }).post(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                ([ (axum::http::header::CONTENT_TYPE, "application/xml") ],
                    "<CompleteMultipartUploadResult><ETag>complete-etag</ETag></CompleteMultipartUploadResult>")
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = super::super::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        for (index, size) in [4, 4, 5, 4].into_iter().enumerate() {
            let mut backend = backend.clone();
            if index == 3 {
                backend.upload_timeout = std::time::Duration::from_millis(20);
            }
            let (ready, descriptor) = tokio::sync::oneshot::channel();
            let (sender, commands) = tokio::sync::mpsc::channel(4);
            let worker = tokio::spawn(async move {
                backend
                    .receive_direct_parts(
                        "temporary",
                        "upload-id",
                        size,
                        super::DirectChannel { ready, commands },
                    )
                    .await
            });
            descriptor.await.unwrap().unwrap();
            if index == 0 {
                sender.send(super::DirectCommand::Complete).await.unwrap();
            }
            let retained_sender = if index == 3 {
                Some(sender)
            } else {
                drop(sender);
                None
            };
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_ok(), size == 4);
            drop(retained_sender);
        }
        assert_eq!(completed.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn automatic_result_check_requires_the_matching_task_marker_and_size() {
        let marker = "0123456789abcdef0123456789abcdef";
        let router = axum::Router::new().route(
            "/{*key}",
            axum::routing::head(move || async move {
                axum::http::Response::builder()
                    .header("content-length", "4")
                    .header("etag", "\"etag\"")
                    .header(
                        format!("x-amz-meta-{}", super::super::S3_OPERATION_METADATA_KEY),
                        marker,
                    )
                    .body(axum::body::Body::empty())
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = super::super::protocol_tests::test_backend(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        assert!(backend.upload_committed("file", 4, marker).await.unwrap());
        assert!(!backend.upload_committed("file", 5, marker).await.unwrap());
        assert!(!backend
            .upload_committed("file", 4, "fedcba9876543210fedcba9876543210")
            .await
            .unwrap());
        server.abort();
    }
}
