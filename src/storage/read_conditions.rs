//! Shared file validators and byte-range selection for local and S3 reads.
use crate::error::{AppError, AppResult};
use axum::http::{header, response::Builder, HeaderMap, HeaderValue, Method, StatusCode};

#[derive(Default)]
pub(crate) struct FileValidators {
    etag: Option<HeaderValue>,
    last_modified: Option<HeaderValue>,
}

impl FileValidators {
    pub(crate) fn local(metadata: &std::fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|time| i64::try_from(time.as_secs()).ok());
        // A timestamp and size are not a strong content identity. Do not hash
        // the entire file for each download or advertise a guessed strong ETag.
        Self::object(None, modified)
    }

    pub(crate) fn object(etag: Option<&str>, modified: Option<i64>) -> Self {
        Self {
            etag: etag
                .filter(|value| is_entity_tag(value))
                .and_then(|value| HeaderValue::from_str(value).ok()),
            last_modified: modified
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .and_then(|time| {
                    HeaderValue::from_str(&time.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
                        .ok()
                }),
        }
    }

    pub(crate) fn apply(&self, mut response: Builder) -> Builder {
        if let Some(etag) = &self.etag {
            response = response.header(header::ETAG, etag.clone());
        }
        if let Some(modified) = &self.last_modified {
            response = response.header(header::LAST_MODIFIED, modified.clone());
        }
        response
    }

    /// Evaluate request preconditions before selecting a range or reading bytes.
    /// The opened resource exists; local files still have no invented ETag.
    pub(crate) fn precondition_response(
        &self,
        headers: &HeaderMap,
        method: &Method,
    ) -> AppResult<Option<axum::response::Response>> {
        let etag = self.etag.as_ref().and_then(|value| value.to_str().ok());
        let modified = self
            .last_modified
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| httpdate::parse_http_date(value).ok());
        let if_match = super::write_conditions::tag_condition(headers, header::IF_MATCH)?;
        let if_none = super::write_conditions::tag_condition(headers, header::IF_NONE_MATCH)?;
        let status = if if_match
            .as_ref()
            .is_some_and(|condition| !condition.matches(true, etag, false))
            || if_match.is_none()
                && modified
                    .zip(header_date(headers, header::IF_UNMODIFIED_SINCE))
                    .is_some_and(|(actual, expected)| actual > expected)
        {
            Some(StatusCode::PRECONDITION_FAILED)
        } else if if_none
            .as_ref()
            .is_some_and(|condition| condition.matches(true, etag, true))
        {
            Some(if matches!(*method, Method::GET | Method::HEAD) {
                StatusCode::NOT_MODIFIED
            } else {
                StatusCode::PRECONDITION_FAILED
            })
        } else if if_none.is_none()
            && matches!(*method, Method::GET | Method::HEAD)
            && modified
                .zip(header_date(headers, header::IF_MODIFIED_SINCE))
                .is_some_and(|(actual, expected)| actual <= expected)
        {
            Some(StatusCode::NOT_MODIFIED)
        } else {
            None
        };
        status
            .map(|status| {
                self.apply(Builder::new().status(status))
                    .body(axum::body::Body::empty())
                    .map_err(|error| {
                        AppError::with_source("failed to build file precondition response", error)
                    })
            })
            .transpose()
    }

    pub(crate) fn select_range(
        &self,
        headers: &HeaderMap,
        length: u64,
        method: &Method,
    ) -> Result<Option<(u64, u64, StatusCode)>, ()> {
        if method != Method::GET {
            return Ok(None);
        }
        if headers.contains_key(header::IF_RANGE) && !self.matches_if_range(headers) {
            // Date validators cannot prove strong equality here. A missing,
            // weak, invalid or different version requires the complete file.
            return Ok(None);
        }
        parse_range(headers, length)
    }

    fn matches_if_range(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(header::IF_RANGE).iter();
        let expected = values
            .next()
            .and_then(|value| value.to_str().ok())
            .map(str::trim);
        if values.next().is_some() {
            return false;
        }
        let actual = self.etag.as_ref().and_then(|value| value.to_str().ok());
        match (expected, actual) {
            (Some(expected), Some(actual)) => {
                !expected.starts_with("W/")
                    && !actual.starts_with("W/")
                    && is_entity_tag(expected)
                    && expected == actual
            }
            _ => false,
        }
    }
}

fn header_date(headers: &HeaderMap, name: header::HeaderName) -> Option<std::time::SystemTime> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    httpdate::parse_http_date(value.to_str().ok()?).ok()
}

pub(super) fn is_entity_tag(value: &str) -> bool {
    let value = value.strip_prefix("W/").unwrap_or(value).as_bytes();
    value.len() >= 2
        && value[0] == b'"'
        && value[value.len() - 1] == b'"'
        && value[1..value.len() - 1]
            .iter()
            .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte) || *byte >= 0x80)
}

