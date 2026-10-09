//! HTTP write preconditions, evaluated again at the backend's publication boundary.
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{header, HeaderMap};

use crate::error::{AppError, AppResult};

const MAX_CONDITION_BYTES: usize = 4096;
const MAX_CONDITION_TAGS: usize = 32;

#[derive(Clone, Debug, Default)]
pub(crate) struct WriteConditions {
    if_match: Option<TagCondition>,
    if_none_match: Option<TagCondition>,
    unmodified_since: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub(super) enum TagCondition {
    Any,
    Tags(Vec<String>),
}

impl WriteConditions {
    pub fn parse(headers: &HeaderMap) -> AppResult<Self> {
        let if_match = tag_condition(headers, header::IF_MATCH)?;
        let if_none_match = tag_condition(headers, header::IF_NONE_MATCH)?;
        // HTTP ignores an invalid date and ignores this date whenever If-Match
        // is present. Do not let a stale date override a matching entity tag.
        let unmodified_since = if if_match.is_none() {
            let mut values = headers.get_all(header::IF_UNMODIFIED_SINCE).iter();
            let value = values.next();
            if values.next().is_some() {
                return Err(AppError::BadRequest("If-Unmodified-Since 不能重复".into()));
            }
            value
                .and_then(|value| value.to_str().ok())
                .and_then(|value| httpdate::parse_http_date(value).ok())
        } else {
            None
        };
        Ok(Self {
            if_match,
            if_none_match,
            unmodified_since,
        })
    }

    pub fn is_conditional(&self) -> bool {
        self.if_match.is_some() || self.if_none_match.is_some() || self.unmodified_since.is_some()
    }

    pub fn has_date(&self) -> bool {
        self.unmodified_since.is_some()
    }

    pub fn check_local(&self, metadata: Option<&std::fs::Metadata>) -> AppResult<()> {
        self.check(
            metadata.is_some(),
            None,
            metadata.and_then(|value| value.modified().ok()),
        )
    }

    pub fn check(
        &self,
        exists: bool,
        etag: Option<&str>,
        modified: Option<SystemTime>,
    ) -> AppResult<()> {
        if let Some(condition) = &self.if_match {
            if !condition.matches(exists, etag, false) {
                return Err(AppError::PreconditionFailed);
            }
        } else if exists && self.unmodified_since.is_some() {
            let known = modified
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|time| time.as_secs());
            let expected = self
                .unmodified_since
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|time| time.as_secs());
            if known.is_none() || known > expected {
                return Err(AppError::PreconditionFailed);
            }
        }
        if let Some(condition) = &self.if_none_match {
            if condition.matches(exists, etag, true) {
                return Err(AppError::PreconditionFailed);
            }
            // Without a content validator a named negative condition cannot be
            // proven. A wildcard only needs the existence decision.
            if exists && etag.is_none() && matches!(condition, TagCondition::Tags(_)) {
                return Err(AppError::storage_capability(
                    "conditional_write",
                    "本地文件没有强版本标签，无法执行指定标签的 If-None-Match",
                ));
            }
        }
        Ok(())
    }
}

impl TagCondition {
    pub(super) fn matches(&self, exists: bool, actual: Option<&str>, weak: bool) -> bool {
        if !exists {
            return false;
        }
        match self {
            Self::Any => true,
            Self::Tags(tags) => actual.is_some_and(|actual| {
                tags.iter().any(|expected| {
                    if weak {
                        expected.strip_prefix("W/").unwrap_or(expected)
                            == actual.strip_prefix("W/").unwrap_or(actual)
                    } else {
                        !expected.starts_with("W/")
                            && !actual.starts_with("W/")
                            && expected == actual
                    }
                })
            }),
        }
    }
}

