use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use aws_sdk_s3::types::{CommonPrefix, Object};
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use axum::{
    body::Body,
    http::{HeaderMap, Uri},
};
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use http_body_util::BodyExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use crate::{
    config::{normalize_s3_endpoint, Config, S3AddressingStyle, S3Provider, S3StorageConfig},
    error::{AppError, AppResult},
    storage::FileResponseMode,
};

use super::{
    append_exact_multipart_matches, copy_source, internal_key, is_owned_internal_multipart_key,
    list_prefix, listing::collect_page_entries, multipart_part_size, multipart_session_matches,
    object_key, parent_relative, simple_copy_matches, snapshot_matches,
    uses_oss_native_write_conditions, valid_transaction_id, validate_multipart_session,
    validate_upload_transaction, ExactLengthBody, MultipartUpload, RawS3Metadata, S3Backend,
    S3MultipartPurpose, S3MultipartSession, S3ObjectSnapshot, S3UploadStage, S3UploadTransaction,
    S3_MULTIPART_MAX_PARTS, S3_MULTIPART_MAX_PART_BYTES, S3_MULTIPART_SESSION_SCHEMA_VERSION,
    S3_MULTIPART_THRESHOLD, S3_SINGLE_COPY_LIMIT,
};

const SMOKE_STREAM_CHUNK_BYTES: u64 = 1024 * 1024;
const SMOKE_UPLOAD_PART_QUERY: &[u8] = b"partNumber=1";

struct SmokeFaultProxy {
    endpoint: String,
    armed: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    offline: Arc<AtomicBool>,
    sustain_after_cut: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl SmokeFaultProxy {
    async fn start(target_endpoint: &str) -> AppResult<Self> {
        let uri = target_endpoint.parse::<Uri>().map_err(|error| {
            AppError::with_source("failed to parse fault-proxy target endpoint", error)
        })?;
        if uri.scheme_str() != Some("http") {
            return Err(AppError::ServiceUnavailable(
                "传输层故障代理仅允许隔离环境中的 HTTP Endpoint".into(),
            ));
        }
        let authority = uri.authority().ok_or_else(|| {
            AppError::ServiceUnavailable("传输层故障代理 Endpoint 缺少地址".into())
        })?;
        let port = authority.port_u16().unwrap_or(80);
        let mut targets = tokio::net::lookup_host((authority.host(), port))
            .await
            .map_err(|error| {
                AppError::with_source("failed to resolve fault-proxy target", error)
            })?;
        let target = targets
            .next()
            .ok_or_else(|| AppError::ServiceUnavailable("传输层故障代理无法解析目标地址".into()))?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| AppError::with_source("failed to bind local S3 fault proxy", error))?;
        let local = listener.local_addr().map_err(|error| {
            AppError::with_source("failed to read local S3 fault-proxy address", error)
        })?;
        let armed = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let offline = Arc::new(AtomicBool::new(false));
        let sustain_after_cut = Arc::new(AtomicBool::new(false));
        let task_armed = armed.clone();
        let task_fired = fired.clone();
        let task_offline = offline.clone();
        let task_sustain_after_cut = sustain_after_cut.clone();
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                if task_offline.load(Ordering::SeqCst) {
                    drop(client);
                    continue;
                }
                let connection_armed = task_armed.clone();
                let connection_fired = task_fired.clone();
                let connection_offline = task_offline.clone();
                let connection_sustain_after_cut = task_sustain_after_cut.clone();
                tokio::spawn(async move {
                    let Ok(upstream) = TcpStream::connect(target).await else {
                        return;
                    };
                    let _ = forward_fault_proxy_connection(
                        client,
                        upstream,
                        connection_armed,
                        connection_fired,
                        connection_offline,
                        connection_sustain_after_cut,
                    )
                    .await;
                });
            }
        });
        Ok(Self {
            endpoint: format!("http://{local}"),
            armed,
            fired,
            offline,
            sustain_after_cut,
            task,
        })
    }

    fn arm(&self) {
        self.arm_with_outage(false);
    }

    fn arm_sustained(&self) {
        self.arm_with_outage(true);
    }

    fn arm_with_outage(&self, sustained: bool) {
        self.fired.store(false, Ordering::SeqCst);
        self.offline.store(false, Ordering::SeqCst);
        self.sustain_after_cut.store(sustained, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    fn restore(&self) {
        self.armed.store(false, Ordering::SeqCst);
        self.sustain_after_cut.store(false, Ordering::SeqCst);
        self.offline.store(false, Ordering::SeqCst);
    }
}

impl Drop for SmokeFaultProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn forward_fault_proxy_connection(
    client: TcpStream,
    upstream: TcpStream,
    armed: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    offline: Arc<AtomicBool>,
    sustain_after_cut: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let (mut client_read, mut client_write) = client.into_split();
    let (mut upstream_read, mut upstream_write) = upstream.into_split();
    tokio::select! {
        result = forward_fault_proxy_requests(
            &mut client_read,
            &mut upstream_write,
            &armed,
            &fired,
            &offline,
            &sustain_after_cut,
        ) => result,
        result = tokio::io::copy(&mut upstream_read, &mut client_write) => {
            result.map(|_| ())
        },
    }
}

async fn forward_fault_proxy_requests(
    client: &mut tokio::net::tcp::OwnedReadHalf,
    upstream: &mut tokio::net::tcp::OwnedWriteHalf,
    armed: &AtomicBool,
    fired: &AtomicBool,
    offline: &AtomicBool,
    sustain_after_cut: &AtomicBool,
) -> std::io::Result<()> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut tail = Vec::with_capacity(SMOKE_UPLOAD_PART_QUERY.len().saturating_sub(1));
    let mut bytes_before_cut = None::<usize>;
    loop {
        let read = client.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        if offline.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(remaining) = bytes_before_cut.as_mut() {
            let forward = (*remaining).min(read);
            upstream.write_all(&buffer[..forward]).await?;
            *remaining -= forward;
            if *remaining == 0 {
                fired.store(true, Ordering::SeqCst);
                if sustain_after_cut.load(Ordering::SeqCst) {
                    offline.store(true, Ordering::SeqCst);
                }
                return Ok(());
            }
            continue;
        }

        if armed.load(Ordering::SeqCst) {
            let mut searchable = Vec::with_capacity(tail.len() + read);
            searchable.extend_from_slice(&tail);
            searchable.extend_from_slice(&buffer[..read]);
            if searchable
                .windows(SMOKE_UPLOAD_PART_QUERY.len())
                .any(|window| window == SMOKE_UPLOAD_PART_QUERY)
                && armed
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                upstream.write_all(&buffer[..read]).await?;
                bytes_before_cut = Some(SMOKE_STREAM_CHUNK_BYTES as usize);
                tail.clear();
                continue;
            }
            let keep = SMOKE_UPLOAD_PART_QUERY.len().saturating_sub(1);
            tail.clear();
            tail.extend_from_slice(&searchable[searchable.len().saturating_sub(keep)..]);
        }
        upstream.write_all(&buffer[..read]).await?;
    }
}