pub(crate) fn parse_range(
    headers: &HeaderMap,
    total_length: u64,
) -> Result<Option<(u64, u64, StatusCode)>, ()> {
    let Some(header_value) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    let raw = header_value.to_str().map_err(|_| ())?;
    let value = raw.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') || total_length == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let suffix = suffix.min(total_length);
        (total_length.saturating_sub(suffix), total_length - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= total_length {
            return Err(());
        }
        let end = if end.is_empty() {
            total_length - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total_length - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(Some((start, end - start + 1, StatusCode::PARTIAL_CONTENT)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_preconditions_follow_http_priority_without_fabricating_local_etags() {
        let validators = FileValidators::object(Some("\"current\""), Some(1_700_000_000));
        for method in [Method::GET, Method::HEAD] {
            for (name, value, expected) in [
                (
                    header::IF_MATCH,
                    "\"old\"",
                    Some(StatusCode::PRECONDITION_FAILED),
                ),
                (header::IF_MATCH, "*", None),
                (
                    header::IF_MATCH,
                    "W/\"current\"",
                    Some(StatusCode::PRECONDITION_FAILED),
                ),
                (
                    header::IF_NONE_MATCH,
                    "W/\"current\"",
                    Some(StatusCode::NOT_MODIFIED),
                ),
                (header::IF_NONE_MATCH, "*", Some(StatusCode::NOT_MODIFIED)),
                (
                    header::IF_MODIFIED_SINCE,
                    "Tue, 14 Nov 2023 22:13:20 GMT",
                    Some(StatusCode::NOT_MODIFIED),
                ),
                (
                    header::IF_UNMODIFIED_SINCE,
                    "Tue, 14 Nov 2023 22:13:19 GMT",
                    Some(StatusCode::PRECONDITION_FAILED),
                ),
            ] {
                let mut headers = HeaderMap::new();
                headers.insert(name, HeaderValue::from_static(value));
                headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1"));
                let response = validators.precondition_response(&headers, &method).unwrap();
                assert_eq!(
                    response.as_ref().map(|response| response.status()),
                    expected
                );
                if let Some(response) = response {
                    assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
                }
            }
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_MATCH, HeaderValue::from_static("\"current\""));
        headers.insert(
            header::IF_UNMODIFIED_SINCE,
            HeaderValue::from_static("Tue, 14 Nov 2023 22:13:19 GMT"),
        );
        assert!(validators
            .precondition_response(&headers, &Method::GET)
            .unwrap()
            .is_none());
        headers.clear();
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("\"old\""));
        headers.insert(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_static("Tue, 14 Nov 2023 22:13:20 GMT"),
        );
        assert!(validators
            .precondition_response(&headers, &Method::GET)
            .unwrap()
            .is_none());
        let local = FileValidators::object(None, Some(1_700_000_000));
        headers.clear();
        headers.insert(header::IF_MATCH, HeaderValue::from_static("\"current\""));
        assert_eq!(
            local
                .precondition_response(&headers, &Method::GET)
                .unwrap()
                .unwrap()
                .status(),
            StatusCode::PRECONDITION_FAILED
        );
        headers.insert(header::IF_MATCH, HeaderValue::from_static("*"));
        assert!(local
            .precondition_response(&headers, &Method::GET)
            .unwrap()
            .is_none());
    }

    #[test]
    fn conditional_ranges_require_a_current_strong_object_version() {
        let current = FileValidators::object(Some("\"current\""), Some(1_700_000_000));
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-4"));
        assert_eq!(
            current.select_range(&headers, 10, &Method::GET).unwrap(),
            Some((2, 3, StatusCode::PARTIAL_CONTENT))
        );
        headers.insert(header::IF_RANGE, HeaderValue::from_static("\"current\""));
        assert_eq!(
            current.select_range(&headers, 10, &Method::GET).unwrap(),
            Some((2, 3, StatusCode::PARTIAL_CONTENT))
        );
        for expected in [
            "\"previous\"",
            "W/\"current\"",
            "Tue, 14 Nov 2023 22:13:20 GMT",
            "invalid",
        ] {
            headers.insert(header::IF_RANGE, HeaderValue::from_str(expected).unwrap());
            assert_eq!(
                current.select_range(&headers, 10, &Method::GET).unwrap(),
                None
            );
        }
        headers.insert(header::IF_RANGE, HeaderValue::from_static("\"current\""));
        assert_eq!(
            FileValidators::object(None, Some(1_700_000_000))
                .select_range(&headers, 10, &Method::GET)
                .unwrap(),
            None
        );
        assert_eq!(
            FileValidators::object(Some("W/\"current\""), None)
                .select_range(&headers, 10, &Method::GET)
                .unwrap(),
            None
        );
        headers.append(header::IF_RANGE, HeaderValue::from_static("\"current\""));
        assert_eq!(
            current.select_range(&headers, 10, &Method::GET).unwrap(),
            None
        );
        assert_eq!(
            current.select_range(&headers, 10, &Method::HEAD).unwrap(),
            None
        );
    }

    #[test]
    fn validator_headers_are_valid_and_untrusted_invalid_tags_are_omitted() {
        let response = FileValidators::object(Some("\"current\""), Some(1_700_000_000))
            .apply(Builder::new())
            .body(())
            .unwrap();
        assert_eq!(response.headers()[header::ETAG], "\"current\"");
        assert_eq!(
            response.headers()[header::LAST_MODIFIED],
            "Tue, 14 Nov 2023 22:13:20 GMT"
        );
        for tag in ["unquoted", "\"bad\"tag\"", "\"control\n\"", "W/invalid"] {
            let response = FileValidators::object(Some(tag), None)
                .apply(Builder::new())
                .body(())
                .unwrap();
            assert!(!response.headers().contains_key(header::ETAG));
        }
    }
}
