//! Bounded group commit; file I/O never holds the public ledger mutex.
use super::{failure, Direction, Record, Runtime, TrafficStore};
use crate::{
    config::SharedConfig,
    error::{AppError, AppResult},
};
use std::{
    collections::BTreeSet,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot, Mutex};

const MAX_PENDING_CHARGES: usize = 64;
const MAX_COMMIT_RECORDS: usize = 64;
const COMPACTION_RECORDS: usize = 4096;

pub(super) struct ChargeRequest {
    pub subject: String,
    pub direction: Direction,
    pub bytes: u64,
    pub reply: oneshot::Sender<AppResult<()>>,
}

pub(super) fn start(
    runtime: Arc<Mutex<Runtime>>,
    journal: std::fs::File,
    snapshot: PathBuf,
    config: SharedConfig,
) -> mpsc::Sender<ChargeRequest> {
    let (sender, receiver) = mpsc::channel(MAX_PENDING_CHARGES);
    // Do not retain a sender here: dropping the last store closes the queue;
    // admitted requests are drained before the file handle is released.
    tokio::spawn(run(runtime, journal, snapshot, config, receiver));
    sender
}

async fn run(
    runtime: Arc<Mutex<Runtime>>,
    mut journal: std::fs::File,
    snapshot: PathBuf,
    config: SharedConfig,
    mut receiver: mpsc::Receiver<ChargeRequest>,
) {
    while let Some(first) = receiver.recv().await {
        let current = config.read().await;
        let state = runtime.lock().await;
        let mut requests = vec![first];
        while requests.len() < MAX_COMMIT_RECORDS {
            let Ok(request) = receiver.try_recv() else {
                break;
            };
            requests.push(request);
        }
        if state.failed {
            drop(state);
            for request in requests {
                let _ = request
                    .reply
                    .send(Err(AppError::ServiceUnavailable("流量记账暂不可用".into())));
            }
            continue;
        }
        let mut ledger = state.ledger.clone();
        let previous_records = state.records;
        #[cfg(test)]
        let pause = state.commit_pause.clone();
        drop(state);
        let mut replies = Vec::with_capacity(requests.len());
        let mut encoded = Vec::new();
        let mut accepted = 0;
        for request in requests {
            ledger.advance(chrono::Utc::now().timestamp(), &current.traffic.cycle);
            let result = TrafficStore::check(
                &ledger,
                &current.traffic,
                &request.subject,
                request.direction,
                request.bytes,
            );
            if result.is_ok() {
                let record = Record {
                    sequence: ledger.sequence + 1,
                    time: ledger.last_time,
                    subject: request.subject,
                    direction: request.direction,
                    bytes: request.bytes,
                    cycle: current.traffic.cycle.clone(),
                };
                // Serialize separately so an encoding failure cannot leave a
                // partial record in an otherwise valid commit group.
                let bytes = match serde_json::to_vec(&record) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let _ = request.reply.send(Err(failure(error)));
                        continue;
                    }
                };
                encoded.extend_from_slice(&bytes);
                encoded.push(b'\n');
                ledger.apply(&record);
                accepted += 1;
            }
            replies.push((request.reply, result));
        }
        let compact = previous_records + accepted >= COMPACTION_RECORDS;
        let compacted = if compact {
            let users: BTreeSet<&str> = current
                .user_accounts
                .iter()
                .map(|user| user.id.as_str())
                .chain(current.traffic.users.keys().map(String::as_str))
                .collect();
            ledger.users.retain(|id, _| users.contains(id.as_str()));
            Some(serde_json::to_vec(&ledger).map_err(failure))
        } else {
            None
        };
        drop(current);
        if accepted == 0 {
            for (reply, result) in replies {
                let _ = reply.send(result);
            }
            continue;
        }
        let target = snapshot.clone();
        #[cfg(test)]
        let _pause = pause.lock().await;
        let written = tokio::task::spawn_blocking(move || {
            let result = (|| -> AppResult<()> {
                journal.seek(SeekFrom::End(0)).map_err(failure)?;
                journal.write_all(&encoded).map_err(failure)?;
                journal.sync_data().map_err(failure)?;
                if let Some(bytes) = compacted {
                    crate::config::publish_traffic_snapshot(&target, &bytes?).map_err(failure)?;
                    journal.set_len(0).map_err(failure)?;
                    journal.sync_all().map_err(failure)?;
                }
                Ok(())
            })();
            (journal, result)
        })
        .await;
        let mut state = runtime.lock().await;
        let error = match written {
            Ok((file, result)) => {
                journal = file;
                result.err()
            }
            Err(error) => {
                state.failed = true;
                for (reply, _) in replies {
                    let _ = reply.send(Err(failure(anyhow::anyhow!(error.to_string()))));
                }
                // Dropping the receiver wakes queued/waiting callers with errors.
                return;
            }
        };
        if error.is_none() {
            state.ledger = ledger;
            state.records = if compact {
                0
            } else {
                previous_records + accepted
            };
            #[cfg(test)]
            {
                state.commits += 1;
            }
        } else {
            state.failed = true;
        }
        drop(state);
        for (reply, result) in replies {
            let result = match (&error, result) {
                (Some(error), Ok(())) => Err(failure(anyhow::anyhow!(error.to_string()))),
                (_, result) => result,
            };
            let _ = reply.send(result);
        }
    }
}