#[test]
fn multipart_part_sizing_stays_within_provider_limits() {
    assert_eq!(
        multipart_part_size(64 * 1024 * 1024).unwrap(),
        64 * 1024 * 1024
    );
    let four_tebibytes = 4_u64 * 1024 * 1024 * 1024 * 1024;
    let part_size = multipart_part_size(four_tebibytes).unwrap() as u64;
    assert!(four_tebibytes.div_ceil(part_size) <= S3_MULTIPART_MAX_PARTS);
    assert!(part_size <= S3_MULTIPART_MAX_PART_BYTES);
    assert!(multipart_part_size(6_u64 * 1024 * 1024 * 1024 * 1024).is_err());
}

fn required_smoke_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing required smoke-test variable {name}"))
}

fn smoke_flag(name: &str) -> bool {
    match std::env::var(name).ok().as_deref() {
        None | Some("") | Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(value) => panic!("invalid {name} value {value:?}; expected true, false, 1, or 0"),
    }
}

fn smoke_pattern_body(content_length: u64) -> Body {
    let chunks = stream::unfold(0_u64, move |offset| async move {
        if offset >= content_length {
            return None;
        }
        let length = SMOKE_STREAM_CHUNK_BYTES.min(content_length - offset) as usize;
        let value = ((offset / SMOKE_STREAM_CHUNK_BYTES) % 251) as u8;
        let next = offset + length as u64;
        Some((
            Ok::<_, std::io::Error>(Bytes::from(vec![value; length])),
            next,
        ))
    });
    Body::from_stream(chunks)
}

fn interrupted_smoke_body(bytes_before_failure: u64) -> Body {
    let chunks = stream::unfold(
        (0_u64, false),
        move |(offset, failure_emitted)| async move {
            if failure_emitted {
                return None;
            }
            if offset >= bytes_before_failure {
                return Some((
                    Err::<Bytes, _>(std::io::Error::other(
                        "injected multipart request-body interruption",
                    )),
                    (offset, true),
                ));
            }
            let length = SMOKE_STREAM_CHUNK_BYTES.min(bytes_before_failure - offset) as usize;
            let next = offset + length as u64;
            Some((Ok(Bytes::from(vec![0xA5; length])), (next, false)))
        },
    );
    Body::from_stream(chunks)
}

async fn verify_smoke_pattern_download(
    backend: &S3Backend,
    relative: &str,
    expected_length: u64,
) -> AppResult<()> {
    let response = backend
        .stream_file(relative, &HeaderMap::new(), FileResponseMode::Attachment)
        .await?;
    let mut body = response.into_body().into_data_stream();
    let mut received = 0_u64;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| {
            AppError::with_source("failed to stream multipart smoke download", error)
        })?;
        let mut local_offset = 0_usize;
        while local_offset < chunk.len() {
            let absolute_offset = received + local_offset as u64;
            let expected = ((absolute_offset / SMOKE_STREAM_CHUNK_BYTES) % 251) as u8;
            let until_boundary =
                SMOKE_STREAM_CHUNK_BYTES - (absolute_offset % SMOKE_STREAM_CHUNK_BYTES);
            let length = (until_boundary as usize).min(chunk.len() - local_offset);
            if !chunk[local_offset..local_offset + length]
                .iter()
                .all(|value| *value == expected)
            {
                return Err(AppError::ServiceUnavailable(
                    "对象存储 Multipart 下载内容校验失败".into(),
                ));
            }
            local_offset += length;
        }
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or(AppError::PayloadTooLarge)?;
    }
    if received != expected_length {
        return Err(AppError::ServiceUnavailable(
            "对象存储 Multipart 下载长度校验失败".into(),
        ));
    }
    Ok(())
}

