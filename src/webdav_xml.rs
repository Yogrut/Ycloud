use crate::error::{AppError, AppResult};

mod query;
pub(crate) use query::{PropertyName, PropfindQuery};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const LIVE_PROPERTIES: [&str; 5] = [
    "displayname",
    "getcontentlength",
    "getlastmodified",
    "getcontenttype",
    "resourcetype",
];

pub struct PropfindResponseEntry {
    pub href: String,
    pub displayname: String,
    pub is_dir: bool,
    pub content_length: u64,
    pub last_modified: String,
    pub content_type: String,
}

// ── XML helpers ───────────────────────────────────────────────────

/// Escape text for safe inclusion in XML 1.0. Invalid control and noncharacter
/// code points are replaced instead of emitting a malformed WebDAV response.
fn xml_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for character in s.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            character if is_xml_10_character(character) => escaped.push(character),
            _ => escaped.push('\u{fffd}'),
        }
    }
    escaped
}

fn is_xml_10_character(character: char) -> bool {
    matches!(character, '\u{0009}' | '\u{000a}' | '\u{000d}')
        || matches!(character as u32, 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}

pub(crate) fn build_multistatus(
    responses: &[PropfindResponseEntry],
    base_url: &str,
    query: &PropfindQuery,
) -> AppResult<String> {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
"#,
    );
    let mut requested = match query {
        PropfindQuery::Selected(names) => names.clone(),
        _ => LIVE_PROPERTIES
            .iter()
            .map(|name| PropertyName::dav(name))
            .collect(),
    };
    if let PropfindQuery::All(include) = query {
        for name in include {
            if !requested.contains(name) {
                requested.push(name.clone());
            }
        }
    }
    for entry in responses {
        let mut found = String::new();
        let mut missing = String::new();
        for name in &requested {
            if let Some(value) = property_value(entry, name) {
                found.push_str(&property_xml(
                    name,
                    (!matches!(query, PropfindQuery::Names)).then_some(value.as_str()),
                ));
            } else {
                missing.push_str(&property_xml(name, None));
            }
        }
        let mut item = format!(
            "<D:response><D:href>{}{}</D:href>",
            xml_escape(base_url),
            xml_escape(&entry.href)
        );
        if !found.is_empty() || requested.is_empty() {
            item.push_str(&propstat(&found, "200 OK"));
        }
        if !missing.is_empty() {
            item.push_str(&propstat(&missing, "404 Not Found"));
        }
        item.push_str("</D:response>");
        if item.len() > MAX_RESPONSE_BYTES.saturating_sub(xml.len() + "</D:multistatus>".len()) {
            return Err(AppError::ServiceUnavailable(
                "PROPFIND 响应超过 8 MiB 安全上限，请查询更小目录或更少属性".into(),
            ));
        }
        xml.push_str(&item);
    }
    xml.push_str("</D:multistatus>");
    Ok(xml)
}

fn property_value(entry: &PropfindResponseEntry, name: &PropertyName) -> Option<String> {
    if name.namespace != "DAV:" {
        return None;
    }
    Some(match name.local.as_str() {
        "displayname" => xml_escape(&entry.displayname),
        "getcontentlength" => entry.content_length.to_string(),
        "getlastmodified" => xml_escape(&entry.last_modified),
        "getcontenttype" => xml_escape(&entry.content_type),
        "resourcetype" => {
            if entry.is_dir {
                "<D:collection/>".into()
            } else {
                String::new()
            }
        }
        _ => return None,
    })
}

fn property_xml(name: &PropertyName, value: Option<&str>) -> String {
    let (qualified, namespace) = if name.namespace == "DAV:" {
        (format!("D:{}", name.local), String::new())
    } else if name.namespace.is_empty() {
        (name.local.clone(), " xmlns=\"\"".into())
    } else {
        (
            format!("P:{}", name.local),
            format!(" xmlns:P=\"{}\"", xml_escape(&name.namespace)),
        )
    };
    match value {
        Some(value) => format!("<{qualified}{namespace}>{value}</{qualified}>"),
        None => format!("<{qualified}{namespace}/>"),
    }
}

fn propstat(properties: &str, status: &str) -> String {
    format!("<D:propstat><D:prop>{properties}</D:prop><D:status>HTTP/1.1 {status}</D:status></D:propstat>")
}

pub fn to_rfc1123(dt: &std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = (*dt).into();
    datetime.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[cfg(test)]
mod tests {
    use super::xml_escape;

    #[test]
    fn xml_escape_encodes_entities_and_replaces_invalid_controls() {
        assert_eq!(
            xml_escape("a<&\"'\u{0001}\t"),
            "a&lt;&amp;&quot;&apos;\u{fffd}\t"
        );
    }
}
