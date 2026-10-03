//! Batch already-ready payloads without waiting for more network data.
use std::collections::VecDeque;

use axum::body::{Body, BodyDataStream};
use bytes::Bytes;
use futures_util::{stream::Fuse, FutureExt, StreamExt};

// Stop collecting at this target or the frame bound. An upstream frame can be
// larger; charge it whole so cancellation cannot discard an uncharged tail.
const BATCH_TARGET_BYTES: usize = 256 * 1024;
const BATCH_FRAMES: usize = 64;

pub(super) struct ReadyBatch {
    input: Fuse<BodyDataStream>,
    pending_error: Option<axum::Error>,
    ready: VecDeque<Bytes>,
}

impl ReadyBatch {
    pub(super) fn new(body: Body) -> Self {
        Self {
            // A ready EOF may be observed while the batch still has output.
            // Never poll a non-fused upstream stream again after that EOF.
            input: body.into_data_stream().fuse(),
            pending_error: None,
            ready: VecDeque::new(),
        }
    }

    pub(super) fn pop(&mut self) -> Option<Bytes> {
        self.ready.pop_front()
    }

    /// Collect at most one bounded batch. Only the first nonempty frame may
    /// wait; later polls are nonblocking. Keep a late source error behind the
    /// successfully received bytes so those bytes are durably charged first.
    pub(super) async fn collect(&mut self) -> Result<usize, std::io::Error> {
        debug_assert!(self.ready.is_empty());
        let mut chunk = loop {
            if let Some(error) = self.pending_error.take() {
                return Err(std::io::Error::other(error));
            }
            let next = self.input.next().await;
            match next {
                Some(Ok(chunk)) if chunk.is_empty() => continue,
                Some(Ok(chunk)) => break chunk,
                Some(Err(error)) => return Err(std::io::Error::other(error)),
                None => return Ok(0),
            }
        };
        let mut bytes = 0;
        for frame in 0..BATCH_FRAMES {
            if !chunk.is_empty() {
                bytes += chunk.len();
                self.ready.push_back(chunk);
            }
            if bytes >= BATCH_TARGET_BYTES || frame + 1 == BATCH_FRAMES {
                break;
            }
            // Do not add a timer, speculative reader task, or fill wait.
            match self.input.next().now_or_never() {
                Some(Some(Ok(next))) => chunk = next,
                Some(Some(Err(error))) => {
                    self.pending_error = Some(error);
                    break;
                }
                Some(None) | None => break,
            }
        }
        Ok(bytes)
    }
}