async fn cleanup_smoke_multipart_state(backend: &S3Backend) {
    if let Ok(output) = backend
        .client
        .list_multipart_uploads()
        .bucket(&backend.bucket)
        .prefix(&backend.prefix)
        .max_uploads(1_000)
        .send()
        .await
    {
        let uploads = output
            .uploads()
            .iter()
            .filter_map(|upload| Some((upload.key()?.to_owned(), upload.upload_id()?.to_owned())))
            .collect::<Vec<_>>();
        for (key, upload_id) in uploads {
            let _ = backend.abort_multipart_operation(&key, &upload_id).await;
        }
    }
    if let Ok(keys) = backend.list_multipart_session_keys().await {
        for key in keys {
            let _ = backend.delete_key(&key, None).await;
        }
    }
}

async fn cleanup_smoke_base_multipart_state(backend: &S3Backend, base_prefix: &str) {
    if let Ok(output) = backend
        .client
        .list_multipart_uploads()
        .bucket(&backend.bucket)
        .prefix(base_prefix)
        .max_uploads(1_000)
        .send()
        .await
    {
        let uploads = output
            .uploads()
            .iter()
            .filter_map(|upload| Some((upload.key()?.to_owned(), upload.upload_id()?.to_owned())))
            .collect::<Vec<_>>();
        for (key, upload_id) in uploads {
            let _ = backend.abort_multipart_operation(&key, &upload_id).await;
        }
    }

    if let Ok(output) = backend
        .client
        .list_objects_v2()
        .bucket(&backend.bucket)
        .prefix(base_prefix)
        .max_keys(1_000)
        .send()
        .await
    {
        const MARKER: &str = "/.ycloud-system/multipart-sessions/";
        let keys = output
            .contents()
            .iter()
            .filter_map(|object| object.key())
            .filter(|key| {
                key.rsplit_once(MARKER)
                    .is_some_and(|(_, id)| valid_transaction_id(id))
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for key in keys {
            let _ = backend.delete_key(&key, None).await;
        }
    }
}

fn smoke_provider(value: Option<&str>) -> S3Provider {
    match value.unwrap_or("minio") {
        "alibaba_oss" => S3Provider::AlibabaOss,
        "tencent_cos" => S3Provider::TencentCos,
        "minio" => S3Provider::Minio,
        "s3_compatible" => S3Provider::S3Compatible,
        value => panic!(
            "invalid YCLOUD_S3_SMOKE_PROVIDER {value:?}; expected alibaba_oss, tencent_cos, minio, or s3_compatible"
        ),
    }
}

fn smoke_addressing_style(value: Option<&str>, provider: S3Provider) -> S3AddressingStyle {
    match value {
        Some("path") => S3AddressingStyle::Path,
        Some("virtual_hosted") => S3AddressingStyle::VirtualHosted,
        Some(value) => panic!(
            "invalid YCLOUD_S3_SMOKE_ADDRESSING_STYLE {value:?}; expected path or virtual_hosted"
        ),
        None if matches!(provider, S3Provider::AlibabaOss | S3Provider::TencentCos) => {
            S3AddressingStyle::VirtualHosted
        }
        None => S3AddressingStyle::Path,
    }
}

fn sanitize_smoke_text(mut text: String, secrets: &[&str]) -> String {
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "<redacted>");
    }
    text.chars().take(1_024).collect()
}

