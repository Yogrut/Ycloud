//! Persisted upload and multipart records and their namespace validation.
//! Validation is pure; authenticated journal I/O and recovery decisions live elsewhere.
use serde::{Deserialize, Serialize};

use super::{
    internal_key, valid_transaction_id, S3_MULTIPART_MAX_PARTS, S3_MULTIPART_MAX_PART_BYTES,
    S3_SINGLE_COPY_LIMIT,
};
use crate::{
    error::{AppError, AppResult},
    storage::StorageService,
};

pub(super) const S3_TRANSACTION_SCHEMA_VERSION: u32 = 1;
pub(super) const S3_MULTIPART_SESSION_SCHEMA_VERSION: u32 = 2;
const MAX_PROVIDER_UPLOAD_ID_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum S3UploadStage {
    Prepared,
    BackupCreated,
    DestinationCommitted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct S3ObjectSnapshot {
    pub(super) size: u64,
    pub(super) etag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct S3UploadTransaction {
    pub(super) schema_version: u32,
    pub(super) id: String,
    pub(super) relative: String,
    pub(super) stage: S3UploadStage,
    pub(super) temporary: S3ObjectSnapshot,
    pub(super) previous: Option<S3ObjectSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct S3MultipartSession {
    pub(super) schema_version: u32,
    pub(super) id: String,
    pub(super) key: String,
    /// Version 1 records always contain the provider ID. Version 2 is first
    /// persisted with `None`, before CreateMultipartUpload is attempted, so a
    /// missing ID proves no parts or signed URLs were released. Such an intent
    /// is discarded without touching remote sessions or destination objects.
    #[serde(default)]
    pub(super) upload_id: Option<String>,
    #[serde(default)]
    pub(super) purpose: Option<S3MultipartPurpose>,
    #[serde(default)]
    pub(super) expected_size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum S3MultipartPurpose {
    Upload,
    Copy,
}

pub(super) fn validate_multipart_session(
    prefix: &str,
    journal_key: &str,
    session: &S3MultipartSession,
) -> AppResult<()> {
    let internal_upload = is_internal_multipart_upload_key(prefix, &session.key);
    let internal_backup = is_internal_multipart_backup_key(prefix, &session.key);
    let regular_object = session.key.strip_prefix(prefix).is_some_and(|relative| {
        !relative.is_empty()
            && !relative.starts_with(".ycloud-system/")
            && StorageService::normalize_relative(relative)
                .is_ok_and(|normalized| normalized == relative)
    });
    let upload_id_valid = session
        .upload_id
        .as_deref()
        .is_none_or(valid_multipart_upload_id);
    let valid_state = match session.schema_version {
        // Read-only compatibility for records written before pre-create
        // intents were introduced.
        S3_TRANSACTION_SCHEMA_VERSION => {
            session.upload_id.is_some()
                && upload_id_valid
                && session.purpose.is_none()
                && session.expected_size.is_none()
                && (internal_upload || regular_object)
        }
        S3_MULTIPART_SESSION_SCHEMA_VERSION => {
            let expected_size = session.expected_size.unwrap_or(0);
            let within_provider_limit =
                expected_size <= S3_MULTIPART_MAX_PART_BYTES.saturating_mul(S3_MULTIPART_MAX_PARTS);
            upload_id_valid
                && within_provider_limit
                && match session.purpose {
                    Some(S3MultipartPurpose::Upload) => {
                        // Direct uploads use multipart even below the relay threshold.
                        internal_upload && session.expected_size.is_some()
                    }
                    Some(S3MultipartPurpose::Copy) => {
                        (regular_object
                            || internal_backup
                            || directory_trash_transaction_id(prefix, &session.key).is_some())
                            && expected_size > S3_SINGLE_COPY_LIMIT
                    }
                    None => false,
                }
        }
        _ => false,
    };
    if !valid_state
        || !valid_transaction_id(&session.id)
        || journal_key != internal_key(prefix, "multipart-sessions", &session.id)
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储分片恢复记录无法安全处理".into(),
        ));
    }
    Ok(())
}

fn valid_multipart_upload_id(upload_id: &str) -> bool {
    !upload_id.is_empty()
        && upload_id.len() <= MAX_PROVIDER_UPLOAD_ID_BYTES
        && !upload_id.chars().any(char::is_control)
}

fn is_internal_multipart_upload_key(prefix: &str, key: &str) -> bool {
    key.strip_prefix(&internal_key(prefix, "uploads", ""))
        .is_some_and(valid_transaction_id)
}

fn is_internal_multipart_backup_key(prefix: &str, key: &str) -> bool {
    key.strip_prefix(&internal_key(prefix, "backups", ""))
        .is_some_and(valid_transaction_id)
}

pub(super) fn directory_trash_transaction_id<'a>(prefix: &str, key: &'a str) -> Option<&'a str> {
    let relative = key.strip_prefix(&internal_key(prefix, "directory-trash", ""))?;
    let (id, suffix) = relative.split_once('/')?;
    (valid_transaction_id(id)
        && (suffix.is_empty()
            || StorageService::normalize_relative(suffix)
                .is_ok_and(|normalized| normalized == suffix.trim_end_matches('/'))))
    .then_some(id)
}

