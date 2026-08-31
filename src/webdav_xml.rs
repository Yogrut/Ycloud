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

pub fn build_multistatus(responses: &[PropfindResponseEntry], base_url: &str) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
"#,
    );
    for entry in responses {
        let coll = if entry.is_dir { "<D:collection/>" } else { "" };
        xml.push_str(&format!(
            r#"<D:response>
  <D:href>{}{}</D:href>
  <D:propstat>
    <D:prop>
      <D:displayname>{}</D:displayname>
      <D:getcontentlength>{}</D:getcontentlength>
      <D:getlastmodified>{}</D:getlastmodified>
      <D:getcontenttype>{}</D:getcontenttype>
      <D:resourcetype>{}</D:resourcetype>
    </D:prop>
    <D:status>HTTP/1.1 200 OK</D:status>
  </D:propstat>
</D:response>"#,
            xml_escape(base_url),
            xml_escape(&entry.href),
            xml_escape(&entry.displayname),
            entry.content_length,
            xml_escape(&entry.last_modified),
            xml_escape(&entry.content_type),
            coll,
        ));
    }
    xml.push_str("</D:multistatus>");
    xml
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
