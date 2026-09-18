use serde::{Deserialize, Serialize};

use super::{internal_key, valid_transaction_id, S3Backend, S3_ACTIVATION_PROBE_JOURNAL_PURPOSE};
use crate::error::{AppError, AppResult};

const SCHEMA_VERSION: u32 = 1;
const JOURNAL_CATEGORY: &str = "activation-probe-intents";
const OBJECT_CATEGORY: &str = "activation-tests";
const MAX_PROBE_BYTES: u64 = 4 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActivationProbeIntent {
    schema_version: u32,
    id: String,
    source_key: String,
    copy_key: String,
    expected_size: u64,
    created_unix: i64,
}

impl S3Backend {
    pub(super) async fn create_activation_probe_intent(
        &self,
        id: &str,
        expected_size: u64,
    ) -> AppResult<(String, ActivationProbeIntent, Option<String>)> {
        let journal_key = internal_key(&self.prefix, JOURNAL_CATEGORY, id);
        let intent = ActivationProbeIntent {
            schema_version: SCHEMA_VERSION,
            id: id.to_owned(),
            source_key: internal_key(&self.prefix, OBJECT_CATEGORY, id),
            copy_key: internal_key(&self.prefix, OBJECT_CATEGORY, &format!("{id}-copy")),
            expected_size,
            created_unix: chrono::Utc::now().timestamp(),
        };
        validate_intent(&self.prefix, &journal_key, &intent)?;
        let etag = self
            .write_authenticated_json_journal(
                &journal_key,
                S3_ACTIVATION_PROBE_JOURNAL_PURPOSE,
                &intent,
                None,
            )
            .await?;
        Ok((journal_key, intent, etag))
    }

    pub(super) async fn recover_activation_probe_intents(&self) -> AppResult<usize> {
        let keys = self.list_recovery_journal_keys(JOURNAL_CATEGORY).await?;
        let recovered = keys.len();
        for key in keys {
            let (intent, journal_etag) = self
                .read_authenticated_json_journal(&key, S3_ACTIVATION_PROBE_JOURNAL_PURPOSE)
                .await?;
            validate_intent(&self.prefix, &key, &intent)?;
            self.settle_activation_probe_intent(&key, Some(&journal_etag), &intent)
                .await?;
        }
        Ok(recovered)
    }

    pub(super) async fn settle_activation_probe_intent(
        &self,
        journal_key: &str,
        journal_etag: Option<&str>,
        intent: &ActivationProbeIntent,
    ) -> AppResult<()> {
        validate_intent(&self.prefix, journal_key, intent)?;
        self.delete_owned_probe_object(&intent.copy_key, intent)
            .await?;
        self.delete_owned_probe_object(&intent.source_key, intent)
            .await?;
        self.delete_key_confirmed(journal_key, journal_etag).await
    }

    async fn delete_owned_probe_object(
        &self,
        key: &str,
        intent: &ActivationProbeIntent,
    ) -> AppResult<()> {
        let Some(object) = self.head_key(key).await? else {
            return Ok(());
        };
        if object.size != intent.expected_size
            || object.etag.is_none()
            || object.operation_id.as_deref() != Some(intent.id.as_str())
        {
            return Err(AppError::ServiceUnavailable(
                "对象存储激活探针对象无法归属于恢复意图；已保留对象和记录".into(),
            ));
        }
        self.delete_key_confirmed(key, object.etag.as_deref()).await
    }
}

fn validate_intent(
    prefix: &str,
    journal_key: &str,
    intent: &ActivationProbeIntent,
) -> AppResult<()> {
    if intent.schema_version != SCHEMA_VERSION
        || !valid_transaction_id(&intent.id)
        || journal_key != internal_key(prefix, JOURNAL_CATEGORY, &intent.id)
        || intent.source_key != internal_key(prefix, OBJECT_CATEGORY, &intent.id)
        || intent.copy_key != internal_key(prefix, OBJECT_CATEGORY, &format!("{}-copy", intent.id))
        || intent.expected_size == 0
        || intent.expected_size > MAX_PROBE_BYTES
        || intent.created_unix <= 0
    {
        return Err(AppError::ServiceUnavailable(
            "对象存储激活探针恢复意图无法安全处理".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_intent_is_confined_to_two_exact_installation_owned_objects() {
        let id = "0123456789abcdef0123456789abcdef";
        let journal_key = internal_key("tenant/", JOURNAL_CATEGORY, id);
        let mut intent = ActivationProbeIntent {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            source_key: internal_key("tenant/", OBJECT_CATEGORY, id),
            copy_key: internal_key("tenant/", OBJECT_CATEGORY, &format!("{id}-copy")),
            expected_size: 42,
            created_unix: 1,
        };
        assert!(validate_intent("tenant/", &journal_key, &intent).is_ok());

        intent.copy_key = internal_key("tenant/", OBJECT_CATEGORY, "different-copy");
        assert!(validate_intent("tenant/", &journal_key, &intent).is_err());
        intent.copy_key = internal_key("tenant/", OBJECT_CATEGORY, &format!("{id}-copy"));
        intent.source_key = internal_key("other/", OBJECT_CATEGORY, id);
        assert!(validate_intent("tenant/", &journal_key, &intent).is_err());
        intent.source_key = internal_key("tenant/", OBJECT_CATEGORY, id);
        intent.expected_size = MAX_PROBE_BYTES + 1;
        assert!(validate_intent("tenant/", &journal_key, &intent).is_err());
    }
}