fn sanitized_smoke_error_chain(
    error: &(dyn std::error::Error + 'static),
    secrets: &[&str],
) -> String {
    let mut parts = Vec::new();
    let mut current = Some(error);
    while let Some(cause) = current {
        parts.push(sanitize_smoke_text(cause.to_string(), secrets));
        current = cause.source();
    }
    parts.join(" -> ").chars().take(1_024).collect()
}

/// Opt-in compatibility test for an isolated S3-compatible bucket.
///
/// The test never runs in the normal suite and creates a unique Prefix for
/// every execution. Credentials are read only from process environment
/// variables and are never printed. Set `YCLOUD_S3_SMOKE_MULTIPART=1`
/// to add a streamed upload larger than the 64 MiB Multipart threshold,
/// `YCLOUD_S3_SMOKE_CHECK_INVALID_CREDENTIALS=1` verifies that the service
/// rejects a valid access-key ID paired with an invalid secret. Set
/// `YCLOUD_S3_SMOKE_INTERRUPTED_MULTIPART=1` to fail the request body after
/// the first part and verify that Ycloud aborts the Multipart session. Set
/// `YCLOUD_S3_SMOKE_TRANSPORT_CUT=1` for HTTP test endpoints to route the
/// test through a loopback proxy that cuts the first UploadPart connection.
/// `YCLOUD_S3_SMOKE_SUSTAINED_OUTAGE=1` keeps that proxy offline while the
/// immediate Abort fails, restores it, and exercises persisted recovery.
#[tokio::test]
#[ignore = "requires an isolated S3 test bucket and explicit credentials"]
async fn s3_compatibility_smoke() {
    let configured_endpoint = required_smoke_env("YCLOUD_S3_SMOKE_ENDPOINT");
    let bucket = required_smoke_env("YCLOUD_S3_SMOKE_BUCKET");
    let access_key_id = required_smoke_env("YCLOUD_S3_SMOKE_ACCESS_KEY_ID");
    let secret_access_key = required_smoke_env("YCLOUD_S3_SMOKE_SECRET_ACCESS_KEY");
    let provider = smoke_provider(std::env::var("YCLOUD_S3_SMOKE_PROVIDER").ok().as_deref());
    let addressing_style = smoke_addressing_style(
        std::env::var("YCLOUD_S3_SMOKE_ADDRESSING_STYLE")
            .ok()
            .as_deref(),
        provider,
    );
    let region = std::env::var("YCLOUD_S3_SMOKE_REGION").unwrap_or_else(|_| "us-east-1".into());
    let base_prefix =
        std::env::var("YCLOUD_S3_SMOKE_PREFIX").unwrap_or_else(|_| "ycloud-smoke/".into());
    let multipart_length = smoke_flag("YCLOUD_S3_SMOKE_MULTIPART")
        .then_some(S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES);
    let check_invalid_credentials = smoke_flag("YCLOUD_S3_SMOKE_CHECK_INVALID_CREDENTIALS");
    let check_interrupted_multipart = smoke_flag("YCLOUD_S3_SMOKE_INTERRUPTED_MULTIPART");
    let check_transport_cut = smoke_flag("YCLOUD_S3_SMOKE_TRANSPORT_CUT");
    let check_sustained_outage = smoke_flag("YCLOUD_S3_SMOKE_SUSTAINED_OUTAGE");
    let fault_proxy = if check_transport_cut || check_sustained_outage {
        Some(SmokeFaultProxy::start(&configured_endpoint).await.unwrap())
    } else {
        None
    };
    let endpoint = fault_proxy.as_ref().map_or_else(
        || configured_endpoint.clone(),
        |proxy| proxy.endpoint.clone(),
    );
    let large_object_length = multipart_length.or_else(|| {
        (check_interrupted_multipart || check_transport_cut || check_sustained_outage)
            .then_some(S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES)
    });
    let base_prefix = base_prefix.trim_matches('/');
    let smoke_root_prefix = if base_prefix.is_empty() {
        String::new()
    } else {
        format!("{base_prefix}/")
    };
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let prefix = if base_prefix.is_empty() {
        format!("{run_id}/")
    } else {
        format!("{base_prefix}/{run_id}/")
    };
    let capacity_limit = large_object_length
        .and_then(|length| length.checked_mul(4))
        .unwrap_or(1024 * 1024);
    let settings = S3StorageConfig {
        provider,
        endpoint: endpoint.clone(),
        bucket,
        region,
        prefix,
        addressing_style,
        access_key_id,
        secret_access_key,
        capacity_limit_bytes: Some(capacity_limit),
    };
    let runtime = Config {
        bind_address: IpAddr::from([127, 0, 0, 1]),
        port: 0,
        storage_path: PathBuf::from("unused-smoke-local-storage"),
        local_mounts: crate::storage_catalog::LocalMountCatalog::new(
            PathBuf::from("unused-smoke-local-storage"),
            Vec::new(),
        )
        .unwrap(),
        config_path: PathBuf::from("unused-smoke-config.json"),
        max_upload_bytes: large_object_length.unwrap_or(1024 * 1024),
        max_upload_batch_bytes: 100 * 1024 * 1024 * 1024,
        max_upload_batch_entries: 10_000,
        max_archive_bytes: 100 * 1024 * 1024 * 1024,
        max_archive_entries: 100_000,
        io_concurrency: 2,
        max_list_entries: 100,
        request_timeout_secs: 30,
        upload_timeout_secs: if large_object_length.is_some() {
            120
        } else {
            30
        },
        disk_reserve_bytes: 0,
        secure_cookies: false,
        allow_lan_http: true,
        public_base_url: None,
        public_host: None,
        trusted_proxy_ips: HashSet::new(),
        allowed_hosts: HashSet::new(),
        s3_allowed_endpoints: HashSet::from([normalize_s3_endpoint(&endpoint).unwrap()]),
        transaction_auth_key: [0x31; 32],
    };
    let backend = S3Backend::new(&settings, &runtime).unwrap();
    let payload = Bytes::from_static(b"ycloud-s3-compatibility-smoke");

    let result: AppResult<()> = async {
        cleanup_smoke_base_multipart_state(&backend, &smoke_root_prefix).await;
        if check_invalid_credentials {
            let mut invalid_settings = settings.clone();
            invalid_settings.secret_access_key =
                format!("invalid-{}", uuid::Uuid::new_v4().simple());
            let invalid_backend = S3Backend::new(&invalid_settings, &runtime)?;
            if invalid_backend.probe().await.is_ok() {
                return Err(AppError::ServiceUnavailable(
                    "对象存储接受了错误凭据".into(),
                ));
            }
        }
        if let Err(error) = backend
            .client
            .list_objects_v2()
            .bucket(&backend.bucket)
            .prefix(&backend.prefix)
            .max_keys(1)
            .send()
            .await
        {
            let service = error.as_service_error();
            let secrets = [
                settings.access_key_id.as_str(),
                settings.secret_access_key.as_str(),
            ];
            let code = service
                .and_then(ProvideErrorMetadata::code)
                .unwrap_or("transport");
            let message = sanitize_smoke_text(
                service
                    .and_then(ProvideErrorMetadata::message)
                    .unwrap_or("no service message")
                    .to_owned(),
                &secrets,
            );
            let status = error
                .raw_response()
                .map(|response| response.status().as_u16());
            let chain = sanitized_smoke_error_chain(&error, &secrets);
            panic!(
                "S3 list probe failed for {provider:?}: status={status:?}, code={code}, message={message}, cause={chain}"
            );
        }
        backend.activation_probe().await?;
        assert_eq!(backend.user_data_size().await?, 0);
        backend.create_directory("suite").await?;
        let uploaded = backend
            .upload_file(
                "suite/source.bin",
                Body::from(payload.clone()),
                payload.len() as u64,
                1024 * 1024,
                Some("application/octet-stream"),
            )
            .await?;
        assert_eq!(uploaded.previous_size, 0);
        assert_eq!(uploaded.size, payload.len() as u64);

        let response = backend
            .stream_file(
                "suite/source.bin",
                &HeaderMap::new(),
                FileResponseMode::Attachment,
            )
            .await?;
        let downloaded = response
            .into_body()
            .collect()
            .await
            .map_err(|error| AppError::with_source("failed to collect smoke download", error))?
            .to_bytes();
        assert_eq!(downloaded, payload);

        backend
            .copy_file("suite/source.bin", "suite/copied.bin")
            .await?;
        backend
            .move_file("suite/copied.bin", "suite/moved.bin")
            .await?;
        assert_eq!(
            backend.delete_file("suite/moved.bin").await?,
            payload.len() as u64
        );

        let replacement = Bytes::from_static(b"replacement");
        let replaced = backend
            .upload_file(
                "suite/source.bin",
                Body::from(replacement.clone()),
                replacement.len() as u64,
                1024 * 1024,
                Some("application/octet-stream"),
            )
            .await?;
        assert_eq!(replaced.previous_size, payload.len() as u64);
        assert_eq!(replaced.size, replacement.len() as u64);

        backend.copy_directory("suite", "suite-copy").await?;
        backend.move_directory("suite-copy", "suite-moved").await?;
        assert_eq!(
            backend.delete_directory("suite-moved").await?,
            replacement.len() as u64
        );
        assert_eq!(
            backend.delete_directory("suite").await?,
            replacement.len() as u64
        );
        assert_eq!(backend.user_data_size().await?, 0);

        if let Some(content_length) = multipart_length {
            backend.create_directory("multipart").await?;
            let uploaded = backend
                .upload_file(
                    "multipart/source.bin",
                    smoke_pattern_body(content_length),
                    content_length,
                    content_length,
                    Some("application/octet-stream"),
                )
                .await?;
            assert_eq!(uploaded.previous_size, 0);
            assert_eq!(uploaded.size, content_length);
            verify_smoke_pattern_download(
                &backend,
                "multipart/source.bin",
                content_length,
            )
            .await?;

            backend
                .copy_file("multipart/source.bin", "multipart/copied.bin")
                .await?;
            backend
                .move_file("multipart/copied.bin", "multipart/moved.bin")
                .await?;
            assert_eq!(
                backend.delete_file("multipart/moved.bin").await?,
                content_length
            );
            assert_eq!(
                backend.delete_file("multipart/source.bin").await?,
                content_length
            );
            assert_eq!(backend.delete_directory("multipart").await?, 0);
            assert_eq!(backend.user_data_size().await?, 0);
        }

        if check_interrupted_multipart {
            let content_length = S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES;
            backend.create_directory("interrupted").await?;
            let upload = backend
                .upload_file(
                    "interrupted/source.bin",
                    interrupted_smoke_body(S3_MULTIPART_THRESHOLD),
                    content_length,
                    content_length,
                    Some("application/octet-stream"),
                )
                .await;
            if upload.is_ok() {
                return Err(AppError::ServiceUnavailable(
                    "对象存储接受了中途断流的 Multipart 上传".into(),
                ));
            }
            match backend.metadata("interrupted/source.bin").await {
                Err(AppError::NotFound) => {}
                Ok(_) => {
                    return Err(AppError::ServiceUnavailable(
                        "中途断流后目标对象仍然可见".into(),
                    ));
                }
                Err(error) => return Err(error),
            }
            let pending = backend
                .client
                .list_multipart_uploads()
                .bucket(&backend.bucket)
                .prefix(&backend.prefix)
                .max_uploads(1)
                .send()
                .await
                .map_err(|error| {
                    AppError::with_source(
                        "failed to inspect interrupted multipart uploads",
                        error,
                    )
                })?;
            if !pending.uploads().is_empty() {
                return Err(AppError::ServiceUnavailable(
                    "中途断流后遗留了未完成的 Multipart 会话".into(),
                ));
            }
            assert_eq!(backend.delete_directory("interrupted").await?, 0);
            assert_eq!(backend.user_data_size().await?, 0);
        }

        if check_transport_cut {
            let proxy = fault_proxy.as_ref().expect("fault proxy is configured");
            let content_length = S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES;
            backend.create_directory("transport-cut").await?;
            proxy.arm();
            let upload = backend
                .upload_file(
                    "transport-cut/source.bin",
                    smoke_pattern_body(content_length),
                    content_length,
                    content_length,
                    Some("application/octet-stream"),
                )
                .await;
            if upload.is_ok() || !proxy.fired() {
                return Err(AppError::ServiceUnavailable(
                    "传输层故障代理没有中断 Multipart 上传".into(),
                ));
            }
            match backend.metadata("transport-cut/source.bin").await {
                Err(AppError::NotFound) => {}
                Ok(_) => {
                    return Err(AppError::ServiceUnavailable(
                        "传输层中断后目标对象仍然可见".into(),
                    ));
                }
                Err(error) => return Err(error),
            }
            let pending = backend
                .client
                .list_multipart_uploads()
                .bucket(&backend.bucket)
                .prefix(&backend.prefix)
                .max_uploads(1)
                .send()
                .await
                .map_err(|error| {
                    AppError::with_source(
                        "failed to inspect transport-cut multipart uploads",
                        error,
                    )
                })?;
            if !pending.uploads().is_empty() {
                return Err(AppError::ServiceUnavailable(
                    "传输层中断后遗留了未完成的 Multipart 会话".into(),
                ));
            }
            assert_eq!(backend.delete_directory("transport-cut").await?, 0);
            assert_eq!(backend.user_data_size().await?, 0);
        }

        if check_sustained_outage {
            let proxy = fault_proxy.as_ref().expect("fault proxy is configured");
            let content_length = S3_MULTIPART_THRESHOLD + SMOKE_STREAM_CHUNK_BYTES;
            backend.create_directory("sustained-outage").await?;
            proxy.arm_sustained();
            let upload = backend
                .upload_file(
                    "sustained-outage/source.bin",
                    smoke_pattern_body(content_length),
                    content_length,
                    content_length,
                    Some("application/octet-stream"),
                )
                .await;
            let fault_fired = proxy.fired();
            proxy.restore();
            if upload.is_ok() || !fault_fired {
                return Err(AppError::ServiceUnavailable(
                    "持续断网代理没有中断 Multipart 上传".into(),
                ));
            }
            let recovered = backend.recover_transactions().await?;
            if recovered == 0 {
                return Err(AppError::ServiceUnavailable(
                    "持续断网后没有发现持久化的 Multipart 恢复记录".into(),
                ));
            }
            match backend.metadata("sustained-outage/source.bin").await {
                Err(AppError::NotFound) => {}
                Ok(_) => {
                    return Err(AppError::ServiceUnavailable(
                        "持续断网恢复后目标对象仍然可见".into(),
                    ));
                }
                Err(error) => return Err(error),
            }
            let pending = backend
                .client
                .list_multipart_uploads()
                .bucket(&backend.bucket)
                .prefix(&backend.prefix)
                .max_uploads(1)
                .send()
                .await
                .map_err(|error| {
                    AppError::with_source(
                        "failed to inspect sustained-outage multipart uploads",
                        error,
                    )
                })?;
            if !pending.uploads().is_empty() {
                return Err(AppError::ServiceUnavailable(
                    "持续断网恢复后仍遗留未完成的 Multipart 会话".into(),
                ));
            }
            assert_eq!(backend.delete_directory("sustained-outage").await?, 0);
            assert_eq!(backend.user_data_size().await?, 0);
        }
        Ok(())
    }
    .await;

    // Preserve transaction evidence on ambiguous failures, but clean all
    // ordinary test paths so successful and simple failed runs do not
    // contaminate later capacity scans.
    let _ = backend.recover_transactions().await;
    cleanup_smoke_multipart_state(&backend).await;
    for path in [
        "sustained-outage",
        "transport-cut",
        "interrupted",
        "multipart",
        "suite-moved",
        "suite-copy",
        "suite",
    ] {
        let _ = backend.delete_directory(path).await;
    }
    result.unwrap();
}

#[test]
fn smoke_defaults_match_provider_addressing_requirements() {
    assert_eq!(smoke_provider(None), S3Provider::Minio);
    assert_eq!(
        smoke_addressing_style(None, S3Provider::Minio),
        S3AddressingStyle::Path
    );
    assert_eq!(
        smoke_addressing_style(None, S3Provider::TencentCos),
        S3AddressingStyle::VirtualHosted
    );
    assert_eq!(
        smoke_addressing_style(None, S3Provider::AlibabaOss),
        S3AddressingStyle::VirtualHosted
    );
    assert!(uses_oss_native_write_conditions(S3Provider::AlibabaOss));
    assert!(!uses_oss_native_write_conditions(S3Provider::TencentCos));
    assert!(!uses_oss_native_write_conditions(S3Provider::Minio));
    assert!(!uses_oss_native_write_conditions(S3Provider::S3Compatible));
}

#[test]
fn object_keys_are_confined_below_the_configured_prefix() {
    let prefix = "users/yogrut/";
    assert_eq!(
        object_key(prefix, "documents/report.pdf").unwrap(),
        "users/yogrut/documents/report.pdf"
    );
    assert_eq!(
        list_prefix(prefix, "documents").unwrap(),
        "users/yogrut/documents/"
    );
    assert_eq!(list_prefix(prefix, "/").unwrap(), "users/yogrut/");
    assert!(object_key(prefix, "../outside").is_err());
    assert!(object_key(prefix, ".ycloud-system/journal").is_err());
}

#[test]
fn listing_only_exposes_direct_children_below_the_prefix() {
    let prefixes = vec![CommonPrefix::builder()
        .prefix("users/yogrut/photos/")
        .build()];
    let objects = vec![
        Object::builder()
            .key("users/yogrut/report.pdf")
            .size(42)
            .build(),
        Object::builder()
            .key("users/yogrut/nested/hidden.txt")
            .size(8)
            .build(),
        Object::builder().key("outside/secret.txt").size(12).build(),
    ];
    let mut entries = BTreeMap::new();
    collect_page_entries("", "users/yogrut/", &prefixes, &objects, &mut entries).unwrap();

    assert_eq!(entries.len(), 2);
    assert!(entries.get("photos").unwrap().is_dir);
    assert_eq!(entries.get("report.pdf").unwrap().size, 42);
    assert!(!entries.contains_key("secret.txt"));
    assert!(!entries.contains_key("hidden.txt"));
}

#[test]
fn listing_rejects_file_and_directory_name_collisions() {
    let prefixes = vec![CommonPrefix::builder()
        .prefix("users/yogrut/archive/")
        .build()];
    let objects = vec![Object::builder()
        .key("users/yogrut/archive")
        .size(1)
        .build()];
    let mut entries = BTreeMap::new();
    assert!(collect_page_entries("", "users/yogrut/", &prefixes, &objects, &mut entries,).is_err());
}

#[test]
fn listing_hides_reserved_system_names() {
    let prefixes = vec![CommonPrefix::builder()
        .prefix("users/yogrut/.ycloud-system/")
        .build()];
    let mut entries = BTreeMap::new();
    collect_page_entries("", "users/yogrut/", &prefixes, &[], &mut entries).unwrap();
    assert!(entries.is_empty());
}

#[tokio::test]
async fn upload_stream_enforces_exact_content_length() {
    let complete = stream::iter(vec![
        Ok::<_, std::io::Error>(Bytes::from_static(b"abc")),
        Ok(Bytes::from_static(b"def")),
    ]);
    let result = ExactLengthBody::new(complete, 6).collect().await;
    assert_eq!(result.unwrap().to_bytes(), Bytes::from_static(b"abcdef"));

    let short = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))]);
    assert!(ExactLengthBody::new(short, 4).collect().await.is_err());

    let long = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"abcd"))]);
    assert!(ExactLengthBody::new(long, 3).collect().await.is_err());
}

