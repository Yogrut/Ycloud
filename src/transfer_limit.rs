use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use axum::{body::Body, response::Response};
use futures_util::StreamExt;
use tokio::sync::{Mutex, Notify};

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
    observed_rate: u64,
}

/// A process-wide streaming token bucket. `0` disables throttling. Waiting is
/// cancellation-safe: dropping a request also drops its pending sleep without
/// retaining a queue entry or permit.
#[derive(Clone, Debug)]
pub struct BandwidthLimiter {
    rate: Arc<AtomicU64>,
    bucket: Arc<Mutex<BucketState>>,
    changed: Arc<Notify>,
}

impl BandwidthLimiter {
    pub fn new(bytes_per_second: u64) -> Self {
        Self {
            rate: Arc::new(AtomicU64::new(bytes_per_second)),
            bucket: Arc::new(Mutex::new(BucketState {
                tokens: bytes_per_second as f64,
                last_refill: Instant::now(),
                observed_rate: bytes_per_second,
            })),
            changed: Arc::new(Notify::new()),
        }
    }

    pub fn set_rate(&self, bytes_per_second: u64) {
        if self.rate.swap(bytes_per_second, Ordering::Relaxed) != bytes_per_second {
            self.changed.notify_waiters();
        }
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    pub async fn consume(&self, bytes: usize) {
        // Unlimited streams retain the adapter, but need no bucket lock.
        if self.rate() == 0 {
            return;
        }
        let mut remaining = bytes as u64;
        while remaining > 0 {
            // Register before inspecting the rate so a concurrent update
            // cannot be lost between that inspection and beginning the wait.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let outcome = {
                let mut bucket = self.bucket.lock().await;
                let rate = self.rate();
                if rate == 0 {
                    return;
                }
                let portion = remaining.min(rate);
                let now = Instant::now();
                if bucket.observed_rate != rate {
                    bucket.observed_rate = rate;
                    bucket.tokens = bucket.tokens.min(rate as f64);
                    bucket.last_refill = now;
                }
                let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
                bucket.tokens = (bucket.tokens + elapsed * rate as f64).min(rate as f64);
                bucket.last_refill = now;
                if bucket.tokens >= portion as f64 {
                    bucket.tokens -= portion as f64;
                    Ok(portion)
                } else {
                    let missing = portion as f64 - bucket.tokens;
                    Err(Duration::from_secs_f64(missing / rate as f64))
                }
            };
            match outcome {
                Ok(consumed) => remaining -= consumed,
                // Never hold the shared bucket while waiting. Competing
                // streams re-check the budget after waking, so cancellation
                // creates no reserved-token debt and the global cap remains
                // authoritative.
                Err(wait) => tokio::select! {
                    _ = tokio::time::sleep(wait) => {},
                    _ = &mut changed => {},
                },
            }
        }
    }

    pub fn wrap_body(&self, body: Body) -> Body {
        // Keep the adapter even at rate zero: an already active transfer must
        // observe later configuration changes at the next unconsumed frame.
        let limiter = self.clone();
        let stream = body.into_data_stream().then(move |result| {
            let limiter = limiter.clone();
            async move {
                if let Ok(bytes) = &result {
                    limiter.consume(bytes.len()).await;
                }
                result
            }
        });
        Body::from_stream(stream)
    }

    pub fn wrap_response(&self, response: Response) -> Response {
        let (parts, body) = response.into_parts();
        Response::from_parts(parts, self.wrap_body(body))
    }
}

#[cfg(test)]
mod tests {
    use super::BandwidthLimiter;
    use std::time::Duration;

    #[tokio::test]
    async fn zero_rate_is_disabled_and_updates_are_visible() {
        let limiter = BandwidthLimiter::new(0);
        limiter.consume(1024).await;
        limiter.set_rate(64 * 1024);
        assert_eq!(limiter.rate(), 64 * 1024);
    }

    #[tokio::test]
    async fn waiting_consumer_does_not_hold_the_bucket_mutex() {
        let limiter = BandwidthLimiter::new(100);
        limiter.consume(100).await;
        let waiting = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.consume(100).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Another consumer must be able to inspect the bucket while the first
        // one sleeps. Setting the rate to zero gives that inspection an
        // immediate completion path.
        limiter.set_rate(0);
        tokio::time::timeout(Duration::from_millis(100), limiter.consume(1))
            .await
            .expect("sleeping consumer must not retain the bucket mutex");
        tokio::time::timeout(Duration::from_millis(100), waiting)
            .await
            .expect("disabling throttling must wake the pending consumer")
            .unwrap();
    }

    #[tokio::test]
    async fn already_wrapped_unlimited_transfers_observe_a_new_limit() {
        use axum::{
            body::{to_bytes, Body},
            response::Response,
        };

        for response_mode in [false, true] {
            let limiter = BandwidthLimiter::new(0);
            let body = if response_mode {
                limiter
                    .wrap_response(Response::new(Body::from("payload")))
                    .into_body()
            } else {
                limiter.wrap_body(Body::from("payload"))
            };
            limiter.set_rate(1);
            let mut reading = Box::pin(to_bytes(body, 1024));
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut reading)
                    .await
                    .is_err()
            );
            limiter.set_rate(0);
            let bytes = tokio::time::timeout(Duration::from_millis(100), reading)
                .await
                .expect("new zero rate must release the same active transfer")
                .unwrap();
            assert_eq!(bytes.as_ref(), b"payload");
        }
    }
}
