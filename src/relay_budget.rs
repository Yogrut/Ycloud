//! Process-wide byte allowance for application-owned S3 relay payloads.
//! Network/TLS/SDK buffers and the rest of the process are not an RSS limit.
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MIB: usize = 1024 * 1024;
const FRAME_WINDOW: usize = MIB;
const FALLBACK_BYTES: usize = 16 * MIB;

struct RelayBudget {
    bytes: usize,
    permits: Arc<Semaphore>,
}

impl RelayBudget {
    fn new(bytes: usize) -> Self {
        Self {
            bytes,
            permits: Arc::new(Semaphore::new(bytes)),
        }
    }

    fn wrap(&self, body: Body) -> Body {
        let permits = self.permits.clone();
        Body::from_stream(futures_util::stream::try_unfold(
            (body.into_data_stream(), permits),
            |(mut stream, permits)| async move {
                loop {
                    // Reserve before polling: budget exhaustion must apply
                    // backpressure, not retain an uncharged frame while waiting.
                    let permit = permits
                        .clone()
                        .acquire_many_owned(FRAME_WINDOW as u32)
                        .await
                        .map_err(std::io::Error::other)?;
                    let Some(frame) = stream.next().await else {
                        return Ok::<_, std::io::Error>(None);
                    };
                    let bytes = frame.map_err(std::io::Error::other)?;
                    if bytes.len() > FRAME_WINDOW {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "relay input frame exceeds memory window",
                        ));
                    }
                    if bytes.is_empty() {
                        continue;
                    }
                    // Clones/slices keep the lease until the last consumer
                    // releases the underlying bytes, not just until body polling.
                    let bytes = Bytes::from_owner(BudgetedFrame {
                        bytes,
                        _permit: permit,
                    });
                    return Ok(Some((bytes, (stream, permits))));
                }
            },
        ))
    }
}

struct BudgetedFrame {
    bytes: Bytes,
    _permit: OwnedSemaphorePermit,
}
impl AsRef<[u8]> for BudgetedFrame {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

fn global() -> &'static RelayBudget {
    static BUDGET: OnceLock<RelayBudget> = OnceLock::new();
    BUDGET.get_or_init(|| RelayBudget::new(budget_for_memory(detect_memory_limit())))
}

pub(crate) fn wrap(body: Body) -> Body {
    global().wrap(body)
}
pub(crate) fn bytes() -> usize {
    global().bytes
}

fn budget_for_memory(memory: Option<u64>) -> usize {
    let amount = memory.map_or(FALLBACK_BYTES as u64, |bytes| bytes / 32);
    let amount = amount.clamp((4 * MIB) as u64, (32 * MIB) as u64) as usize;
    amount / FRAME_WINDOW * FRAME_WINDOW
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo(contents: &str) -> Option<u64> {
    let line = contents
        .lines()
        .find(|line| line.starts_with("MemTotal:"))?;
    let mut words = line.split_whitespace().skip(1);
    let kib = words.next()?.parse::<u64>().ok()?;
    (words.next()? == "kB")
        .then_some(kib.checked_mul(1024)?)
        .filter(|value| *value > 0)
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_limit(contents: &str) -> Option<u64> {
    contents
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
}

fn detect_memory_limit() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut limits = Vec::new();
        if let Some(total) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| parse_meminfo(&text))
        {
            limits.push(total);
        }
        let root = std::path::Path::new("/sys/fs/cgroup");
        let mut paths = vec![
            root.join("memory.max"),
            root.join("memory/memory.limit_in_bytes"),
        ];
        if let Ok(groups) = std::fs::read_to_string("/proc/self/cgroup") {
            if let Some(relative) = groups.lines().find_map(|line| line.strip_prefix("0::")) {
                let relative = std::path::Path::new(relative.trim_start_matches('/'));
                if relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                {
                    let mut path = root.join(relative);
                    while path.starts_with(root) {
                        paths.push(path.join("memory.max"));
                        if !path.pop() {
                            break;
                        }
                    }
                }
            }
        }
        for path in paths {
            if let Some(limit) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| parse_cgroup_limit(&text))
            {
                limits.push(limit);
            }
        }
        limits.into_iter().min()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn automatic_budget_respects_low_memory_and_has_a_small_upper_bound() {
        assert_eq!(budget_for_memory(Some(1024 * MIB as u64)), 32 * MIB);
        assert_eq!(budget_for_memory(Some(256 * MIB as u64)), 8 * MIB);
        assert_eq!(budget_for_memory(Some(64 * MIB as u64)), 4 * MIB);
        assert_eq!(budget_for_memory(Some(u64::MAX)), 32 * MIB);
        assert_eq!(budget_for_memory(None), 16 * MIB);
        assert_eq!(
            parse_meminfo("MemTotal: 262144 kB\n"),
            Some(256 * MIB as u64)
        );
        assert_eq!(parse_cgroup_limit("max"), None);
        assert_eq!(parse_cgroup_limit("268435456\n"), Some(256 * MIB as u64));
    }

    #[tokio::test]
    async fn independent_streams_share_bytes_and_do_not_read_ahead_when_full() {
        let budget = RelayBudget::new(2 * FRAME_WINDOW);
        let polls = Arc::new(AtomicUsize::new(0));
        let body = || {
            let polls = polls.clone();
            Body::from_stream(futures_util::stream::once(async move {
                polls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::io::Error>(Bytes::from_static(b"payload"))
            }))
        };
        let mut first = budget.wrap(body()).into_data_stream();
        let mut second = budget.wrap(body()).into_data_stream();
        let mut third = budget.wrap(body()).into_data_stream();
        let held = first.next().await.unwrap().unwrap();
        let other = second.next().await.unwrap().unwrap();
        assert_eq!(budget.permits.available_permits(), 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), third.next())
                .await
                .is_err()
        );
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        let clone = held.clone();
        drop(held);
        assert_eq!(budget.permits.available_permits(), 0);
        drop(clone);
        let last = third.next().await.unwrap().unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        drop((other, last, first, second, third));
        assert_eq!(budget.permits.available_permits(), 2 * FRAME_WINDOW);
    }

    #[tokio::test]
    async fn cancelled_waiters_and_oversize_frames_release_their_allowance() {
        let budget = RelayBudget::new(FRAME_WINDOW);
        let body = Body::from(vec![0u8; FRAME_WINDOW + 1]);
        assert!(budget
            .wrap(body)
            .into_data_stream()
            .next()
            .await
            .unwrap()
            .is_err());
        assert_eq!(budget.permits.available_permits(), FRAME_WINDOW);
        let mut pending = budget
            .wrap(Body::from_stream(futures_util::stream::pending::<
                Result<Bytes, std::io::Error>,
            >()))
            .into_data_stream();
        let mut waiter = Box::pin(pending.next());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiter)
                .await
                .is_err()
        );
        assert_eq!(budget.permits.available_permits(), 0);
        drop(waiter);
        drop(pending);
        assert_eq!(budget.permits.available_permits(), FRAME_WINDOW);
    }
}
