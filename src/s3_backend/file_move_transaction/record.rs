//! Persisted single-file move records and pure identity/stage validation.

use crate::{
    error::{AppError, AppResult},
    s3_backend::{internal_key, valid_transaction_id, RawS3Metadata},
    storage::StorageService,
};
use serde::{Deserialize, Serialize};

pub(super) const SCHEMA_VERSION: u32 = 1;
pub(super) const JOURNAL_CATEGORY: &str = "file-move-transactions";
const MAX_ETAG_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    Prepared,
    DestinationCopied,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObjectSnapshot {
    pub(super) size: u64,
    pub(super) etag: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Transaction {
    pub(super) schema_version: u32,
    pub(super) id: String,
    pub(super) source_relative: String,
    pub(super) destination_relative: String,
    pub(super) stage: Stage,
    pub(super) source: ObjectSnapshot,
    pub(super) destination: Option<ObjectSnapshot>,
}

pub(super) fn snapshot_matches(metadata: &RawS3Metadata, snapshot: &ObjectSnapshot) -> bool {
    metadata.size == snapshot.size && metadata.etag.as_deref() == Some(snapshot.etag.as_str())
}

pub(super) fn prepared_destination_matches(
    metadata: &RawS3Metadata,
    transaction: &Transaction,
) -> bool {
    metadata.size == transaction.source.size
        && metadata.etag.is_some()
        && (metadata.etag.as_deref() == Some(transaction.source.etag.as_str())
            || metadata.operation_id.as_deref() == Some(transaction.id.as_str()))
}

pub(super) fn validate_transaction(
    prefix: &str,
    journal_key: &str,
    transaction: &Transaction,
) -> AppResult<()> {
    let source = StorageService::normalize_relative(&transaction.source_relative)?;
    let destination = StorageService::normalize_relative(&transaction.destination_relative)?;
    let valid_destination = match transaction.stage {
        Stage::Prepared => transaction.destination.is_none(),
        Stage::DestinationCopied => transaction.destination.as_ref().is_some_and(|snapshot| {
            valid_snapshot(snapshot) && snapshot.size == transaction.source.size
        }),
    };
    if transaction.schema_version != SCHEMA_VERSION
        || !valid_transaction_id(&transaction.id)
        || journal_key != internal_key(prefix, JOURNAL_CATEGORY, &transaction.id)
        || source.is_empty()
        || destination.is_empty()
        || source != transaction.source_relative
        || destination != transaction.destination_relative
        || source == destination
        || !valid_snapshot(&transaction.source)
        || !valid_destination
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储单文件移动记录无法安全处理".into(),
        ));
    }
    Ok(())
}

fn valid_snapshot(snapshot: &ObjectSnapshot) -> bool {
    !snapshot.etag.is_empty()
        && snapshot.etag.len() <= MAX_ETAG_BYTES
        && !snapshot.etag.chars().any(char::is_control)
}
