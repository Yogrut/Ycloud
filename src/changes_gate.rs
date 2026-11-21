//! Per-credential cap for live change streams.
//!
//! The global semaphore stops one client from exhausting every connection; this
//! table stops a single credential from taking every permit. Tokens are hashed
//! before use so the raw credential never lives in a long-lived structure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ring::digest;

/// Concurrent streams one browser credential may hold.
const MAX_STREAMS_PER_TOKEN: usize = 6;

#[derive(Default)]
pub(crate) struct TokenStreams {
    counts: Mutex<HashMap<[u8; 16], usize>>,
}

impl TokenStreams {
    pub(crate) fn try_enter(self: &Arc<Self>, token: &str) -> Option<TokenStreamPermit> {
        let key = fingerprint(token);
        let mut counts = match self.counts.lock() {
            Ok(counts) => counts,
            Err(poisoned) => poisoned.into_inner(),
        };
        let slot = counts.entry(key).or_insert(0);
        if *slot >= MAX_STREAMS_PER_TOKEN {
            return None;
        }
        *slot += 1;
        Some(TokenStreamPermit {
            streams: Arc::clone(self),
            key,
        })
    }
}

/// Releases its slot when the stream ends, including when the stream is dropped
/// without being polled to completion.
pub(crate) struct TokenStreamPermit {
    streams: Arc<TokenStreams>,
    key: [u8; 16],
}

impl Drop for TokenStreamPermit {
    fn drop(&mut self) {
        let mut counts = match self.streams.counts.lock() {
            Ok(counts) => counts,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(slot) = counts.get_mut(&self.key) {
            *slot = slot.saturating_sub(1);
            if *slot == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

fn fingerprint(token: &str) -> [u8; 16] {
    let digest = digest::digest(&digest::SHA256, token.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest.as_ref()[..16]);
    key
}

#[cfg(test)]
mod tests {
    use super::{TokenStreams, MAX_STREAMS_PER_TOKEN};
    use std::sync::Arc;

    #[test]
    fn caps_one_credential_and_frees_slots_on_drop() {
        let streams = Arc::new(TokenStreams::default());
        let permits: Vec<_> = (0..MAX_STREAMS_PER_TOKEN)
            .map(|_| streams.try_enter("token-a").expect("slot available"))
            .collect();
        assert!(streams.try_enter("token-a").is_none());
        // Another credential keeps its own budget.
        assert!(streams.try_enter("token-b").is_some());
        drop(permits);
        assert!(streams.try_enter("token-a").is_some());
    }

    #[test]
    fn count_returns_to_zero_and_vanishes() {
        let streams = Arc::new(TokenStreams::default());
        {
            let _permit = streams.try_enter("token-a").expect("slot available");
            assert_eq!(streams.counts.lock().unwrap().len(), 1);
        }
        assert!(streams.counts.lock().unwrap().is_empty());
    }
}