#[test]
fn transaction_keys_are_hidden_and_copy_sources_are_encoded() {
    assert_eq!(
        internal_key("users/yogrut/", "uploads", "1234"),
        "users/yogrut/.ycloud-system/uploads/1234"
    );
    assert_eq!(
        copy_source("bucket", "users/yogrut/游戏 备份.zip"),
        "bucket/users/yogrut/%E6%B8%B8%E6%88%8F%20%E5%A4%87%E4%BB%BD.zip"
    );
}

#[test]
fn parent_paths_remain_relative_to_the_storage_prefix() {
    assert_eq!(parent_relative("file.txt"), "");
    assert_eq!(parent_relative("folder/file.txt"), "folder");
    assert_eq!(parent_relative("a/b/file.txt"), "a/b");
}

#[test]
fn transaction_records_are_confined_and_require_etags() {
    let transaction = S3UploadTransaction {
        schema_version: 1,
        id: "0123456789abcdef0123456789abcdef".into(),
        relative: "folder/file.bin".into(),
        stage: S3UploadStage::Prepared,
        temporary: S3ObjectSnapshot {
            size: 42,
            etag: Some("new".into()),
        },
        previous: Some(S3ObjectSnapshot {
            size: 21,
            etag: Some("old".into()),
        }),
    };
    let key = internal_key("tenant/", "transactions", &transaction.id);
    assert!(validate_upload_transaction("tenant/", &key, &transaction).is_ok());
    assert!(valid_transaction_id(&transaction.id));

    let mut escaped = transaction.clone();
    escaped.relative = "../outside".into();
    assert!(validate_upload_transaction("tenant/", &key, &escaped).is_err());

    let mut unverifiable = transaction.clone();
    unverifiable.temporary.etag = None;
    assert!(validate_upload_transaction("tenant/", &key, &unverifiable).is_err());
}

