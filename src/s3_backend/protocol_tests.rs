use std::{
    collections::HashSet,
    net::IpAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::body::Body;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use super::{
    authenticated_journal, S3Backend, S3MultipartPurpose, S3MultipartSession, S3ObjectSnapshot,
    S3UploadStage, S3UploadTransaction, S3_ACTIVATION_PROBE_JOURNAL_PURPOSE,
    S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE, S3_MULTIPART_SESSION_JOURNAL_PURPOSE,
    S3_MULTIPART_SESSION_SCHEMA_VERSION, S3_MULTIPART_THRESHOLD, S3_OPERATION_METADATA_KEY,
    S3_SINGLE_COPY_LIMIT,
};
use crate::{
    config::{normalize_s3_endpoint, Config, S3AddressingStyle, S3Provider, S3StorageConfig},
    error::CommitState,
};

const SOURCE_ETAG: &str = "\"source-etag\"";
const SOURCE_LENGTH: usize = 7;
const MULTIPART_ETAG: &str = "\"multipart-etag-65\"";
const INTERNAL_INTENT_ID: &str = "0123456789abcdef0123456789abcdef";

struct CopyResponseLossSimulator {
    endpoint: String,
    destination_etag: &'static str,
    committed: Arc<AtomicBool>,
    copy_attempts: Arc<AtomicUsize>,
    destination_checks: Arc<AtomicUsize>,
    source_present: Arc<AtomicBool>,
    move_journal_present: Arc<AtomicBool>,
    move_journal_writes: Arc<AtomicUsize>,
    move_journal_deletes: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
struct CopyHandlerState {
    destination_etag: &'static str,
    committed: Arc<AtomicBool>,
    copy_attempts: Arc<AtomicUsize>,
    destination_checks: Arc<AtomicUsize>,
    source_present: Arc<AtomicBool>,
    move_journal_present: Arc<AtomicBool>,
    move_journal_writes: Arc<AtomicUsize>,
    move_journal_deletes: Arc<AtomicUsize>,
}

impl CopyResponseLossSimulator {
    async fn start(destination_etag: &'static str) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let committed = Arc::new(AtomicBool::new(false));
        let copy_attempts = Arc::new(AtomicUsize::new(0));
        let destination_checks = Arc::new(AtomicUsize::new(0));
        let source_present = Arc::new(AtomicBool::new(true));
        let move_journal_present = Arc::new(AtomicBool::new(false));
        let move_journal_writes = Arc::new(AtomicUsize::new(0));
        let move_journal_deletes = Arc::new(AtomicUsize::new(0));
        let handler_state = CopyHandlerState {
            destination_etag,
            committed: committed.clone(),
            copy_attempts: copy_attempts.clone(),
            destination_checks: destination_checks.clone(),
            source_present: source_present.clone(),
            move_journal_present: move_journal_present.clone(),
            move_journal_writes: move_journal_writes.clone(),
            move_journal_deletes: move_journal_deletes.clone(),
        };
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let handler_state = handler_state.clone();
                tokio::spawn(async move {
                    handle_connection(stream, handler_state).await;
                });
            }
        });
        Self {
            endpoint,
            destination_etag,
            committed,
            copy_attempts,
            destination_checks,
            source_present,
            move_journal_present,
            move_journal_writes,
            move_journal_deletes,
            task,
        }
    }
}

impl Drop for CopyResponseLossSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct MultipartResponseLossSimulator {
    endpoint: String,
    completed: Arc<AtomicBool>,
    content_type_preserved: Arc<AtomicBool>,
    copied_parts: Arc<AtomicUsize>,
    journal_deletes: Arc<AtomicUsize>,
    operation_id: Arc<Mutex<Option<String>>>,
    task: JoinHandle<()>,
}

impl MultipartResponseLossSimulator {
    async fn start(marker_matches: bool) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let completed = Arc::new(AtomicBool::new(false));
        let content_type_preserved = Arc::new(AtomicBool::new(false));
        let copied_parts = Arc::new(AtomicUsize::new(0));
        let journal_deletes = Arc::new(AtomicUsize::new(0));
        let session_present = Arc::new(AtomicBool::new(false));
        let operation_id = Arc::new(Mutex::new(None));
        let task_completed = completed.clone();
        let task_content_type_preserved = content_type_preserved.clone();
        let task_copied_parts = copied_parts.clone();
        let task_journal_deletes = journal_deletes.clone();
        let task_session_present = session_present.clone();
        let task_operation_id = operation_id.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let completed = task_completed.clone();
                let content_type_preserved = task_content_type_preserved.clone();
                let copied_parts = task_copied_parts.clone();
                let journal_deletes = task_journal_deletes.clone();
                let session_present = task_session_present.clone();
                let operation_id = task_operation_id.clone();
                tokio::spawn(async move {
                    handle_multipart_connection(
                        stream,
                        marker_matches,
                        &completed,
                        &content_type_preserved,
                        &copied_parts,
                        &journal_deletes,
                        &operation_id,
                        &session_present,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            completed,
            content_type_preserved,
            copied_parts,
            journal_deletes,
            operation_id,
            task,
        }
    }
}

impl Drop for MultipartResponseLossSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct RecoveryInventorySimulator {
    endpoint: String,
    list_requests: Arc<AtomicUsize>,
    delete_requests: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl RecoveryInventorySimulator {
    async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let list_requests = Arc::new(AtomicUsize::new(0));
        let delete_requests = Arc::new(AtomicUsize::new(0));
        let task_list_requests = list_requests.clone();
        let task_delete_requests = delete_requests.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let list_requests = task_list_requests.clone();
                let delete_requests = task_delete_requests.clone();
                tokio::spawn(async move {
                    handle_recovery_inventory_connection(stream, &list_requests, &delete_requests)
                        .await;
                });
            }
        });
        Self {
            endpoint,
            list_requests,
            delete_requests,
            task,
        }
    }
}

