//! Persisted directory manifests and their pure validation/authentication rules.
//! Remote execution and journal I/O intentionally remain outside this module.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    s3_backend::{
        authenticated_journal, internal_key, list_prefix, object_key, valid_transaction_id,
        S3MultipartSession,
    },
    storage::StorageService,
};

pub(super) const SCHEMA_VERSION: u32 = 3;
const JOURNAL_PURPOSE: &str = "directory-transaction:v2";
pub(super) const MAX_OBJECTS: usize = 1_000;
// Keep the existing conservative directory limit, not the provider's copy limit.
pub(super) const MAX_SINGLE_COPY_BYTES: u64 = 5_000_000_000;
pub(super) const JOURNAL_CATEGORY: &str = "directory-transactions";
pub(super) const TRASH_CATEGORY: &str = "directory-trash";
const MAX_OBJECT_KEY_BYTES: usize = 1_024;
const MAX_ETAG_BYTES: usize = 1_024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operation {
    Copy,
    Move,
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    CopyingTargets,
    CopyCompleted,
    DeletingSources,
    SourcesDeleted,
}

impl Operation {
    pub(super) fn completed_stage(self) -> Stage {
        match self {
            Self::Copy => Stage::CopyCompleted,
            Self::Move | Self::Delete => Stage::SourcesDeleted,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObjectRecord {
    pub(super) source_key: String,
    pub(super) target_key: String,
    pub(super) size: u64,
    pub(super) source_etag: String,
    pub(super) target_etag: Option<String>,
    pub(super) source_deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Transaction {
    pub(super) schema_version: u32,
    pub(super) id: String,
    pub(super) operation: Operation,
    pub(super) source_relative: String,
    pub(super) destination_relative: Option<String>,
    pub(super) stage: Stage,
    pub(super) objects: Vec<ObjectRecord>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) source_is_file: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) replacement: Option<Box<Transaction>>,
    #[serde(default)]
    pub(super) auth_tag: String,
}

pub(super) fn validate_transaction(
    auth_key: &[u8; 32],
    prefix: &str,
    journal_key: &str,
    transaction: &Transaction,
) -> AppResult<()> {
    if transaction.schema_version == 1 && transaction.auth_tag.is_empty() {
        return Err(AppError::ServiceUnavailable(
            "检测到旧版未认证目录事务；已保留记录并拒绝自动执行".into(),
        ));
    }
    validate_transaction_structure(prefix, journal_key, transaction)?;
    verify_transaction_auth(auth_key, transaction)
}

fn validate_transaction_structure(
    prefix: &str,
    journal_key: &str,
    transaction: &Transaction,
) -> AppResult<()> {
    if !matches!(transaction.schema_version, 2 | 3)
        || !valid_transaction_id(&transaction.id)
        || journal_key != internal_key(prefix, JOURNAL_CATEGORY, &transaction.id)
        || transaction.objects.is_empty()
        || transaction.objects.len() > MAX_OBJECTS
    {
        return Err(invalid_transaction());
    }
    if transaction.schema_version == 2
        && (transaction.source_is_file || transaction.replacement.is_some())
    {
        return Err(invalid_transaction());
    }
    let source = StorageService::normalize_relative(&transaction.source_relative)?;
    if source.is_empty() || source != transaction.source_relative {
        return Err(invalid_transaction());
    }
    let source_prefix = if transaction.source_is_file {
        object_key(prefix, &source)?
    } else {
        list_prefix(prefix, &source)?
    };
    let target_prefix = expected_target_prefix(prefix, transaction, &source)?;

    validate_transaction_progress(transaction)?;
    if let Some(replacement) = &transaction.replacement {
        if transaction.operation == Operation::Delete
            || replacement.operation != Operation::Delete
            || replacement.id != transaction.id
            || replacement.replacement.is_some()
            || Some(&replacement.source_relative) != transaction.destination_relative.as_ref()
            || replacement.stage != Stage::SourcesDeleted
                && (transaction.stage != Stage::CopyingTargets
                    || transaction
                        .objects
                        .iter()
                        .any(|object| object.target_etag.is_some() || object.source_deleted))
        {
            return Err(invalid_transaction());
        }
        validate_transaction_structure(prefix, journal_key, replacement)?;
    }

    let mut source_keys = BTreeSet::new();
    let mut target_keys = BTreeSet::new();
    for object in &transaction.objects {
        let suffix = object
            .source_key
            .strip_prefix(&source_prefix)
            .ok_or_else(invalid_transaction)?;
        if object.source_key.len() > MAX_OBJECT_KEY_BYTES
            || transaction.source_is_file && !suffix.is_empty()
            || object.target_key.len() > MAX_OBJECT_KEY_BYTES
            || object.source_etag.is_empty()
            || object.size > MAX_SINGLE_COPY_BYTES
            || object.source_etag.len() > MAX_ETAG_BYTES
            || object
                .target_etag
                .as_ref()
                .is_some_and(|etag| etag.is_empty() || etag.len() > MAX_ETAG_BYTES)
            || object.target_key != format!("{target_prefix}{suffix}")
            || object.source_key.chars().any(char::is_control)
            || object.target_key.chars().any(char::is_control)
            || !source_keys.insert(&object.source_key)
            || !target_keys.insert(&object.target_key)
        {
            return Err(invalid_transaction());
        }
    }
    Ok(())
}

fn validate_transaction_progress(transaction: &Transaction) -> AppResult<()> {
    // These are persisted checkpoints, not guesses about remote objects. Source
    // deletion can only start after every copied target has a recorded identity.
    let valid = match transaction.stage {
        Stage::CopyingTargets => transaction
            .objects
            .iter()
            .all(|object| !object.source_deleted),
        Stage::CopyCompleted => {
            transaction.operation.completed_stage() == transaction.stage
                && transaction
                    .objects
                    .iter()
                    .all(|object| object.target_etag.is_some() && !object.source_deleted)
        }
        Stage::DeletingSources => {
            transaction.operation != Operation::Copy
                && transaction
                    .objects
                    .iter()
                    .all(|object| object.target_etag.is_some())
        }
        Stage::SourcesDeleted => {
            transaction.operation.completed_stage() == transaction.stage
                && transaction
                    .objects
                    .iter()
                    .all(|object| object.target_etag.is_some() && object.source_deleted)
        }
    };
    if !valid {
        return Err(invalid_transaction());
    }
    Ok(())
}

fn transaction_auth_bytes(transaction: &Transaction) -> AppResult<Vec<u8>> {
    let mut unsigned = transaction.clone();
    unsigned.auth_tag.clear();
    let encoded = serde_json::to_vec(&unsigned).map_err(|error| {
        AppError::with_source("failed to authenticate S3 directory transaction", error)
    })?;
    Ok(encoded)
}

fn journal_purpose(transaction: &Transaction) -> &'static str {
    if transaction.schema_version == 2 {
        JOURNAL_PURPOSE
    } else {
        "directory-transaction:v3"
    }
}

pub(super) fn sign_transaction(
    auth_key: &[u8; 32],
    transaction: &mut Transaction,
) -> AppResult<()> {
    let payload = transaction_auth_bytes(transaction)?;
    transaction.auth_tag =
        authenticated_journal::sign_payload(auth_key, journal_purpose(transaction), &payload);
    Ok(())
}

fn verify_transaction_auth(auth_key: &[u8; 32], transaction: &Transaction) -> AppResult<()> {
    let payload = transaction_auth_bytes(transaction)?;
    authenticated_journal::verify_payload(
        auth_key,
        journal_purpose(transaction),
        &payload,
        &transaction.auth_tag,
    )
    .map_err(|_| invalid_transaction())
}

fn expected_target_prefix(
    prefix: &str,
    transaction: &Transaction,
    source: &str,
) -> AppResult<String> {
    match transaction.operation {
        Operation::Copy | Operation::Move => {
            let destination = transaction
                .destination_relative
                .as_deref()
                .ok_or_else(invalid_transaction)?;
            let normalized = StorageService::normalize_relative(destination)?;
            if normalized.is_empty()
                || normalized != destination
                || normalized == source
                || normalized.starts_with(&format!("{source}/"))
                || source.starts_with(&format!("{normalized}/"))
            {
                return Err(invalid_transaction());
            }
            if transaction.source_is_file {
                object_key(prefix, &normalized)
            } else {
                list_prefix(prefix, &normalized)
            }
        }
        Operation::Delete => {
            if transaction.destination_relative.is_some() {
                return Err(invalid_transaction());
            }
            Ok(internal_key(
                prefix,
                TRASH_CATEGORY,
                &format!("{}/", transaction.id),
            ))
        }
    }
}

pub(super) fn directory_multipart_target_matches(
    transaction: &Transaction,
    session: &S3MultipartSession,
) -> bool {
    (transaction.operation == Operation::Delete
        && transaction.objects.iter().any(|object| {
            object.target_key == session.key && Some(object.size) == session.expected_size
        }))
        || transaction
            .replacement
            .as_ref()
            .is_some_and(|replacement| directory_multipart_target_matches(replacement, session))
}

pub(super) fn directory_copy_id(transaction_id: &str, source_key: &str) -> String {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{transaction_id}:{source_key}").as_bytes(),
    );
    digest.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid_transaction() -> AppError {
    AppError::ServiceUnavailable("对象存储目录事务记录无法安全恢复".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_serialization_preserves_the_existing_authenticated_payload() {
        let payload = br#"{"schema_version":2,"id":"0123456789abcdef0123456789abcdef","operation":"copy","source_relative":"source","destination_relative":"destination","stage":"copying_targets","objects":[{"source_key":"tenant/source/file.bin","target_key":"tenant/destination/file.bin","size":42,"source_etag":"source-etag","target_etag":null,"source_deleted":false}],"auth_tag":""}"#;
        let mut transaction: Transaction = serde_json::from_slice(payload).unwrap();
        assert_eq!(transaction_auth_bytes(&transaction).unwrap(), payload);
        let expected_tag =
            authenticated_journal::sign_payload(&[0x41; 32], "directory-transaction:v2", payload);
        sign_transaction(&[0x41; 32], &mut transaction).unwrap();
        assert_eq!(transaction.auth_tag, expected_tag);
        let encoded = serde_json::to_vec(&transaction).unwrap();
        let restored: Transaction = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, transaction);
        validate_transaction(
            &[0x41; 32],
            "tenant/",
            &internal_key("tenant/", JOURNAL_CATEGORY, &transaction.id),
            &restored,
        )
        .unwrap();
    }

    #[test]
    fn manifest_object_key_and_etag_limits_remain_byte_bounded() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = copy_transaction(id);
        let template = transaction.objects[1].clone();
        transaction.objects = (0..MAX_OBJECTS)
            .map(|index| ObjectRecord {
                source_key: format!("tenant/source/{index}"),
                target_key: format!("tenant/destination/{index}"),
                ..template.clone()
            })
            .collect();
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.objects.push(ObjectRecord {
            source_key: "tenant/source/extra".into(),
            target_key: "tenant/destination/extra".into(),
            ..template.clone()
        });
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());

