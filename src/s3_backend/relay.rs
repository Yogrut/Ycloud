//! Split one incoming stream without buffering complete S3 parts.
use axum::body::{Body, BodyDataStream};
use bytes::Bytes;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(super) struct RelaySource {
    stream: BodyDataStream,
    pending: Bytes,
}

impl RelaySource {
    pub fn new(body: Body) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            stream: body.into_data_stream(),
            pending: Bytes::new(),
        }))
    }

    pub fn part(source: Arc<Mutex<Self>>, length: u64) -> Body {
        Body::from_stream(futures_util::stream::try_unfold(
            (source, length),
            |(source, left)| async move {
                if left == 0 {
                    return Ok::<_, std::io::Error>(None);
                }
                let mut input = source.lock().await;
                while input.pending.is_empty() {
                    input.pending = input
                        .stream
                        .next()
                        .await
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "上传请求体长度不足",
                            )
                        })?
                        .map_err(std::io::Error::other)?;
                }
                // Keep only an incoming frame, not an entire multipart chunk.
                let take = input.pending.len().min(left.min(256 * 1024) as usize);
                let bytes = input.pending.split_to(take);
                drop(input);
                Ok(Some((bytes, (source, left - take as u64))))
            },
        ))
    }

    pub async fn finish(&mut self) -> crate::error::AppResult<()> {
        if !self.pending.is_empty() {
            return Err(crate::error::AppError::PayloadTooLarge);
        }
        while let Some(frame) = self.stream.next().await {
            let frame = frame.map_err(|_| crate::error::AppError::ClientClosedRequest)?;
            if !frame.is_empty() {
                return Err(crate::error::AppError::PayloadTooLarge);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    #[tokio::test]
    async fn parts_preserve_frame_boundaries_without_reading_ahead() {
        let source = RelaySource::new(Body::from("abcdefghij"));
        assert_eq!(
            RelaySource::part(source.clone(), 4)
                .collect()
                .await
                .unwrap()
                .to_bytes(),
            "abcd"
        );
        assert_eq!(
            RelaySource::part(source.clone(), 4)
                .collect()
                .await
                .unwrap()
                .to_bytes(),
            "efgh"
        );
        assert_eq!(
            RelaySource::part(source.clone(), 2)
                .collect()
                .await
                .unwrap()
                .to_bytes(),
            "ij"
        );
        source.lock().await.finish().await.unwrap();
    }
    #[tokio::test]
    async fn incomplete_and_excess_bodies_are_rejected() {
        let short = RelaySource::new(Body::from("abc"));
        assert!(RelaySource::part(short, 4).collect().await.is_err());
        let extra = RelaySource::new(Body::from("abcde"));
        RelaySource::part(extra.clone(), 4).collect().await.unwrap();
        assert!(extra.lock().await.finish().await.is_err());
    }
}