impl Drop for RecoveryInventorySimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct InternalUploadIntentRecoverySimulator {
    endpoint: String,
    object_present: Arc<AtomicBool>,
    intent_present: Arc<AtomicBool>,
    marker_seen: Arc<AtomicBool>,
    object_deletes: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl InternalUploadIntentRecoverySimulator {
    async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let object_present = Arc::new(AtomicBool::new(false));
        let intent_present = Arc::new(AtomicBool::new(false));
        let marker_seen = Arc::new(AtomicBool::new(false));
        let object_deletes = Arc::new(AtomicUsize::new(0));
        let task_object_present = object_present.clone();
        let task_intent_present = intent_present.clone();
        let task_marker_seen = marker_seen.clone();
        let task_object_deletes = object_deletes.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let object_present = task_object_present.clone();
                let intent_present = task_intent_present.clone();
                let marker_seen = task_marker_seen.clone();
                let object_deletes = task_object_deletes.clone();
                tokio::spawn(async move {
                    handle_internal_upload_intent_connection(
                        stream,
                        &object_present,
                        &intent_present,
                        &marker_seen,
                        &object_deletes,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            object_present,
            intent_present,
            marker_seen,
            object_deletes,
            task,
        }
    }
}

impl Drop for InternalUploadIntentRecoverySimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct UploadCleanupFailureSimulator {
    endpoint: String,
    temporary_delete_attempts: Arc<AtomicUsize>,
    journal_delete_attempts: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

struct IntentPutResponseLossSimulator {
    endpoint: String,
    intent_present: Arc<AtomicBool>,
    intent_put_attempts: Arc<AtomicUsize>,
    object_put_attempts: Arc<AtomicUsize>,
    intent_delete_attempts: Arc<AtomicUsize>,
    intent_id: Arc<Mutex<Option<String>>>,
    task: JoinHandle<()>,
}

impl IntentPutResponseLossSimulator {
    async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let intent_present = Arc::new(AtomicBool::new(false));
        let intent_put_attempts = Arc::new(AtomicUsize::new(0));
        let object_put_attempts = Arc::new(AtomicUsize::new(0));
        let intent_delete_attempts = Arc::new(AtomicUsize::new(0));
        let intent_id = Arc::new(Mutex::new(None));
        let task_intent_present = intent_present.clone();
        let task_intent_put_attempts = intent_put_attempts.clone();
        let task_object_put_attempts = object_put_attempts.clone();
        let task_intent_delete_attempts = intent_delete_attempts.clone();
        let task_intent_id = intent_id.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let intent_present = task_intent_present.clone();
                let intent_put_attempts = task_intent_put_attempts.clone();
                let object_put_attempts = task_object_put_attempts.clone();
                let intent_delete_attempts = task_intent_delete_attempts.clone();
                let intent_id = task_intent_id.clone();
                tokio::spawn(async move {
                    handle_intent_put_response_loss_connection(
                        stream,
                        &intent_present,
                        &intent_put_attempts,
                        &object_put_attempts,
                        &intent_delete_attempts,
                        &intent_id,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            intent_present,
            intent_put_attempts,
            object_put_attempts,
            intent_delete_attempts,
            intent_id,
            task,
        }
    }
}

impl Drop for IntentPutResponseLossSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Copy)]
enum MultipartRecoveryFault {
    HeadUnavailable,
    AbortUnavailable,
    PaginationStuck,
}

struct MultipartRecoveryFaultSimulator {
    endpoint: String,
    abort_attempts: Arc<AtomicUsize>,
    journal_deletes: Arc<AtomicUsize>,
    list_attempts: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl MultipartRecoveryFaultSimulator {
    async fn start(fault: MultipartRecoveryFault) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let abort_attempts = Arc::new(AtomicUsize::new(0));
        let journal_deletes = Arc::new(AtomicUsize::new(0));
        let list_attempts = Arc::new(AtomicUsize::new(0));
        let task_abort_attempts = abort_attempts.clone();
        let task_journal_deletes = journal_deletes.clone();
        let task_list_attempts = list_attempts.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let abort_attempts = task_abort_attempts.clone();
                let journal_deletes = task_journal_deletes.clone();
                let list_attempts = task_list_attempts.clone();
                tokio::spawn(async move {
                    handle_multipart_recovery_fault_connection(
                        stream,
                        fault,
                        &abort_attempts,
                        &journal_deletes,
                        &list_attempts,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            abort_attempts,
            journal_deletes,
            list_attempts,
            task,
        }
    }
}

impl Drop for MultipartRecoveryFaultSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct MultipartCreateResponseLossSimulator {
    endpoint: String,
    operation_id: Arc<Mutex<Option<String>>>,
    intent_present: Arc<AtomicBool>,
    session_present: Arc<AtomicBool>,
    provider_session_present: Arc<AtomicBool>,
    create_attempts: Arc<AtomicUsize>,
    part_upload_attempts: Arc<AtomicUsize>,
    abort_attempts: Arc<AtomicUsize>,
    session_deletes: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl MultipartCreateResponseLossSimulator {
    async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let operation_id = Arc::new(Mutex::new(None));
        let intent_present = Arc::new(AtomicBool::new(false));
        let session_present = Arc::new(AtomicBool::new(false));
        let provider_session_present = Arc::new(AtomicBool::new(false));
        let create_attempts = Arc::new(AtomicUsize::new(0));
        let part_upload_attempts = Arc::new(AtomicUsize::new(0));
        let abort_attempts = Arc::new(AtomicUsize::new(0));
        let session_deletes = Arc::new(AtomicUsize::new(0));
        let task_operation_id = operation_id.clone();
        let task_intent_present = intent_present.clone();
        let task_session_present = session_present.clone();
        let task_provider_session_present = provider_session_present.clone();
        let task_create_attempts = create_attempts.clone();
        let task_part_upload_attempts = part_upload_attempts.clone();
        let task_abort_attempts = abort_attempts.clone();
        let task_session_deletes = session_deletes.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let operation_id = task_operation_id.clone();
                let intent_present = task_intent_present.clone();
                let session_present = task_session_present.clone();
                let provider_session_present = task_provider_session_present.clone();
                let create_attempts = task_create_attempts.clone();
                let part_upload_attempts = task_part_upload_attempts.clone();
                let abort_attempts = task_abort_attempts.clone();
                let session_deletes = task_session_deletes.clone();
                tokio::spawn(async move {
                    handle_multipart_create_response_loss_connection(
                        stream,
                        &operation_id,
                        &intent_present,
                        &session_present,
                        &provider_session_present,
                        &create_attempts,
                        &part_upload_attempts,
                        &abort_attempts,
                        &session_deletes,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            operation_id,
            intent_present,
            session_present,
            provider_session_present,
            create_attempts,
            part_upload_attempts,
            abort_attempts,
            session_deletes,
            task,
        }
    }
}

impl Drop for MultipartCreateResponseLossSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct ActivationProbeSimulator {
    endpoint: String,
    operation_id: Arc<Mutex<Option<String>>>,
    journal_present: Arc<AtomicBool>,
    source_present: Arc<AtomicBool>,
    copy_present: Arc<AtomicBool>,
    marker_seen: Arc<AtomicBool>,
    copy_attempts: Arc<AtomicUsize>,
    copy_delete_attempts: Arc<AtomicUsize>,
    source_delete_attempts: Arc<AtomicUsize>,
    journal_deletes: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl ActivationProbeSimulator {
    async fn start(pending: bool) -> Self {
        Self::start_with_fault(pending, false).await
    }

    async fn start_with_conditional_create_failure() -> Self {
        Self::start_with_fault(false, true).await
    }

    async fn start_with_fault(pending: bool, fail_conditional_create: bool) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let operation_id = Arc::new(Mutex::new(pending.then(|| INTERNAL_INTENT_ID.to_owned())));
        let journal_present = Arc::new(AtomicBool::new(pending));
        let source_present = Arc::new(AtomicBool::new(pending));
        let copy_present = Arc::new(AtomicBool::new(pending));
        let marker_seen = Arc::new(AtomicBool::new(false));
        let copy_attempts = Arc::new(AtomicUsize::new(0));
        let copy_delete_attempts = Arc::new(AtomicUsize::new(0));
        let source_delete_attempts = Arc::new(AtomicUsize::new(0));
        let journal_deletes = Arc::new(AtomicUsize::new(0));
        let task_operation_id = operation_id.clone();
        let task_journal_present = journal_present.clone();
        let task_source_present = source_present.clone();
        let task_copy_present = copy_present.clone();
        let task_marker_seen = marker_seen.clone();
        let task_copy_attempts = copy_attempts.clone();
        let task_copy_delete_attempts = copy_delete_attempts.clone();
        let task_source_delete_attempts = source_delete_attempts.clone();
        let task_journal_deletes = journal_deletes.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let operation_id = task_operation_id.clone();
                let journal_present = task_journal_present.clone();
                let source_present = task_source_present.clone();
                let copy_present = task_copy_present.clone();
                let marker_seen = task_marker_seen.clone();
                let copy_attempts = task_copy_attempts.clone();
                let copy_delete_attempts = task_copy_delete_attempts.clone();
                let source_delete_attempts = task_source_delete_attempts.clone();
                let journal_deletes = task_journal_deletes.clone();
                tokio::spawn(async move {
                    handle_activation_probe_connection(
                        stream,
                        &operation_id,
                        &journal_present,
                        &source_present,
                        &copy_present,
                        &marker_seen,
                        &copy_attempts,
                        &copy_delete_attempts,
                        &source_delete_attempts,
                        &journal_deletes,
                        fail_conditional_create,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            operation_id,
            journal_present,
            source_present,
            copy_present,
            marker_seen,
            copy_attempts,
            copy_delete_attempts,
            source_delete_attempts,
            journal_deletes,
            task,
        }
    }
}

impl Drop for ActivationProbeSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl UploadCleanupFailureSimulator {
    async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let temporary_delete_attempts = Arc::new(AtomicUsize::new(0));
        let journal_delete_attempts = Arc::new(AtomicUsize::new(0));
        let task_temporary_deletes = temporary_delete_attempts.clone();
        let task_journal_deletes = journal_delete_attempts.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let temporary_deletes = task_temporary_deletes.clone();
                let journal_deletes = task_journal_deletes.clone();
                tokio::spawn(async move {
                    handle_upload_cleanup_failure_connection(
                        stream,
                        &temporary_deletes,
                        &journal_deletes,
                    )
                    .await;
                });
            }
        });
        Self {
            endpoint,
            temporary_delete_attempts,
            journal_delete_attempts,
            task,
        }
    }
}

impl Drop for UploadCleanupFailureSimulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_connection(mut stream: TcpStream, state: CopyHandlerState) {
    let Some((method, path, headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = path
        .split_once('?')
        .map_or((path.as_str(), ""), |(path, query)| (path, query));
    let response = match (method.as_str(), path) {
        ("HEAD", "/bucket/tenant/source.txt") if state.source_present.load(Ordering::SeqCst) => {
            object_response(SOURCE_ETAG)
        }
        ("HEAD", "/bucket/tenant/source.txt") => not_found_response(),
        ("GET", _) if query.contains("list-type=2") => empty_list_response(),
        ("PUT", path)
            if path.starts_with("/bucket/tenant/.ycloud-system/file-move-transactions/") =>
        {
            state.move_journal_present.store(true, Ordering::SeqCst);
            state.move_journal_writes.fetch_add(1, Ordering::SeqCst);
            put_object_response()
        }
        ("HEAD", path)
            if path.starts_with("/bucket/tenant/.ycloud-system/file-move-transactions/") =>
        {
            if state.move_journal_present.load(Ordering::SeqCst) {
                object_response_with_metadata(1, r#""journal-etag""#, "application/json", None)
            } else {
                not_found_response()
            }
        }
        ("DELETE", path)
            if path.starts_with("/bucket/tenant/.ycloud-system/file-move-transactions/") =>
        {
            state.move_journal_present.store(false, Ordering::SeqCst);
            state.move_journal_deletes.fetch_add(1, Ordering::SeqCst);
            delete_object_response()
        }
        ("DELETE", "/bucket/tenant/source.txt") => {
            state.source_present.store(false, Ordering::SeqCst);
            error_response()
        }
        ("PUT", "/bucket/tenant/destination.txt")
            if headers
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("x-amz-copy-source:")) =>
        {
            state.committed.store(true, Ordering::SeqCst);
            state.copy_attempts.fetch_add(1, Ordering::SeqCst);
            error_response()
        }
        ("HEAD", "/bucket/tenant/destination.txt") => {
            state.destination_checks.fetch_add(1, Ordering::SeqCst);
            if state.committed.load(Ordering::SeqCst) {
                object_response(state.destination_etag)
            } else {
                not_found_response()
            }
        }
        _ => bad_request_response(),
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_multipart_connection(
    mut stream: TcpStream,
    marker_matches: bool,
    completed: &AtomicBool,
    content_type_preserved: &AtomicBool,
    copied_parts: &AtomicUsize,
    journal_deletes: &AtomicUsize,
    operation_id: &Mutex<Option<String>>,
    session_present: &AtomicBool,
) {
    let Some((method, raw_path, headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let journal_path = path.starts_with("/bucket/tenant/.ycloud-system/multipart-sessions/");
    let response = if method == "PUT" && journal_path {
        session_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "HEAD" && journal_path {
        if session_present.load(Ordering::SeqCst) {
            object_response_with_metadata(1, r#""journal-etag""#, "application/json", None)
        } else {
            not_found_response()
        }
    } else if method == "DELETE" && journal_path {
        session_present.store(false, Ordering::SeqCst);
        journal_deletes.fetch_add(1, Ordering::SeqCst);
        delete_object_response()
    } else if method == "HEAD" && path == "/bucket/tenant/source.bin" {
        object_response_with_metadata(
            S3_SINGLE_COPY_LIMIT + 1,
            SOURCE_ETAG,
            "application/test-binary",
            None,
        )
    } else if method == "HEAD" && path == "/bucket/tenant/destination.bin" {
        if completed.load(Ordering::SeqCst) {
            let marker = if marker_matches {
                operation_id.lock().unwrap().clone()
            } else {
                Some("different-operation".into())
            };
            object_response_with_metadata(
                S3_SINGLE_COPY_LIMIT + 1,
                MULTIPART_ETAG,
                "application/test-binary",
                marker.as_deref(),
            )
        } else {
            not_found_response()
        }
    } else if method == "POST"
        && path == "/bucket/tenant/destination.bin"
        && query
            .split('&')
            .any(|value| value == "uploads" || value.starts_with("uploads="))
    {
        let marker = header_value(&headers, &format!("x-amz-meta-{S3_OPERATION_METADATA_KEY}"));
        *operation_id.lock().unwrap() = marker.map(str::to_owned);
        content_type_preserved.store(
            header_value(&headers, "content-type") == Some("application/test-binary"),
            Ordering::SeqCst,
        );
        create_multipart_response()
    } else if method == "PUT"
        && path == "/bucket/tenant/destination.bin"
        && query.contains("partNumber=")
        && query.contains("uploadId=local-upload")
    {
        copied_parts.fetch_add(1, Ordering::SeqCst);
        copy_part_response()
    } else if method == "POST"
        && path == "/bucket/tenant/destination.bin"
        && query.contains("uploadId=local-upload")
    {
        completed.store(true, Ordering::SeqCst);
        error_response()
    } else if method == "DELETE"
        && path == "/bucket/tenant/destination.bin"
        && query.contains("uploadId=local-upload")
    {
        no_such_upload_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle_recovery_inventory_connection(
    mut stream: TcpStream,
    list_requests: &AtomicUsize,
    delete_requests: &AtomicUsize,
) {
    let Some((method, raw_path, _headers)) = read_request(&mut stream).await else {
        return;
    };
    let (_path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let response = if method == "GET" && query.contains("list-type=2") {
        list_requests.fetch_add(1, Ordering::SeqCst);
        if query.contains("uploads") {
            list_response(Some(
                "tenant/.ycloud-system/uploads/0123456789abcdef0123456789abcdef",
            ))
        } else if query.contains("backups") {
            list_response(Some(
                "tenant/.ycloud-system/backups/fedcba9876543210fedcba9876543210",
            ))
        } else {
            list_response(None)
        }
    } else if method == "DELETE" {
        delete_requests.fetch_add(1, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle_internal_upload_intent_connection(
    mut stream: TcpStream,
    object_present: &AtomicBool,
    intent_present: &AtomicBool,
    marker_seen: &AtomicBool,
    object_deletes: &AtomicUsize,
) {
    let Some((method, raw_path, headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let intent_path =
        format!("/bucket/tenant/.ycloud-system/internal-upload-intents/{INTERNAL_INTENT_ID}");
    let object_path = format!("/bucket/tenant/.ycloud-system/uploads/{INTERNAL_INTENT_ID}");
    let response = if method == "PUT" && path == intent_path {
        intent_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "PUT" && path == object_path {
        marker_seen.store(
            header_value(&headers, &format!("x-amz-meta-{S3_OPERATION_METADATA_KEY}"))
                == Some(INTERNAL_INTENT_ID),
            Ordering::SeqCst,
        );
        object_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "GET" && query.contains("list-type=2") {
        if query.contains("internal-upload-intents") && intent_present.load(Ordering::SeqCst) {
            list_response(Some(&format!(
                "tenant/.ycloud-system/internal-upload-intents/{INTERNAL_INTENT_ID}"
            )))
        } else {
            list_response(None)
        }
    } else if method == "GET" && path == intent_path {
        internal_upload_intent_response()
    } else if method == "HEAD" && path == object_path {
        if object_present.load(Ordering::SeqCst) {
            object_response_with_metadata(
                SOURCE_LENGTH as u64,
                "\"temporary-etag\"",
                "text/plain",
                Some(INTERNAL_INTENT_ID),
            )
        } else {
            not_found_response()
        }
    } else if method == "HEAD" && path == intent_path {
        if intent_present.load(Ordering::SeqCst) {
            object_response_with_metadata(1, "\"journal-etag\"", "application/json", None)
        } else {
            not_found_response()
        }
    } else if method == "DELETE" && path == object_path {
        object_deletes.fetch_add(1, Ordering::SeqCst);
        object_present.store(false, Ordering::SeqCst);
        error_response()
    } else if method == "DELETE" && path == intent_path {
        intent_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle_upload_cleanup_failure_connection(
    mut stream: TcpStream,
    temporary_delete_attempts: &AtomicUsize,
    journal_delete_attempts: &AtomicUsize,
) {
    let Some((method, raw_path, _headers)) = read_request(&mut stream).await else {
        return;
    };
    let path = raw_path
        .split_once('?')
        .map_or(raw_path.as_str(), |(path, _)| path);
    let temporary_path = format!("/bucket/tenant/.ycloud-system/uploads/{INTERNAL_INTENT_ID}");
    let journal_path = format!("/bucket/tenant/.ycloud-system/transactions/{INTERNAL_INTENT_ID}");
    let response = if method == "HEAD" && path == temporary_path {
        object_response_with_metadata(
            SOURCE_LENGTH as u64,
            "\"temporary-etag\"",
            "text/plain",
            Some(INTERNAL_INTENT_ID),
        )
    } else if method == "DELETE" && path == temporary_path {
        temporary_delete_attempts.fetch_add(1, Ordering::SeqCst);
        error_response()
    } else if method == "DELETE" && path == journal_path {
        journal_delete_attempts.fetch_add(1, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle_intent_put_response_loss_connection(
    mut stream: TcpStream,
    intent_present: &AtomicBool,
    intent_put_attempts: &AtomicUsize,
    object_put_attempts: &AtomicUsize,
    intent_delete_attempts: &AtomicUsize,
    intent_id: &Mutex<Option<String>>,
) {
    let Some((method, raw_path, _headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let intent_prefix = "/bucket/tenant/.ycloud-system/internal-upload-intents/";
    let object_prefix = "/bucket/tenant/.ycloud-system/uploads/";
    let captured_id = intent_id.lock().unwrap().clone();
    let expected_intent_path = captured_id
        .as_deref()
        .map(|id| format!("{intent_prefix}{id}"));
    let expected_object_path = captured_id
        .as_deref()
        .map(|id| format!("{object_prefix}{id}"));
    let response = if method == "PUT" && path.starts_with(intent_prefix) {
        let id = path.trim_start_matches(intent_prefix);
        if valid_protocol_transaction_id(id) {
            *intent_id.lock().unwrap() = Some(id.to_owned());
        }
        intent_put_attempts.fetch_add(1, Ordering::SeqCst);
        intent_present.store(true, Ordering::SeqCst);
        error_response()
    } else if method == "PUT" && expected_object_path.as_deref() == Some(path) {
        object_put_attempts.fetch_add(1, Ordering::SeqCst);
        put_object_response()
    } else if method == "GET" && query.contains("list-type=2") {
        if query.contains("internal-upload-intents") && intent_present.load(Ordering::SeqCst) {
            let key = intent_id
                .lock()
                .unwrap()
                .as_ref()
                .map(|id| format!("tenant/.ycloud-system/internal-upload-intents/{id}"));
            list_response(key.as_deref())
        } else {
            list_response(None)
        }
    } else if method == "GET" && expected_intent_path.as_deref() == Some(path) {
        internal_upload_intent_response_for(
            intent_id
                .lock()
                .unwrap()
                .as_deref()
                .unwrap_or(INTERNAL_INTENT_ID),
        )
    } else if method == "HEAD" && expected_object_path.as_deref() == Some(path) {
        not_found_response()
    } else if method == "HEAD" && expected_intent_path.as_deref() == Some(path) {
        if intent_present.load(Ordering::SeqCst) {
            object_response_with_metadata(1, "\"journal-etag\"", "application/json", None)
        } else {
            not_found_response()
        }
    } else if method == "DELETE" && expected_intent_path.as_deref() == Some(path) {
        intent_delete_attempts.fetch_add(1, Ordering::SeqCst);
        intent_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle_multipart_recovery_fault_connection(
    mut stream: TcpStream,
    fault: MultipartRecoveryFault,
    abort_attempts: &AtomicUsize,
    journal_deletes: &AtomicUsize,
    list_attempts: &AtomicUsize,
) {
    let Some((method, raw_path, _headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let object_path = format!("/bucket/tenant/.ycloud-system/uploads/{INTERNAL_INTENT_ID}");
    let journal_prefix = "/bucket/tenant/.ycloud-system/multipart-sessions/";
    let response = if method == "HEAD" && path == object_path {
        if matches!(fault, MultipartRecoveryFault::HeadUnavailable) {
            error_response()
        } else {
            not_found_response()
        }
    } else if method == "DELETE" && path == object_path && query.contains("uploadId=") {
        abort_attempts.fetch_add(1, Ordering::SeqCst);
        if matches!(fault, MultipartRecoveryFault::AbortUnavailable) {
            error_response()
        } else {
            delete_object_response()
        }
    } else if method == "GET" && query.contains("uploads") {
        list_attempts.fetch_add(1, Ordering::SeqCst);
        stuck_multipart_list_response(&format!(
            "tenant/.ycloud-system/uploads/{INTERNAL_INTENT_ID}"
        ))
    } else if method == "DELETE" && path.starts_with(journal_prefix) {
        journal_deletes.fetch_add(1, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_multipart_create_response_loss_connection(
    mut stream: TcpStream,
    operation_id: &Mutex<Option<String>>,
    intent_present: &AtomicBool,
    session_present: &AtomicBool,
    provider_session_present: &AtomicBool,
    create_attempts: &AtomicUsize,
    part_upload_attempts: &AtomicUsize,
    abort_attempts: &AtomicUsize,
    session_deletes: &AtomicUsize,
) {
    let Some((method, raw_path, _headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let intent_prefix = "/bucket/tenant/.ycloud-system/internal-upload-intents/";
    let session_prefix = "/bucket/tenant/.ycloud-system/multipart-sessions/";
    let captured_id = operation_id.lock().unwrap().clone();
    let object_path = captured_id
        .as_deref()
        .map(|id| format!("/bucket/tenant/.ycloud-system/uploads/{id}"));
    let intent_path = captured_id
        .as_deref()
        .map(|id| format!("{intent_prefix}{id}"));
    let session_path = captured_id
        .as_deref()
        .map(|id| format!("{session_prefix}{id}"));

    let response = if method == "PUT" && path.starts_with(intent_prefix) {
        let id = path.trim_start_matches(intent_prefix);
        if valid_protocol_transaction_id(id) {
            *operation_id.lock().unwrap() = Some(id.to_owned());
        }
        intent_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "PUT" && session_path.as_deref() == Some(path) {
        session_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "POST"
        && object_path.as_deref() == Some(path)
        && query
            .split('&')
            .any(|value| value == "uploads" || value.starts_with("uploads="))
    {
        create_attempts.fetch_add(1, Ordering::SeqCst);
        provider_session_present.store(true, Ordering::SeqCst);
        error_response()
    } else if method == "PUT"
        && object_path.as_deref() == Some(path)
        && query.contains("partNumber=")
    {
        part_upload_attempts.fetch_add(1, Ordering::SeqCst);
        put_object_response()
    } else if method == "HEAD" && object_path.as_deref() == Some(path) {
        not_found_response()
    } else if method == "HEAD" && intent_path.as_deref() == Some(path) {
        if intent_present.load(Ordering::SeqCst) {
            object_response_with_metadata(1, "\"journal-etag\"", "application/json", None)
        } else {
            not_found_response()
        }
    } else if method == "HEAD" && session_path.as_deref() == Some(path) {
        if session_present.load(Ordering::SeqCst) {
            object_response_with_metadata(1, r#""journal-etag""#, "application/json", None)
        } else {
            not_found_response()
        }
    } else if method == "DELETE" && intent_path.as_deref() == Some(path) {
        intent_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else if method == "GET" && query.contains("list-type=2") {
        if query.contains("multipart-sessions") && session_present.load(Ordering::SeqCst) {
            let key = operation_id
                .lock()
                .unwrap()
                .as_ref()
                .map(|id| format!("tenant/.ycloud-system/multipart-sessions/{id}"));
            list_response(key.as_deref())
        } else {
            list_response(None)
        }
    } else if method == "GET" && session_path.as_deref() == Some(path) {
        multipart_session_response_for(
            operation_id
                .lock()
                .unwrap()
                .as_deref()
                .unwrap_or(INTERNAL_INTENT_ID),
            S3_MULTIPART_THRESHOLD + 1,
        )
    } else if method == "GET" && query.contains("uploads") {
        let id = operation_id.lock().unwrap().clone();
        if provider_session_present.load(Ordering::SeqCst) {
            multipart_list_response(
                &format!(
                    "tenant/.ycloud-system/uploads/{}",
                    id.as_deref().unwrap_or(INTERNAL_INTENT_ID)
                ),
                "lost-create-upload",
            )
        } else {
            empty_multipart_list_response()
        }
    } else if method == "DELETE"
        && object_path.as_deref() == Some(path)
        && query.contains("uploadId=lost-create-upload")
    {
        abort_attempts.fetch_add(1, Ordering::SeqCst);
        provider_session_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else if method == "DELETE" && session_path.as_deref() == Some(path) {
        session_deletes.fetch_add(1, Ordering::SeqCst);
        session_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_activation_probe_connection(
    mut stream: TcpStream,
    operation_id: &Mutex<Option<String>>,
    journal_present: &AtomicBool,
    source_present: &AtomicBool,
    copy_present: &AtomicBool,
    marker_seen: &AtomicBool,
    copy_attempts: &AtomicUsize,
    copy_delete_attempts: &AtomicUsize,
    source_delete_attempts: &AtomicUsize,
    journal_deletes: &AtomicUsize,
    fail_conditional_create: bool,
) {
    let Some((method, raw_path, headers)) = read_request(&mut stream).await else {
        return;
    };
    let (path, query) = raw_path
        .split_once('?')
        .map_or((raw_path.as_str(), ""), |(path, query)| (path, query));
    let journal_prefix = "/bucket/tenant/.ycloud-system/activation-probe-intents/";
    let captured_id = operation_id.lock().unwrap().clone();
    let source_path = captured_id
        .as_deref()
        .map(|id| format!("/bucket/tenant/.ycloud-system/activation-tests/{id}"));
    let copy_path = captured_id
        .as_deref()
        .map(|id| format!("/bucket/tenant/.ycloud-system/activation-tests/{id}-copy"));
    let journal_path = captured_id
        .as_deref()
        .map(|id| format!("{journal_prefix}{id}"));
    let expected_size = captured_id
        .as_deref()
        .map(|id| format!("ycloud-storage-activation:{id}").len() as u64)
        .unwrap_or(0);

    let response = if method == "GET" && query.contains("list-type=2") {
        if query.contains("activation-probe-intents") && journal_present.load(Ordering::SeqCst) {
            let key = operation_id
                .lock()
                .unwrap()
                .as_ref()
                .map(|id| format!("tenant/.ycloud-system/activation-probe-intents/{id}"));
            list_response(key.as_deref())
        } else {
            list_response(None)
        }
    } else if method == "PUT" && path.starts_with(journal_prefix) {
        let id = path.trim_start_matches(journal_prefix);
        if valid_protocol_transaction_id(id) {
            *operation_id.lock().unwrap() = Some(id.to_owned());
        }
        journal_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "GET" && journal_path.as_deref() == Some(path) {
        activation_probe_intent_response_for(
            operation_id
                .lock()
                .unwrap()
                .as_deref()
                .unwrap_or(INTERNAL_INTENT_ID),
        )
    } else if method == "PUT" && source_path.as_deref() == Some(path) {
        if fail_conditional_create {
            let _ = stream.write_all(error_response().as_bytes()).await;
            let _ = stream.shutdown().await;
            return;
        }
        marker_seen.store(
            header_value(&headers, &format!("x-amz-meta-{S3_OPERATION_METADATA_KEY}"))
                == captured_id.as_deref(),
            Ordering::SeqCst,
        );
        source_present.store(true, Ordering::SeqCst);
        put_object_response()
    } else if method == "PUT"
        && copy_path.as_deref() == Some(path)
        && headers
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("x-amz-copy-source:"))
    {
        copy_attempts.fetch_add(1, Ordering::SeqCst);
        copy_present.store(true, Ordering::SeqCst);
        error_response()
    } else if method == "HEAD" && source_path.as_deref() == Some(path) {
        if source_present.load(Ordering::SeqCst) {
            object_response_with_metadata(
                expected_size,
                "\"journal-etag\"",
                "application/octet-stream",
                captured_id.as_deref(),
            )
        } else {
            not_found_response()
        }
    } else if method == "HEAD" && copy_path.as_deref() == Some(path) {
        if copy_present.load(Ordering::SeqCst) {
            object_response_with_metadata(
                expected_size,
                "\"journal-etag\"",
                "application/octet-stream",
                captured_id.as_deref(),
            )
        } else {
            not_found_response()
        }
    } else if method == "HEAD" && journal_path.as_deref() == Some(path) {
        if journal_present.load(Ordering::SeqCst) {
            object_response_with_metadata(1, "\"journal-etag\"", "application/json", None)
        } else {
            not_found_response()
        }
    } else if method == "GET" && source_path.as_deref() == Some(path) {
        activation_payload_response(
            operation_id
                .lock()
                .unwrap()
                .as_deref()
                .unwrap_or(INTERNAL_INTENT_ID),
        )
    } else if method == "DELETE" && copy_path.as_deref() == Some(path) {
        copy_delete_attempts.fetch_add(1, Ordering::SeqCst);
        copy_present.store(false, Ordering::SeqCst);
        error_response()
    } else if method == "DELETE" && source_path.as_deref() == Some(path) {
        source_delete_attempts.fetch_add(1, Ordering::SeqCst);
        source_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else if method == "DELETE" && journal_path.as_deref() == Some(path) {
        journal_deletes.fetch_add(1, Ordering::SeqCst);
        journal_present.store(false, Ordering::SeqCst);
        delete_object_response()
    } else {
        bad_request_response()
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn valid_protocol_transaction_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn header_value<'a>(headers: &'a str, expected: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(expected).then_some(value.trim())
    })
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut bytes = Vec::with_capacity(4_096);
    let mut chunk = [0_u8; 1_024];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if bytes.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec()).ok()?;
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let expected = header_end.checked_add(4)?.checked_add(content_length)?;
    while bytes.len() < expected {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > 128 * 1024 {
            return None;
        }
    }
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    Some((method, path, lines.collect::<Vec<_>>().join("\n")))
}

fn object_response(etag: &str) -> String {
    object_response_with_metadata(SOURCE_LENGTH as u64, etag, "text/plain", None)
}

fn object_response_with_metadata(
    length: u64,
    etag: &str,
    content_type: &str,
    operation_id: Option<&str>,
) -> String {
    let metadata = operation_id.map_or_else(String::new, |value| {
        format!("x-amz-meta-{S3_OPERATION_METADATA_KEY}: {value}\r\n")
    });
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nETag: {etag}\r\nContent-Type: {content_type}\r\n{metadata}Connection: close\r\nx-amz-request-id: local-test\r\n\r\n"
    )
}

fn put_object_response() -> String {
    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"journal-etag\"\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n".into()
}

fn delete_object_response() -> String {
    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n".into()
}

fn empty_list_response() -> String {
    list_response(None)
}

fn list_response(key: Option<&str>) -> String {
    let contents = key.map_or_else(String::new, |key| {
        format!(
            "<Contents><Key>{key}</Key><ETag>&quot;inventory-etag&quot;</ETag><Size>7</Size><StorageClass>STANDARD</StorageClass></Contents>"
        )
    });
    let key_count = usize::from(key.is_some());
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>bucket</Name><Prefix></Prefix>{contents}<KeyCount>{key_count}</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated></ListBucketResult>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn internal_upload_intent_response() -> String {
    internal_upload_intent_response_for(INTERNAL_INTENT_ID)
}

fn internal_upload_intent_response_for(id: &str) -> String {
    authenticated_json_response(
        S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE,
        serde_json::json!({
            "schema_version": 1,
            "id": id,
            "key": format!("tenant/.ycloud-system/uploads/{id}"),
            "expected_size": SOURCE_LENGTH,
            "created_unix": 1
        }),
    )
}

fn authenticated_json_response(purpose: &str, payload: serde_json::Value) -> String {
    let body = authenticated_journal::encode(&[0x31; 32], purpose, &payload).unwrap();
    let body = String::from_utf8(body).unwrap();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nETag: \"journal-etag\"\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn activation_probe_intent_response_for(id: &str) -> String {
    let expected_size = format!("ycloud-storage-activation:{id}").len();
    authenticated_json_response(
        S3_ACTIVATION_PROBE_JOURNAL_PURPOSE,
        serde_json::json!({
            "schema_version": 1,
            "id": id,
            "source_key": format!("tenant/.ycloud-system/activation-tests/{id}"),
            "copy_key": format!("tenant/.ycloud-system/activation-tests/{id}-copy"),
            "expected_size": expected_size,
            "created_unix": 1
        }),
    )
}

fn activation_payload_response(id: &str) -> String {
    let body = format!("ycloud-storage-activation:{id}");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nETag: \"journal-etag\"\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn create_multipart_response() -> String {
    let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>bucket</Bucket><Key>tenant/destination.bin</Key><UploadId>local-upload</UploadId></InitiateMultipartUploadResult>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn stuck_multipart_list_response(key: &str) -> String {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>bucket</Bucket><KeyMarker>stuck-key</KeyMarker><UploadIdMarker>stuck-upload</UploadIdMarker><NextKeyMarker>stuck-key</NextKeyMarker><NextUploadIdMarker>stuck-upload</NextUploadIdMarker><MaxUploads>1000</MaxUploads><IsTruncated>true</IsTruncated><Upload><Key>{key}</Key><UploadId>lost-upload</UploadId><Initiated>2026-09-14T00:00:00Z</Initiated></Upload></ListMultipartUploadsResult>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn multipart_list_response(key: &str, upload_id: &str) -> String {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>bucket</Bucket><MaxUploads>1000</MaxUploads><IsTruncated>false</IsTruncated><Upload><Key>{key}</Key><UploadId>{upload_id}</UploadId><Initiated>2026-09-14T00:00:00Z</Initiated></Upload></ListMultipartUploadsResult>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn empty_multipart_list_response() -> String {
    let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>bucket</Bucket><MaxUploads>1000</MaxUploads><IsTruncated>false</IsTruncated></ListMultipartUploadsResult>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn multipart_session_response_for(id: &str, expected_size: u64) -> String {
    authenticated_json_response(
        S3_MULTIPART_SESSION_JOURNAL_PURPOSE,
        serde_json::json!({
            "schema_version": 2,
            "id": id,
            "key": format!("tenant/.ycloud-system/uploads/{id}"),
            "upload_id": null,
            "purpose": "upload",
            "expected_size": expected_size
        }),
    )
}

fn copy_part_response() -> String {
    let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CopyPartResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><ETag>\"part-etag\"</ETag><LastModified>2026-09-14T00:00:00Z</LastModified></CopyPartResult>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn error_response() -> String {
    let body = "<Error><Code>InternalError</Code><Message>response lost</Message></Error>";
    format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn not_found_response() -> String {
    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n".into()
}

fn no_such_upload_response() -> String {
    let body =
        "<Error><Code>NoSuchUpload</Code><Message>multipart session is gone</Message></Error>";
    format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n{body}",
        body.len()
    )
}

fn bad_request_response() -> String {
    "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\nx-amz-request-id: local-test\r\n\r\n".into()
}

fn test_backend(endpoint: &str) -> S3Backend {
    let settings = S3StorageConfig {
        provider: S3Provider::S3Compatible,
        endpoint: endpoint.into(),
        bucket: "bucket".into(),
        region: "us-east-1".into(),
        prefix: "tenant/".into(),
        addressing_style: S3AddressingStyle::Path,
        access_key_id: "local-test-access-key".into(),
        secret_access_key: "local-test-secret-key".into(),
        capacity_limit_bytes: Some(1024 * 1024),
    };
    let runtime = Config {
        bind_address: IpAddr::from([127, 0, 0, 1]),
        port: 0,
        storage_path: PathBuf::from("unused-protocol-test-storage"),
        local_mounts: crate::storage_catalog::LocalMountCatalog::new(
            PathBuf::from("unused-protocol-test-storage"),
            Vec::new(),
        )
        .unwrap(),
        config_path: PathBuf::from("unused-protocol-test-config.json"),
        max_upload_bytes: 1024 * 1024,
        max_upload_batch_bytes: 1024 * 1024,
        max_upload_batch_entries: 10,
        max_archive_bytes: 1024 * 1024,
        max_archive_entries: 10,
        io_concurrency: 2,
        max_list_entries: 100,
        request_timeout_secs: 5,
        upload_timeout_secs: 5,
        disk_reserve_bytes: 0,
        secure_cookies: false,
        allow_lan_http: true,
        public_base_url: None,
        public_host: None,
        trusted_proxy_ips: HashSet::new(),
        allowed_hosts: HashSet::new(),
        s3_allowed_endpoints: HashSet::from([normalize_s3_endpoint(endpoint).unwrap()]),
        transaction_auth_key: [0x31; 32],
    };
    S3Backend::new(&settings, &runtime).unwrap()
}

#[tokio::test]
async fn copy_response_loss_is_reconciled_from_the_committed_object() {
    let simulator = CopyResponseLossSimulator::start(SOURCE_ETAG).await;
    let backend = test_backend(&simulator.endpoint);

    let etag = backend
        .copy_key(
            "tenant/source.txt",
            "tenant/destination.txt",
            Some(SOURCE_ETAG),
            true,
        )
        .await
        .unwrap();

    assert_eq!(etag, SOURCE_ETAG);
    assert!(simulator.committed.load(Ordering::SeqCst));
    assert!(simulator.copy_attempts.load(Ordering::SeqCst) >= 1);
    assert!(simulator.destination_checks.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.destination_etag, SOURCE_ETAG);
}

#[tokio::test]
async fn copy_response_loss_with_mismatched_destination_stays_unknown() {
    let simulator = CopyResponseLossSimulator::start("\"different-etag\"").await;
    let backend = test_backend(&simulator.endpoint);

    let error = backend
        .copy_key(
            "tenant/source.txt",
            "tenant/destination.txt",
            Some(SOURCE_ETAG),
            true,
        )
        .await
        .unwrap_err();

    assert_eq!(
        error.operation().map(|outcome| outcome.commit),
        Some(CommitState::Unknown)
    );
    assert!(simulator.committed.load(Ordering::SeqCst));
    assert!(simulator.destination_checks.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn file_move_survives_lost_copy_and_source_delete_responses() {
    let simulator = CopyResponseLossSimulator::start(SOURCE_ETAG).await;
    let backend = test_backend(&simulator.endpoint);

    backend
        .move_file("source.txt", "destination.txt")
        .await
        .unwrap();

    assert!(simulator.committed.load(Ordering::SeqCst));
    assert!(!simulator.source_present.load(Ordering::SeqCst));
    assert!(!simulator.move_journal_present.load(Ordering::SeqCst));
    assert_eq!(simulator.move_journal_writes.load(Ordering::SeqCst), 2);
    assert_eq!(simulator.move_journal_deletes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn multipart_complete_response_loss_uses_marker_and_preserves_content_type() {
    let simulator = MultipartResponseLossSimulator::start(true).await;
    let backend = test_backend(&simulator.endpoint);
    let operation_id = "0123456789abcdef0123456789abcdef";

    let etag = backend
        .copy_key_with_operation(
            "tenant/source.bin",
            "tenant/destination.bin",
            Some(SOURCE_ETAG),
            true,
            Some(operation_id),
        )
        .await
        .unwrap();

    assert_eq!(etag, MULTIPART_ETAG);
    assert!(simulator.completed.load(Ordering::SeqCst));
    assert!(simulator.content_type_preserved.load(Ordering::SeqCst));
    assert_eq!(simulator.copied_parts.load(Ordering::SeqCst), 65);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 1);
    assert_eq!(
        simulator.operation_id.lock().unwrap().as_deref(),
        Some(operation_id)
    );
}

#[tokio::test]
async fn missing_multipart_with_mismatched_marker_retains_recovery_evidence() {
    let simulator = MultipartResponseLossSimulator::start(false).await;
    let backend = test_backend(&simulator.endpoint);

    let error = backend
        .copy_key(
            "tenant/source.bin",
            "tenant/destination.bin",
            Some(SOURCE_ETAG),
            true,
        )
        .await
        .unwrap_err();

    assert_eq!(
        error.operation().map(|outcome| outcome.commit),
        Some(CommitState::Unknown)
    );
    assert!(simulator.completed.load(Ordering::SeqCst));
    assert_eq!(simulator.copied_parts.load(Ordering::SeqCst), 65);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recovery_inventory_reports_internal_objects_without_deleting_them() {
    let simulator = RecoveryInventorySimulator::start().await;
    let backend = test_backend(&simulator.endpoint);

    let recovered = backend.recover_transactions().await.unwrap();

    assert_eq!(recovered, 0);
    assert_eq!(
        backend.recovery_status(),
        super::S3RecoveryStatus {
            orphan_uploads: 1,
            orphan_backups: 1,
            ..super::S3RecoveryStatus::default()
        }
    );
    assert_eq!(simulator.list_requests.load(Ordering::SeqCst), 8);
    assert_eq!(simulator.delete_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn startup_recovery_settles_a_pre_transaction_internal_upload_intent() {
    let simulator = InternalUploadIntentRecoverySimulator::start().await;
    let backend = test_backend(&simulator.endpoint);
    let object_key = format!("tenant/.ycloud-system/uploads/{INTERNAL_INTENT_ID}");

    let (_journal_key, _intent, _journal_etag) = backend
        .create_internal_upload_intent(INTERNAL_INTENT_ID, SOURCE_LENGTH as u64)
        .await
        .unwrap();
    backend
        .single_upload(
            &object_key,
            Body::from("payload"),
            SOURCE_LENGTH as u64,
            Some("text/plain"),
            INTERNAL_INTENT_ID,
        )
        .await
        .unwrap();

    let recovered = backend.recover_transactions().await.unwrap();

    assert_eq!(recovered, 1);
    assert!(simulator.marker_seen.load(Ordering::SeqCst));
    assert!(simulator.object_deletes.load(Ordering::SeqCst) >= 1);
    assert!(!simulator.object_present.load(Ordering::SeqCst));
    assert!(!simulator.intent_present.load(Ordering::SeqCst));
    assert_eq!(
        backend.recovery_status(),
        super::S3RecoveryStatus::default()
    );
}

#[tokio::test]
async fn upload_transaction_journal_is_retained_until_internal_cleanup_is_confirmed() {
    let simulator = UploadCleanupFailureSimulator::start().await;
    let backend = test_backend(&simulator.endpoint);
    let transaction = S3UploadTransaction {
        schema_version: 1,
        id: INTERNAL_INTENT_ID.into(),
        relative: "destination.txt".into(),
        stage: S3UploadStage::DestinationCommitted,
        temporary: S3ObjectSnapshot {
            size: SOURCE_LENGTH as u64,
            etag: Some("\"temporary-etag\"".into()),
        },
        previous: None,
    };
    let journal_key = format!("tenant/.ycloud-system/transactions/{INTERNAL_INTENT_ID}");

    let error = backend
        .finish_upload_transaction(&journal_key, Some("\"journal-etag\""), &transaction)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("删除失败"));
    assert!(simulator.temporary_delete_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.journal_delete_attempts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lost_internal_intent_put_response_never_starts_an_unowned_upload() {
    let simulator = IntentPutResponseLossSimulator::start().await;
    let backend = test_backend(&simulator.endpoint);

    let error = backend
        .upload_file(
            "destination.txt",
            Body::from("payload"),
            SOURCE_LENGTH as u64,
            1024,
            Some("text/plain"),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("事务状态"));
    assert!(simulator.intent_put_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.object_put_attempts.load(Ordering::SeqCst), 0);
    assert!(simulator.intent_present.load(Ordering::SeqCst));
    assert!(simulator.intent_id.lock().unwrap().is_some());

    let recovered = backend.recover_transactions().await.unwrap();
    assert_eq!(recovered, 1);
    assert_eq!(simulator.object_put_attempts.load(Ordering::SeqCst), 0);
    assert!(simulator.intent_delete_attempts.load(Ordering::SeqCst) >= 1);
    assert!(!simulator.intent_present.load(Ordering::SeqCst));
}

fn multipart_recovery_session(upload_id: Option<&str>) -> S3MultipartSession {
    S3MultipartSession {
        schema_version: S3_MULTIPART_SESSION_SCHEMA_VERSION,
        id: INTERNAL_INTENT_ID.into(),
        key: format!("tenant/.ycloud-system/uploads/{INTERNAL_INTENT_ID}"),
        upload_id: upload_id.map(str::to_owned),
        purpose: Some(S3MultipartPurpose::Upload),
        expected_size: Some(S3_MULTIPART_THRESHOLD),
    }
}

#[tokio::test]
async fn temporary_head_failure_preserves_multipart_recovery_record() {
    let simulator =
        MultipartRecoveryFaultSimulator::start(MultipartRecoveryFault::HeadUnavailable).await;
    let backend = test_backend(&simulator.endpoint);
    let journal_key = format!("tenant/.ycloud-system/multipart-sessions/{INTERNAL_INTENT_ID}");

    backend
        .recover_multipart_session(
            &journal_key,
            "\"journal-etag\"",
            &multipart_recovery_session(Some("known-upload")),
        )
        .await
        .unwrap_err();

    assert_eq!(simulator.abort_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn abort_transport_failure_preserves_multipart_recovery_record() {
    let simulator =
        MultipartRecoveryFaultSimulator::start(MultipartRecoveryFault::AbortUnavailable).await;
    let backend = test_backend(&simulator.endpoint);
    let journal_key = format!("tenant/.ycloud-system/multipart-sessions/{INTERNAL_INTENT_ID}");

    backend
        .recover_multipart_session(
            &journal_key,
            "\"journal-etag\"",
            &multipart_recovery_session(Some("known-upload")),
        )
        .await
        .unwrap_err();

    assert!(simulator.abort_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_progressing_multipart_pagination_preserves_intent_without_abort() {
    let simulator =
        MultipartRecoveryFaultSimulator::start(MultipartRecoveryFault::PaginationStuck).await;
    let backend = test_backend(&simulator.endpoint);
    let journal_key = format!("tenant/.ycloud-system/multipart-sessions/{INTERNAL_INTENT_ID}");

    let error = backend
        .recover_multipart_session(
            &journal_key,
            "\"journal-etag\"",
            &multipart_recovery_session(None),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("分页未前进"));
    assert!(simulator.list_attempts.load(Ordering::SeqCst) >= 2);
    assert_eq!(simulator.abort_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lost_create_multipart_response_is_recovered_from_precreate_intent() {
    let simulator = MultipartCreateResponseLossSimulator::start().await;
    let backend = test_backend(&simulator.endpoint);
    let upload_size = S3_MULTIPART_THRESHOLD + 1;

    let error = backend
        .upload_file(
            "large.bin",
            Body::empty(),
            upload_size,
            upload_size,
            Some("application/octet-stream"),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("上传失败"));
    assert!(simulator.operation_id.lock().unwrap().is_some());
    assert!(simulator.create_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.part_upload_attempts.load(Ordering::SeqCst), 0);
    assert!(!simulator.intent_present.load(Ordering::SeqCst));
    assert!(simulator.session_present.load(Ordering::SeqCst));
    assert!(simulator.provider_session_present.load(Ordering::SeqCst));

    let recovered = backend.recover_transactions().await.unwrap();
    assert_eq!(recovered, 1);
    assert!(simulator.abort_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.session_deletes.load(Ordering::SeqCst), 1);
    assert!(!simulator.session_present.load(Ordering::SeqCst));
    assert!(!simulator.provider_session_present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn activation_probe_owns_and_confirms_cleanup_after_lost_responses() {
    let simulator = ActivationProbeSimulator::start(false).await;
    let backend = test_backend(&simulator.endpoint);

    backend.activation_probe().await.unwrap();

    assert!(simulator.operation_id.lock().unwrap().is_some());
    assert!(simulator.marker_seen.load(Ordering::SeqCst));
    assert!(simulator.copy_attempts.load(Ordering::SeqCst) >= 1);
    assert!(simulator.copy_delete_attempts.load(Ordering::SeqCst) >= 1);
    assert!(simulator.source_delete_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 1);
    assert!(!simulator.copy_present.load(Ordering::SeqCst));
    assert!(!simulator.source_present.load(Ordering::SeqCst));
    assert!(!simulator.journal_present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn activation_probe_reports_the_exact_missing_capability_and_cleans_its_journal() {
    let simulator = ActivationProbeSimulator::start_with_conditional_create_failure().await;
    let backend = test_backend(&simulator.endpoint);

    let error = backend.activation_probe().await.unwrap_err();

    assert_eq!(error.code(), "storage_capability_unavailable");
    assert_eq!(error.capability(), Some("conditional_create"));
    assert!(!simulator.source_present.load(Ordering::SeqCst));
    assert!(!simulator.journal_present.load(Ordering::SeqCst));
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn startup_recovery_settles_owned_activation_probe_objects() {
    let simulator = ActivationProbeSimulator::start(true).await;
    let backend = test_backend(&simulator.endpoint);

    let recovered = backend.recover_transactions().await.unwrap();

    assert_eq!(recovered, 1);
    assert!(simulator.copy_delete_attempts.load(Ordering::SeqCst) >= 1);
    assert!(simulator.source_delete_attempts.load(Ordering::SeqCst) >= 1);
    assert_eq!(simulator.journal_deletes.load(Ordering::SeqCst), 1);
    assert!(!simulator.copy_present.load(Ordering::SeqCst));
    assert!(!simulator.source_present.load(Ordering::SeqCst));
    assert!(!simulator.journal_present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn runtime_worker_settles_a_tracked_activation_probe_intent() {
    let simulator = ActivationProbeSimulator::start(true).await;
    let backend = test_backend(&simulator.endpoint);
    let journal_key =
        format!("tenant/.ycloud-system/activation-probe-intents/{INTERNAL_INTENT_ID}");
    backend.recovery_runtime.journal_write_started(&journal_key);
    crate::storage_backend::spawn_s3_recovery_reconciler(backend.clone());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = backend.recovery_status();
            if status.pending_records == 0
                && !status.recovering
                && !simulator.journal_present.load(Ordering::SeqCst)
                && !simulator.source_present.load(Ordering::SeqCst)
                && !simulator.copy_present.load(Ordering::SeqCst)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("runtime recovery worker did not settle its tracked intent");

    let status = backend.recovery_status();
    assert_eq!(status.pending_records, 0);
    assert_eq!(status.consecutive_failures, 0);
    assert!(!status.recovering);
    backend.stop_recovery_worker();
}

#[tokio::test]
async fn runtime_worker_never_adopts_a_journal_owned_by_an_active_upload() {
    let simulator = RecoveryInventorySimulator::start().await;
    let backend = test_backend(&simulator.endpoint);
    let journal_key = "tenant/.ycloud-system/transactions/foreground";
    let upload_owner = backend.recovery_gate.read().await;
    backend.recovery_runtime.journal_write_started(journal_key);
    crate::storage_backend::spawn_s3_recovery_reconciler(backend.clone());

    tokio::time::timeout(Duration::from_secs(3), async {
        while !backend.recovery_status().recovering {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("runtime worker did not wait for the active upload owner");
    backend.recovery_runtime.journal_settled(journal_key);
    drop(upload_owner);
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.recovery_status().recovering {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("runtime worker did not leave its lock wait");

    assert_eq!(simulator.list_requests.load(Ordering::SeqCst), 0);
    backend.stop_recovery_worker();
}