pub(super) fn tag_condition(
    headers: &HeaderMap,
    name: header::HeaderName,
) -> AppResult<Option<TagCondition>> {
    let mut combined = String::new();
    for value in headers.get_all(&name).iter() {
        let value = value.to_str().map_err(|_| invalid_tags())?;
        if combined.len().saturating_add(value.len() + 1) > MAX_CONDITION_BYTES {
            return Err(invalid_tags());
        }
        if !combined.is_empty() {
            combined.push(',');
        }
        combined.push_str(value);
    }
    if combined.is_empty() {
        return if headers.contains_key(name) {
            Err(invalid_tags())
        } else {
            Ok(None)
        };
    }
    let mut remaining = combined.trim();
    if remaining == "*" {
        return Ok(Some(TagCondition::Any));
    }
    let mut tags = Vec::new();
    while !remaining.is_empty() {
        // Commas inside an opaque quoted tag are data, not list separators.
        let offset = usize::from(remaining.starts_with("W/")) * 2;
        if remaining.as_bytes().get(offset) != Some(&b'"') {
            return Err(invalid_tags());
        }
        let end = remaining[offset + 1..].find('"').ok_or_else(invalid_tags)? + offset + 2;
        let tag = &remaining[..end];
        if !super::read_conditions::is_entity_tag(tag) {
            return Err(invalid_tags());
        }
        tags.push(tag.to_owned());
        if tags.len() > MAX_CONDITION_TAGS {
            return Err(invalid_tags());
        }
        remaining = remaining[end..].trim_start();
        if remaining.is_empty() {
            break;
        }
        remaining = remaining
            .strip_prefix(',')
            .ok_or_else(invalid_tags)?
            .trim_start();
        if remaining.is_empty() {
            return Err(invalid_tags());
        }
    }
    Ok(Some(TagCondition::Tags(tags)))
}

fn invalid_tags() -> AppError {
    AppError::BadRequest("条件写入需要有效且有界的 If-Match/If-None-Match 标签清单".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn conditions(name: header::HeaderName, value: &'static str) -> WriteConditions {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        WriteConditions::parse(&headers).unwrap()
    }

    #[test]
    fn existence_and_strong_or_weak_tag_rules_are_distinct() {
        let current = Some("\"one,two\"");
        assert!(conditions(header::IF_MATCH, "\"previous\", \"one,two\"")
            .check(true, current, None)
            .is_ok());
        assert!(matches!(
            conditions(header::IF_MATCH, "W/\"one,two\"").check(true, current, None),
            Err(AppError::PreconditionFailed)
        ));
        assert!(matches!(
            conditions(header::IF_NONE_MATCH, "W/\"one,two\"").check(true, current, None),
            Err(AppError::PreconditionFailed)
        ));
        assert!(conditions(header::IF_NONE_MATCH, "*")
            .check(false, None, None)
            .is_ok());
        assert!(matches!(
            conditions(header::IF_MATCH, "*").check(false, None, None),
            Err(AppError::PreconditionFailed)
        ));
        assert!(conditions(header::IF_MATCH, "*")
            .check(true, None, None)
            .is_ok());
        assert!(conditions(header::IF_NONE_MATCH, "\"unknown\"")
            .check(true, None, None)
            .is_err());
    }

    #[test]
    fn dates_are_ignored_only_when_invalid_or_overridden_by_if_match() {
        let older = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let newer = older + std::time::Duration::from_secs(1);
        let conditional = conditions(header::IF_UNMODIFIED_SINCE, "Tue, 14 Nov 2023 22:13:20 GMT");
        assert!(conditional.check(true, None, Some(older)).is_ok());
        assert!(matches!(
            conditional.check(true, None, Some(newer)),
            Err(AppError::PreconditionFailed)
        ));
        assert!(matches!(
            conditional.check(true, None, None),
            Err(AppError::PreconditionFailed)
        ));
        assert!(!conditions(header::IF_UNMODIFIED_SINCE, "not a date").is_conditional());
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_MATCH, HeaderValue::from_static("\"current\""));
        headers.insert(
            header::IF_UNMODIFIED_SINCE,
            HeaderValue::from_static("Tue, 14 Nov 2023 22:13:20 GMT"),
        );
        assert!(WriteConditions::parse(&headers)
            .unwrap()
            .check(true, Some("\"current\""), Some(newer))
            .is_ok());
    }
}