#[test]
fn multipart_recovery_records_are_confined_to_the_backend_namespace() {
    let id = "0123456789abcdef0123456789abcdef";
    let mut session = S3MultipartSession {
        schema_version: 1,
        id: id.into(),
        key: format!("tenant/.ycloud-system/uploads/{id}"),
        upload_id: Some("provider-upload-id".into()),
        purpose: None,
        expected_size: None,
    };
    let journal_key = internal_key("tenant/", "multipart-sessions", id);
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

    session.upload_id = None;
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
    session.schema_version = S3_MULTIPART_SESSION_SCHEMA_VERSION;
    session.purpose = Some(S3MultipartPurpose::Upload);
    session.expected_size = Some(S3_MULTIPART_THRESHOLD);
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());
    session.upload_id = Some("provider-upload-id".into());
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

    session.key = "tenant/folder/file.bin".into();
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
    session.purpose = Some(S3MultipartPurpose::Copy);
    session.expected_size = Some(S3_SINGLE_COPY_LIMIT + 1);
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

    session.key = format!("tenant/.ycloud-system/backups/{id}");
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

    session.key = "other-tenant/file.bin".into();
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());

    session.key = "tenant/.ycloud-system/transactions/forged".into();
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());

    session.key = "tenant/folder/file.bin".into();
    session.upload_id = Some("bad\nupload-id".into());
    assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
}

