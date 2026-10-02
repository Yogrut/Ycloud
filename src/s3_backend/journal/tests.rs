use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use aws_sdk_s3::operation::get_object::GetObjectOutput;
use axum::body::Body;
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use http_body::Frame;
use http_body_util::StreamBody;

use super::*;

fn byte_stream(
    stream: impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + Sync + 'static,
) -> ByteStream {
    ByteStream::from_body_1_x(StreamBody::new(stream.map(|chunk| chunk.map(Frame::data))))
}

fn output(body: ByteStream, length: Option<i64>, etag: Option<&str>) -> GetObjectOutput {
    GetObjectOutput::builder()
        .body(body)
        .set_content_length(length)
        .set_e_tag(etag.map(str::to_owned))
        .build()
}

#[tokio::test]
async fn journal_payload_preserves_chunked_records_at_the_limit() {
    for chunks in [vec![b"abcdef".as_slice()], vec![b"ab", b"", b"cde", b"f"]] {
        for length in [None, Some(6)] {
            let body = byte_stream(stream::iter(
                chunks
                    .iter()
                    .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
                    .collect::<Vec<_>>(),
            ));
            let (data, etag) = read_journal_payload(output(body, length, Some("record-etag")), 6)
                .await
                .unwrap();
            assert_eq!(data, b"abcdef");
            assert_eq!(etag, "record-etag");
        }
    }
    for length in [None, Some(0)] {
        let (data, etag) = read_journal_payload(
            output(ByteStream::from_static(b""), length, Some("empty-etag")),
            0,
        )
        .await
        .unwrap();
        assert!(data.is_empty());
        assert_eq!(etag, "empty-etag");
    }
}

#[tokio::test]
async fn journal_payload_requires_identity_before_polling_the_body() {
    let polls = Arc::new(AtomicUsize::new(0));
    let observed = polls.clone();
    let body = byte_stream(stream::once(async move {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok::<_, std::io::Error>(Bytes::from_static(b"record"))
    }));
    let error = read_journal_payload(output(body, None, None), 6)
        .await
        .unwrap_err();
    assert!(matches!(error, AppError::ServiceUnavailable(_)));
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn journal_payload_does_not_decode_partial_stream_results() {
    let body = byte_stream(stream::iter([
        Ok(Bytes::from_static(b"part")),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "journal read interrupted",
        )),
    ]));
    let error = read_journal_payload(output(body, None, Some("record-etag")), 16)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AppError::Internal {
            source: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn cancelling_a_pending_journal_read_drops_its_stream() {
    let ownership = Arc::new(());
    let in_stream = ownership.clone();
    let body = byte_stream(stream::once(async move {
        let result = futures_util::future::pending::<Result<Bytes, std::io::Error>>().await;
        drop(in_stream);
        result
    }));
    let mut read = Box::pin(read_journal_payload(
        output(body, None, Some("record-etag")),
        16,
    ));
    assert!(futures_util::poll!(read.as_mut()).is_pending());
    assert_eq!(Arc::strong_count(&ownership), 2);
    drop(read);
    assert_eq!(Arc::strong_count(&ownership), 1);
}

#[tokio::test]
async fn authenticated_journal_read_preserves_chunked_format_and_maintenance_pause() {
    use axum::{
        http::{Method, Response},
        routing::any,
        Router,
    };

    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let value = serde_json::json!({"stage": "prepared"});
    let purpose = crate::s3_backend::S3_FILE_MOVE_JOURNAL_PURPOSE;
    let encoded = authenticated_journal::encode(&[0x31; 32], purpose, &value).unwrap();
    let router = Router::new().route(
        "/{*key}",
        any(move |method: Method| {
            let encoded = encoded.clone();
            let requests = observed.clone();
            async move {
                assert_eq!(method, Method::GET);
                requests.fetch_add(1, Ordering::SeqCst);
                let chunks = encoded
                    .chunks(13)
                    .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
                    .collect::<Vec<_>>();
                Response::builder()
                    .header("etag", "record-etag")
                    .body(Body::from_stream(stream::iter(chunks)))
                    .unwrap()
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
    let key = internal_key(
        "tenant/",
        "file-move-transactions",
        "0123456789abcdef0123456789abcdef",
    );
    let (restored, etag): (serde_json::Value, _) = backend
        .read_authenticated_json_journal(&key, purpose)
        .await
        .unwrap();
    assert_eq!(restored, value);
    assert_eq!(etag, "record-etag");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    backend.pause_maintenance();
    assert!(backend
        .read_authenticated_json_journal::<serde_json::Value>(&key, purpose)
        .await
        .is_err());
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
}
