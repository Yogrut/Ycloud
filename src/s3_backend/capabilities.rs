use serde::Serialize;

use crate::config::S3Provider;

pub(super) const PREFIX_LIST: &str = "prefix_list";
pub(super) const CONDITIONAL_JOURNAL: &str = "conditional_journal";
pub(super) const CONDITIONAL_CREATE: &str = "conditional_create";
pub(super) const HEAD_METADATA: &str = "head_metadata";
pub(super) const CONDITIONAL_READ: &str = "conditional_read";
pub(super) const SERVER_SIDE_COPY: &str = "server_side_copy";
pub(super) const CONFIRMED_DELETE: &str = "confirmed_delete";
pub(super) const CONDITIONAL_JOURNAL_UPDATE: &str = "conditional_journal_update";
pub(super) const RANGE_READ: &str = "range_read";
pub(super) const MULTIPART_CREATE: &str = "multipart_create";
pub(super) const MULTIPART_LIST_EXACT_KEY: &str = "multipart_list_exact_key";
pub(super) const MULTIPART_PART_COPY: &str = "multipart_part_copy";
pub(super) const MULTIPART_ABORT: &str = "multipart_abort";

const ACTIVATION_CAPABILITIES: &[&str] = &[
    PREFIX_LIST,
    CONDITIONAL_JOURNAL,
    CONDITIONAL_CREATE,
    HEAD_METADATA,
    CONDITIONAL_READ,
    SERVER_SIDE_COPY,
    CONFIRMED_DELETE,
];

const FIRST_USE_CAPABILITIES: &[&str] = &[
    CONDITIONAL_JOURNAL_UPDATE,
    RANGE_READ,
    MULTIPART_CREATE,
    MULTIPART_LIST_EXACT_KEY,
    MULTIPART_PART_COPY,
    MULTIPART_ABORT,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalCreateMode {
    S3IfNoneMatch,
    OssForbidOverwrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalUpdateMode {
    S3IfMatch,
    HeadThenWriteExclusivePrefix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalDeleteMode {
    S3IfMatch,
    HeadThenDeleteExclusivePrefix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct S3CapabilityReport {
    pub profile_version: u32,
    pub provider: S3Provider,
    pub conditional_create: ConditionalCreateMode,
    pub conditional_update: ConditionalUpdateMode,
    pub conditional_delete: ConditionalDeleteMode,
    pub activation_verified: &'static [&'static str],
    pub deferred_until_first_use: &'static [&'static str],
    pub requires_exclusive_internal_prefix: bool,
}

pub(super) fn report(provider: S3Provider) -> S3CapabilityReport {
    let (
        conditional_create,
        conditional_update,
        conditional_delete,
        requires_exclusive_internal_prefix,
    ) = match provider {
        S3Provider::AlibabaOss => (
            ConditionalCreateMode::OssForbidOverwrite,
            ConditionalUpdateMode::HeadThenWriteExclusivePrefix,
            ConditionalDeleteMode::HeadThenDeleteExclusivePrefix,
            true,
        ),
        S3Provider::TencentCos | S3Provider::Minio | S3Provider::S3Compatible => (
            ConditionalCreateMode::S3IfNoneMatch,
            ConditionalUpdateMode::S3IfMatch,
            ConditionalDeleteMode::S3IfMatch,
            false,
        ),
    };
    S3CapabilityReport {
        profile_version: 1,
        provider,
        conditional_create,
        conditional_update,
        conditional_delete,
        activation_verified: ACTIVATION_CAPABILITIES,
        deferred_until_first_use: FIRST_USE_CAPABILITIES,
        requires_exclusive_internal_prefix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_profiles_make_native_and_emulated_conditions_explicit() {
        let oss = report(S3Provider::AlibabaOss);
        assert_eq!(
            oss.conditional_create,
            ConditionalCreateMode::OssForbidOverwrite
        );
        assert_eq!(
            oss.conditional_update,
            ConditionalUpdateMode::HeadThenWriteExclusivePrefix
        );
        assert!(oss.requires_exclusive_internal_prefix);

        for provider in [
            S3Provider::TencentCos,
            S3Provider::Minio,
            S3Provider::S3Compatible,
        ] {
            let profile = report(provider);
            assert_eq!(
                profile.conditional_create,
                ConditionalCreateMode::S3IfNoneMatch
            );
            assert_eq!(profile.conditional_update, ConditionalUpdateMode::S3IfMatch);
            assert!(!profile.requires_exclusive_internal_prefix);
        }
        assert!(oss.activation_verified.contains(&CONDITIONAL_CREATE));
        assert!(oss.deferred_until_first_use.contains(&MULTIPART_CREATE));
    }
}