#[test]
fn multipart_intent_recovery_selects_only_the_exact_recorded_key() {
    let uploads = [
        MultipartUpload::builder()
            .key("tenant/folder/file.bin")
            .upload_id("ours-a")
            .build(),
        MultipartUpload::builder()
            .key("tenant/folder/file.bin.extra")
            .upload_id("not-ours")
            .build(),
        MultipartUpload::builder()
            .key("tenant/folder/file.bin")
            .upload_id("ours-b")
            .build(),
    ];
    let mut matches = Vec::new();
    append_exact_multipart_matches("tenant/folder/file.bin", &uploads, &mut matches).unwrap();
    assert_eq!(matches, ["ours-a", "ours-b"]);
}

#[test]
fn recovery_snapshots_match_both_size_and_etag() {
    let metadata = RawS3Metadata {
        size: 42,
        etag: Some("etag-a".into()),
        content_type: Some("application/octet-stream".into()),
        operation_id: None,
    };
    assert!(snapshot_matches(
        &metadata,
        &S3ObjectSnapshot {
            size: 42,
            etag: Some("etag-a".into()),
        }
    ));
    assert!(!snapshot_matches(
        &metadata,
        &S3ObjectSnapshot {
            size: 42,
            etag: Some("etag-b".into()),
        }
    ));
    assert!(!snapshot_matches(
        &metadata,
        &S3ObjectSnapshot {
            size: 41,
            etag: Some("etag-a".into()),
        }
    ));
}