pub(super) fn validate_upload_transaction(
    prefix: &str,
    journal_key: &str,
    transaction: &S3UploadTransaction,
) -> AppResult<()> {
    if transaction.schema_version != S3_TRANSACTION_SCHEMA_VERSION
        || !valid_transaction_id(&transaction.id)
        || journal_key != internal_key(prefix, "transactions", &transaction.id)
        || transaction.temporary.etag.is_none()
        || transaction
            .previous
            .as_ref()
            .is_some_and(|snapshot| snapshot.etag.is_none())
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务记录无法安全恢复".into(),
        ));
    }
    let relative = StorageService::normalize_relative(&transaction.relative)?;
    if relative.is_empty() || relative != transaction.relative {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务记录包含无效目标路径".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        authenticated_journal, S3_MULTIPART_SESSION_JOURNAL_PURPOSE, S3_MULTIPART_THRESHOLD,
    };
    use super::*;

    #[test]
    fn transaction_records_are_confined_and_require_etags() {
        let transaction = S3UploadTransaction {
            schema_version: 1,
            id: "0123456789abcdef0123456789abcdef".into(),
            relative: "folder/file.bin".into(),
            stage: S3UploadStage::Prepared,
            temporary: S3ObjectSnapshot {
                size: 42,
                etag: Some("new".into()),
            },
            previous: Some(S3ObjectSnapshot {
                size: 21,
                etag: Some("old".into()),
            }),
        };
        let key = internal_key("tenant/", "transactions", &transaction.id);
        assert!(validate_upload_transaction("tenant/", &key, &transaction).is_ok());
        assert!(valid_transaction_id(&transaction.id));

        let mut escaped = transaction.clone();
        escaped.relative = "../outside".into();
        assert!(validate_upload_transaction("tenant/", &key, &escaped).is_err());

        let mut unverifiable = transaction.clone();
        unverifiable.temporary.etag = None;
        assert!(validate_upload_transaction("tenant/", &key, &unverifiable).is_err());
    }

    #[test]
    fn multipart_recovery_records_are_confined_to_the_backend_namespace() {
        let id = "0123456789abcdef0123456789abcdef";
        let mut session = S3MultipartSession {
            schema_version: 1,
            id: id.into(),
            key: format!("tenant/.ycloud-system/uploads/{id}"),
            upload_id: Some("provider-upload-id".into()),
            purpose: None,
            expected_size: None,
        };
        let journal_key = internal_key("tenant/", "multipart-sessions", id);
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

        session.upload_id = None;
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
        session.schema_version = S3_MULTIPART_SESSION_SCHEMA_VERSION;
        session.purpose = Some(S3MultipartPurpose::Upload);
        session.expected_size = Some(S3_MULTIPART_THRESHOLD);
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());
        for size in [0, 1, S3_MULTIPART_THRESHOLD - 1, S3_MULTIPART_THRESHOLD] {
            session.expected_size = Some(size);
            assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());
        }
        session.upload_id = Some("provider-upload-id".into());
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

        session.key = "tenant/folder/file.bin".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
        session.purpose = Some(S3MultipartPurpose::Copy);
        session.expected_size = Some(S3_SINGLE_COPY_LIMIT + 1);
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

        session.key = format!("tenant/.ycloud-system/backups/{id}");
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_ok());

        session.key = "other-tenant/file.bin".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());

        session.key = "tenant/.ycloud-system/transactions/forged".into();
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());

        session.key = "tenant/folder/file.bin".into();
        session.upload_id = Some("bad\nupload-id".into());
        assert!(validate_multipart_session("tenant/", &journal_key, &session).is_err());
    }

    #[test]
    fn multipart_record_round_trips_authenticated_legacy_and_current_formats() {
        let id = "0123456789abcdef0123456789abcdef";
        let key = internal_key("tenant/", "multipart-sessions", id);
        let auth_key = [0x39; 32];
        for (schema_version, upload_id, purpose, expected_size) in [
            (
                S3_TRANSACTION_SCHEMA_VERSION,
                Some("legacy-upload-id".to_owned()),
                None,
                None,
            ),
            (
                S3_MULTIPART_SESSION_SCHEMA_VERSION,
                None,
                Some(S3MultipartPurpose::Upload),
                Some(0),
            ),
            (
                S3_MULTIPART_SESSION_SCHEMA_VERSION,
                Some("current-upload-id".to_owned()),
                Some(S3MultipartPurpose::Upload),
                Some(S3_MULTIPART_THRESHOLD),
            ),
        ] {
            let session = S3MultipartSession {
                schema_version,
                id: id.into(),
                key: internal_key("tenant/", "uploads", id),
                upload_id,
                purpose,
                expected_size,
            };
            let bytes = authenticated_journal::encode(
                &auth_key,
                S3_MULTIPART_SESSION_JOURNAL_PURPOSE,
                &session,
            )
            .unwrap();
            let restored: S3MultipartSession = authenticated_journal::decode(
                &auth_key,
                S3_MULTIPART_SESSION_JOURNAL_PURPOSE,
                &bytes,
            )
            .unwrap();
            assert_eq!(restored, session);
            assert!(validate_multipart_session("tenant/", &key, &restored).is_ok());
            assert!(validate_multipart_session("another/", &key, &restored).is_err());
        }

        let legacy = format!(
            r#"{{"schema_version":1,"id":"{id}","key":"tenant/.ycloud-system/uploads/{id}","upload_id":"legacy-upload-id"}}"#
        );
        let restored: S3MultipartSession = serde_json::from_str(&legacy).unwrap();
        assert!(validate_multipart_session("tenant/", &key, &restored).is_ok());
        assert!(restored.purpose.is_none());
        assert!(restored.expected_size.is_none());
    }

    #[test]
    fn provider_upload_ids_are_bounded_in_bytes_and_reject_control_characters() {
        assert!(!valid_multipart_upload_id(""));
        assert!(!valid_multipart_upload_id("provider\nupload"));
        assert!(valid_multipart_upload_id(
            &"x".repeat(MAX_PROVIDER_UPLOAD_ID_BYTES)
        ));
        assert!(!valid_multipart_upload_id(
            &"x".repeat(MAX_PROVIDER_UPLOAD_ID_BYTES + 1)
        ));
        assert!(valid_multipart_upload_id(
            &"é".repeat(MAX_PROVIDER_UPLOAD_ID_BYTES / 2)
        ));
        assert!(!valid_multipart_upload_id(
            &"é".repeat(MAX_PROVIDER_UPLOAD_ID_BYTES / 2 + 1)
        ));
    }
}
