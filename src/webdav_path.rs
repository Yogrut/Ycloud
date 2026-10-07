use axum::http::Method;

use crate::config::Share;

pub fn parse_share_path(dav_path: &str) -> (&str, &str) {
    let trimmed = dav_path.trim_start_matches('/');
    trimmed.split_once('/').unwrap_or((trimmed, ""))
}

pub fn join_relative(parent: &str, child: &str) -> String {
    match (parent.trim_matches('/'), child.trim_matches('/')) {
        ("", child) => child.to_string(),
        (parent, "") => parent.to_string(),
        (parent, child) => format!("{parent}/{child}"),
    }
}

pub fn display_relative_path(share: &Share, storage_relative: &str) -> String {
    storage_relative
        .strip_prefix(share.path.trim_matches('/'))
        .unwrap_or(storage_relative)
        .trim_matches('/')
        .to_string()
}

pub fn parse_destination(
    destination: &str,
    share_name: &str,
    request_host: Option<&str>,
) -> Option<String> {
    let uri: axum::http::Uri = destination.parse().ok()?;
    if uri.query().is_some() {
        return None;
    }
    if let Some(authority) = uri.authority() {
        let default_port = match uri.scheme_str()? {
            "https" => 443,
            "http" => 80,
            _ => return None,
        };
        let request: axum::http::uri::Authority = request_host?.parse().ok()?;
        let port = |value: &axum::http::uri::Authority| {
            if value.port().is_some() {
                value.port_u16()
            } else {
                Some(default_port)
            }
        };
        if authority.as_str().contains('@')
            || request.as_str().contains('@')
            || !authority.host().eq_ignore_ascii_case(request.host())
            || port(authority).is_none()
            || port(authority) != port(&request)
        {
            return None;
        }
    }
    let path = uri.path();
    let marker = format!("/dav/{}", percent_encode(share_name));
    let remainder = path.strip_prefix(&marker)?;
    if !remainder.is_empty() && !remainder.starts_with('/') {
        return None;
    }
    percent_decode(remainder.trim_start_matches('/'))
}

pub fn is_write_method(method: &Method) -> bool {
    matches!(
        method.as_str(),
        "PUT" | "DELETE" | "MKCOL" | "MOVE" | "COPY"
    )
}

pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? * 16 + hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_destination, percent_decode, percent_encode};

    #[test]
    fn destination_must_remain_in_the_same_share() {
        assert_eq!(
            parse_destination(
                "http://localhost:18473/dav/Team%20Files/docs/runbook.md",
                "Team Files",
                Some("localhost:18473")
            ),
            Some("docs/runbook.md".into())
        );
        assert_eq!(
            parse_destination(
                "http://localhost:18473/dav/Other/file",
                "Team Files",
                Some("localhost:18473")
            ),
            None
        );
        assert_eq!(
            parse_destination(
                "http://evil.example/dav/Team%20Files/file",
                "Team Files",
                Some("localhost:18473")
            ),
            None
        );
        assert_eq!(
            parse_destination(
                "http://localhost:18473/prefix/dav/Team%20Files/file",
                "Team Files",
                Some("localhost:18473")
            ),
            None
        );
    }

    #[test]
    fn percent_encoding_round_trips_utf8() {
        let encoded = percent_encode("文档/计划 2026.md");
        assert_eq!(
            percent_decode(&encoded).as_deref(),
            Some("文档/计划 2026.md")
        );
    }

    #[test]
    fn destination_normalizes_authority_without_allowing_a_different_host_or_port() {
        for (destination, host) in [
            ("https://CLOUD.example:443/dav/local/file", "cloud.example"),
            ("https://cloud.example/dav/local/file", "CLOUD.example:443"),
            ("http://cloud.example:80/dav/local/file", "cloud.example"),
            ("https://[::1]:443/dav/local/file", "[::1]"),
        ] {
            assert_eq!(
                parse_destination(destination, "local", Some(host)),
                Some("file".into())
            );
        }
        for destination in [
            "https://cloud.example:8443/dav/local/file",
            "https://other.example/dav/local/file",
            "https://cloud.example/dav/locality/file",
            "https://cloud.example/dav/local/file?query=ignored",
        ] {
            assert_eq!(
                parse_destination(destination, "local", Some("cloud.example")),
                None
            );
        }
    }
}
