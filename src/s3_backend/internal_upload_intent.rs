use serde::{Deserialize, Serialize};

use super::{
    internal_key, valid_transaction_id, S3Backend, S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE,
    S3_MULTIPART_MAX_PARTS, S3_MULTIPART_MAX_PART_BYTES,
};
use crate::error::{AppError, AppResult, CleanupState, CommitState};

const SCHEMA_VERSION: u32 = 1;
const JOURNAL_CATEGORY: &str = "internal-upload-intents";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InternalUploadIntent {
    schema_version: u32,
    id: String,
    key: String,
    expected_size: u64,
    created_unix: i64,
}

impl S3Backend {
    pub(super) async fn create_internal_upload_intent(
        &self,
        id: &str,
        expected_size: u64,
    ) -> AppResult<(String, InternalUploadIntent, Option<String>)> {
        let journal_key = internal_key(&self.prefix, JOURNAL_CATEGORY, id);
        let intent = InternalUploadIntent {
            schema_version: SCHEMA_VERSION,
            id: id.to_owned(),
            key: internal_key(&self.prefix, "uploads", id),
            expected_size,
            created_unix: chrono::Utc::now().timestamp(),
        };
        validate_intent(&self.prefix, &journal_key, &intent)?;
        let etag = self
            .write_authenticated_json_journal(
                &journal_key,
                S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE,
                &intent,
                None,
            )
            .await?;
        Ok((journal_key, intent, etag))
    }

    pub(super) async fn release_internal_upload_intent(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
    ) -> AppResult<()> {
        self.delete_key_confirmed(journal_key, journal_etag).await
    }

    pub(super) async fn abandon_internal_upload(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        intent: &InternalUploadIntent,
    ) -> AppResult<()> {
        self.settle_internal_upload_intent(journal_key, journal_etag, intent)
            .await
    }

    pub(super) async fn finish_uncommitted_internal_upload(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        intent: &InternalUploadIntent,
        error: AppError,
    ) -> AppError {
        let cleanup = if self
            .abandon_internal_upload(journal_key, journal_etag, intent)
            .await
            .is_ok()
        {
            CleanupState::Complete
        } else {
            CleanupState::Pending
        };
        error.with_operation(CommitState::NotCommitted, cleanup)
    }

    pub(super) async fn recover_internal_upload_intents(&self) -> AppResult<usize> {
        let keys = self.list_recovery_journal_keys(JOURNAL_CATEGORY).await?;
        let recovered = keys.len();
        for key in keys {
            let (intent, journal_etag) = self
                .read_authenticated_json_journal(&key, S3_INTERNAL_UPLOAD_INTENT_JOURNAL_PURPOSE)
                .await?;
            validate_intent(&self.prefix, &key, &intent)?;
            self.settle_internal_upload_intent(&key, Some(&journal_etag), &intent)
                .await?;
        }
        Ok(recovered)
    }

    async fn settle_internal_upload_intent(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        intent: &InternalUploadIntent,
    ) -> AppResult<()> {
        validate_intent(&self.prefix, journal_key, intent)?;
        if let Some(object) = self.head_key(&intent.key).await? {
            if object.size != intent.expected_size
                || object.etag.is_none()
                || object.operation_id.as_deref() != Some(intent.id.as_str())
            {
                return Err(AppError::ServiceUnavailable(
                    "对象存储内部上传对象无法归属于恢复意图；已保留对象和记录".into(),
                ));
            }
            self.delete_key_confirmed(&intent.key, object.etag.as_deref())
                .await?;
        }
        self.delete_key_confirmed(journal_key, journal_etag).await
    }
}

fn validate_intent(
    prefix: &str,
    journal_key: &str,
    intent: &InternalUploadIntent,
) -> AppResult<()> {
    let expected_key = internal_key(prefix, "uploads", &intent.id);
    if intent.schema_version != SCHEMA_VERSION
        || !valid_transaction_id(&intent.id)
        || journal_key != internal_key(prefix, JOURNAL_CATEGORY, &intent.id)
        || intent.key != expected_key
        || intent.expected_size > S3_MULTIPART_MAX_PART_BYTES.saturating_mul(S3_MULTIPART_MAX_PARTS)
        || intent.created_unix <= 0
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储内部上传恢复意图无法安全处理".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_upload_intent_is_bound_to_one_exact_owned_object() {
        let id = "0123456789abcdef0123456789abcdef";
        let journal_key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut intent = InternalUploadIntent {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            key: internal_key("tenant/", "uploads", id),
            expected_size: 42,
            created_unix: 1,
        };
        assert!(validate_intent("tenant/", &journal_key, &intent).is_ok());

        intent.key = internal_key("other/", "uploads", id);
        assert!(validate_intent("tenant/", &journal_key, &intent).is_err());
        intent.key = internal_key("tenant/", "backups", id);
        assert!(validate_intent("tenant/", &journal_key, &intent).is_err());
        intent.key = internal_key("tenant/", "uploads", id);
        intent.created_unix = 0;
        assert!(validate_intent("tenant/", &journal_key, &intent).is_err());
    }
}