#[test]
fn copy_result_proof_requires_the_expected_identity() {
    let source = RawS3Metadata {
        size: 42,
        etag: Some("etag-a".into()),
        content_type: Some("text/plain".into()),
        operation_id: None,
    };
    let mut destination = source.clone();
    assert!(simple_copy_matches(&destination, &source));

    destination.size = 43;
    assert!(!simple_copy_matches(&destination, &source));
    destination.size = 42;
    destination.etag = Some("etag-b".into());
    assert!(!simple_copy_matches(&destination, &source));
    destination.etag = None;
    assert!(!simple_copy_matches(&destination, &source));
}

#[test]
fn multipart_result_proof_requires_marker_size_and_etag() {
    let id = "0123456789abcdef0123456789abcdef";
    let session = S3MultipartSession {
        schema_version: S3_MULTIPART_SESSION_SCHEMA_VERSION,
        id: id.into(),
        key: "tenant/folder/file.bin".into(),
        upload_id: Some("provider-upload-id".into()),
        purpose: Some(S3MultipartPurpose::Copy),
        expected_size: Some(S3_SINGLE_COPY_LIMIT + 1),
    };
    let mut metadata = RawS3Metadata {
        size: S3_SINGLE_COPY_LIMIT + 1,
        etag: Some("multipart-etag".into()),
        content_type: Some("application/octet-stream".into()),
        operation_id: Some(id.into()),
    };
    assert!(multipart_session_matches(&metadata, &session));

    metadata.operation_id = Some("fedcba9876543210fedcba9876543210".into());
    assert!(!multipart_session_matches(&metadata, &session));
    metadata.operation_id = Some(id.into());
    metadata.size += 1;
    assert!(!multipart_session_matches(&metadata, &session));
    metadata.size -= 1;
    metadata.etag = None;
    assert!(!multipart_session_matches(&metadata, &session));

    let mut internal = session;
    internal.key = format!("tenant/.ycloud-system/backups/{id}");
    assert!(is_owned_internal_multipart_key("tenant/", &internal));
    internal.key = "tenant/folder/file.bin".into();
    assert!(!is_owned_internal_multipart_key("tenant/", &internal));
}