        let suffix = "x".repeat(MAX_OBJECT_KEY_BYTES - "tenant/destination/".len());
        transaction.objects = vec![ObjectRecord {
            source_key: format!("tenant/source/{suffix}"),
            target_key: format!("tenant/destination/{suffix}"),
            ..template
        }];
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.objects[0].source_key.push('x');
        transaction.objects[0].target_key.push('x');
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());

        transaction = copy_transaction(id);
        transaction.objects[1].source_etag = "é".repeat(MAX_ETAG_BYTES / 2);
        transaction.objects[1].target_etag = Some("x".repeat(MAX_ETAG_BYTES));
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.objects[1].source_etag.push('é');
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
        transaction.objects[1].source_etag = "source-etag".into();
        transaction.objects[1]
            .target_etag
            .as_mut()
            .unwrap()
            .push('x');
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
    }

    #[test]
    fn authenticated_large_delete_manifest_binds_its_multipart_target() {
        let id = "0123456789abcdef0123456789abcdef";
        let size = crate::s3_backend::S3_SINGLE_COPY_LIMIT + 1;
        let target = internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/file.bin"));
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Delete,
            source_relative: "source".into(),
            destination_relative: None,
            stage: Stage::CopyingTargets,
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: target.clone(),
                size,
                source_etag: "source-etag".into(),
                target_etag: None,
                source_deleted: false,
            }],
            source_is_file: false,
            replacement: None,
            auth_tag: String::new(),
        };
        sign_transaction(&[0x31; 32], &mut transaction).unwrap();
        validate_transaction(
            &[0x31; 32],
            "tenant/",
            &internal_key("tenant/", JOURNAL_CATEGORY, id),
            &transaction,
        )
        .unwrap();
        let session = crate::s3_backend::S3MultipartSession {
            schema_version: crate::s3_backend::S3_MULTIPART_SESSION_SCHEMA_VERSION,
            id: super::directory_copy_id(id, "tenant/source/file.bin"),
            key: target,
            purpose: Some(crate::s3_backend::S3MultipartPurpose::Copy),
            expected_size: Some(size),
            upload_id: None,
        };
        assert!(super::directory_multipart_target_matches(
            &transaction,
            &session
        ));
        crate::s3_backend::validate_multipart_session(
            "tenant/",
            &internal_key("tenant/", "multipart-sessions", &session.id),
            &session,
        )
        .unwrap();
        assert_eq!(
            session.id,
            super::directory_copy_id(id, "tenant/source/file.bin")
        );
    }

    #[test]
    fn copy_manifest_is_confined_to_exact_prefix_mapping() {
        let id = "0123456789abcdef0123456789abcdef";
        let transaction = copy_transaction(id);
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());

        let mut escaped = transaction.clone();
        escaped.objects[1].target_key = "tenant/outside/file.bin".into();
        assert!(validate_transaction_structure("tenant/", &key, &escaped).is_err());

        let mut duplicate = transaction.clone();
        duplicate.objects.push(duplicate.objects[0].clone());
        assert!(validate_transaction_structure("tenant/", &key, &duplicate).is_err());
    }

    #[test]
    fn delete_manifest_only_targets_internal_trash() {
        let id = "fedcba9876543210fedcba9876543210";
        let mut transaction = Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Delete,
            source_relative: "source".into(),
            destination_relative: None,
            stage: Stage::SourcesDeleted,
            objects: vec![ObjectRecord {
                source_key: "tenant/source/file.bin".into(),
                target_key: format!("tenant/.ycloud-system/{TRASH_CATEGORY}/{id}/file.bin"),
                size: 42,
                source_etag: "source-etag".into(),
                target_etag: Some("trash-etag".into()),
                source_deleted: true,
            }],
            source_is_file: false,
            replacement: None,
            auth_tag: String::new(),
        };
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.objects[0].target_key = "tenant/source-backup/file.bin".into();
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
    }

    #[test]
    fn impossible_stage_and_duplicate_keys_are_rejected() {
        let id = "00112233445566778899aabbccddeeff";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = copy_transaction(id);
        transaction.operation = Operation::Move;
        transaction.stage = Stage::SourcesDeleted;
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());

        let mut oversized = copy_transaction(id);
        oversized.objects[1].size = super::MAX_SINGLE_COPY_BYTES + 1;
        assert!(validate_transaction_structure("tenant/", &key, &oversized).is_err());
    }

    #[test]
    fn directory_stages_accept_only_progress_the_executor_can_persist() {
        // Indices describe one object's persisted proof, not remote state.
        let progress = [(false, false), (true, false), (false, true), (true, true)];
        let cases: [(Operation, Stage, &[usize]); 12] = [
            (Operation::Copy, Stage::CopyingTargets, &[0, 1]),
            (Operation::Copy, Stage::CopyCompleted, &[1]),
            (Operation::Copy, Stage::DeletingSources, &[]),
            (Operation::Copy, Stage::SourcesDeleted, &[]),
            (Operation::Move, Stage::CopyingTargets, &[0, 1]),
            (Operation::Move, Stage::CopyCompleted, &[]),
            (Operation::Move, Stage::DeletingSources, &[1, 3]),
            (Operation::Move, Stage::SourcesDeleted, &[3]),
            (Operation::Delete, Stage::CopyingTargets, &[0, 1]),
            (Operation::Delete, Stage::CopyCompleted, &[]),
            (Operation::Delete, Stage::DeletingSources, &[1, 3]),
            (Operation::Delete, Stage::SourcesDeleted, &[3]),
        ];
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        for (operation, stage, accepted) in cases {
            for first in 0..progress.len() {
                for second in 0..progress.len() {
                    let mut transaction = copy_transaction(id);
                    transaction.operation = operation;
                    transaction.stage = stage;
                    if operation == Operation::Delete {
                        transaction.destination_relative = None;
                        for object in &mut transaction.objects {
                            let suffix = object.source_key.strip_prefix("tenant/source/").unwrap();
                            object.target_key =
                                internal_key("tenant/", TRASH_CATEGORY, &format!("{id}/{suffix}"));
                        }
                    }
                    for (object, index) in transaction.objects.iter_mut().zip([first, second]) {
                        let (copied, deleted) = progress[index];
                        object.target_etag = copied.then(|| "copied-etag".into());
                        object.source_deleted = deleted;
                    }
                    sign_transaction(&[0x41; 32], &mut transaction).unwrap();
                    assert_eq!(
                        validate_transaction(&[0x41; 32], "tenant/", &key, &transaction).is_ok(),
                        accepted.contains(&first) && accepted.contains(&second),
                        "{operation:?}/{stage:?}, object progress {first}/{second}"
                    );
                }
            }
        }
    }

    #[test]
    fn completed_copy_checkpoint_requires_every_target_to_be_recorded() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = copy_transaction(id);
        transaction.stage = Stage::CopyCompleted;
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
        for object in &mut transaction.objects {
            object.target_etag = Some("copied-etag".into());
        }
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_ok());
        transaction.operation = Operation::Move;
        assert!(validate_transaction_structure("tenant/", &key, &transaction).is_err());
    }

    #[test]
    fn authenticated_manifest_rejects_tampering_wrong_installation_and_legacy_records() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let auth_key = [0x41; 32];
        let mut transaction = copy_transaction(id);
        sign_transaction(&auth_key, &mut transaction).unwrap();
        assert!(validate_transaction(&auth_key, "tenant/", &key, &transaction).is_ok());

        let mut tampered = transaction.clone();
        tampered.objects[1].size += 1;
        assert!(validate_transaction(&auth_key, "tenant/", &key, &tampered).is_err());
        assert!(validate_transaction(&[0x42; 32], "tenant/", &key, &transaction).is_err());

        let mut legacy = copy_transaction(id);
        legacy.schema_version = 1;
        assert!(validate_transaction(&auth_key, "tenant/", &key, &legacy).is_err());
    }

    fn copy_transaction(id: &str) -> Transaction {
        Transaction {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            operation: Operation::Copy,
            source_relative: "source".into(),
            destination_relative: Some("destination".into()),
            stage: Stage::CopyingTargets,
            objects: vec![
                ObjectRecord {
                    source_key: "tenant/source/".into(),
                    target_key: "tenant/destination/".into(),
                    size: 0,
                    source_etag: "marker-etag".into(),
                    target_etag: Some("copied-marker-etag".into()),
                    source_deleted: false,
                },
                ObjectRecord {
                    source_key: "tenant/source/nested/file.bin".into(),
                    target_key: "tenant/destination/nested/file.bin".into(),
                    size: 42,
                    source_etag: "file-etag".into(),
                    target_etag: None,
                    source_deleted: false,
                },
            ],
            source_is_file: false,
            replacement: None,
            auth_tag: String::new(),
        }
    }

    #[test]
    fn overwrite_retains_each_existing_manifest_budget_instead_of_halving_it() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut transaction = copy_transaction(id);
        let mut template = transaction.objects[0].clone();
        template.target_etag = None;
        transaction.objects = (0..600)
            .map(|index| ObjectRecord {
                source_key: format!("tenant/source/{index}.bin"),
                target_key: format!("tenant/destination/{index}.bin"),
                ..template.clone()
            })
            .collect();
        let mut replacement = copy_transaction(id);
        replacement.operation = Operation::Delete;
        replacement.source_relative = "destination".into();
        replacement.destination_relative = None;
        replacement.objects = (0..600)
            .map(|index| ObjectRecord {
                source_key: format!("tenant/destination/old-{index}.bin"),
                target_key: internal_key(
                    "tenant/",
                    TRASH_CATEGORY,
                    &format!("{id}/old-{index}.bin"),
                ),
                ..template.clone()
            })
            .collect();
        transaction.replacement = Some(Box::new(replacement));
        sign_transaction(&[0x41; 32], &mut transaction).unwrap();
        validate_transaction(&[0x41; 32], "tenant/", &key, &transaction).unwrap();
    }
}
