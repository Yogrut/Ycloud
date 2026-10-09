//! Bounded, namespace-aware PROPFIND request selection.
use std::{borrow::Cow, collections::HashSet};

use axum::http::HeaderValue;

use crate::error::{AppError, AppResult};

const MAX_XML_NODES: u32 = 2048;
const MAX_PROPERTIES: usize = 128;
const MAX_NAMESPACE_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PropertyName {
    pub namespace: String,
    pub local: String,
}

impl PropertyName {
    pub fn dav(local: &str) -> Self {
        Self {
            namespace: "DAV:".into(),
            local: local.into(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum PropfindQuery {
    All(Vec<PropertyName>),
    Names,
    Selected(Vec<PropertyName>),
}

impl PropfindQuery {
    pub fn parse(bytes: &[u8], content_type: Option<&HeaderValue>) -> AppResult<Self> {
        if bytes.is_empty() {
            return Ok(Self::All(Vec::new()));
        }
        let mut encoding_name = "undetermined";
        let result = select_encoding(bytes, content_type).and_then(|(encoding, skip)| {
            encoding_name = encoding.name();
            let text = encoding.decode(&bytes[skip..])?;
            Self::parse_text(&text)
        });
        result.map_err(|reason| {
            // Authenticated control requests only. Never log XML, MIME values,
            // parser error text, or credential/request headers.
            tracing::warn!(
                reason = reason.name(),
                encoding = encoding_name,
                body_bytes = bytes.len(),
                "WebDAV PROPFIND rejected"
            );
            AppError::BadRequest(format!("Invalid PROPFIND request ({})", reason.name()).into())
        })
    }

    fn parse_text(text: &str) -> QueryResult<Self> {
        let document = roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_XML_NODES,
                ..Default::default()
            },
        )
        .map_err(|error| match error {
            roxmltree::Error::DtdDetected => QueryFailure::Dtd,
            roxmltree::Error::NodesLimitReached => QueryFailure::NodeLimit,
            _ => QueryFailure::Xml,
        })?;
        let root = document.root_element();
        if !root.has_tag_name(("DAV:", "propfind")) {
            return Err(QueryFailure::Root);
        }
        require_no_text(root)?;
        let mut selection = None;
        let mut include = None;
        for child in root.children().filter(|node| node.is_element()) {
            if child.tag_name().namespace() != Some("DAV:") {
                continue;
            }
            let selected = match child.tag_name().name() {
                "allprop" => {
                    require_empty(child)?;
                    Self::All(Vec::new())
                }
                "propname" => {
                    require_empty(child)?;
                    Self::Names
                }
                "prop" => Self::Selected(property_names(child)?),
                "include" => {
                    if include.replace(property_names(child)?).is_some() {
                        return Err(QueryFailure::Selector);
                    }
                    continue;
                }
                _ => continue,
            };
            if selection.replace(selected).is_some() {
                return Err(QueryFailure::Selector);
            }
        }
        match (selection, include) {
            (Some(Self::All(_)), include) => Ok(Self::All(include.unwrap_or_default())),
            (Some(selection), None) => Ok(selection),
            _ => Err(QueryFailure::Selector),
        }
    }
}

fn property_names(parent: roxmltree::Node<'_, '_>) -> QueryResult<Vec<PropertyName>> {
    require_no_text(parent)?;
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut count = 0;
    for node in parent.children().filter(|node| node.is_element()) {
        count += 1;
        if count > MAX_PROPERTIES {
            return Err(QueryFailure::PropertyLimit);
        }
        require_empty(node)?;
        let tag = node.tag_name();
        let namespace = tag.namespace().unwrap_or_default();
        if namespace.len() > MAX_NAMESPACE_BYTES || tag.name().len() > MAX_NAME_BYTES {
            return Err(QueryFailure::NameLimit);
        }
        let name = PropertyName {
            namespace: namespace.into(),
            local: tag.name().into(),
        };
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    Ok(names)
}

fn require_empty(node: roxmltree::Node<'_, '_>) -> QueryResult<()> {
    require_no_text(node)?;
    if node.children().any(|child| child.is_element()) {
        return Err(QueryFailure::Property);
    }
    Ok(())
}

fn require_no_text(node: roxmltree::Node<'_, '_>) -> QueryResult<()> {
    if node
        .children()
        .any(|child| child.is_text() && child.text().is_some_and(|text| !text.trim().is_empty()))
    {
        return Err(QueryFailure::Text);
    }
    Ok(())
}

type QueryResult<T> = Result<T, QueryFailure>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryFailure {
    ContentType,
    UnsupportedEncoding,
    MissingBom,
    Utf8,
    Utf16,
    Xml,
    Dtd,
    NodeLimit,
    Root,
    Selector,
    Property,
    PropertyLimit,
    NameLimit,
    Text,
}

impl QueryFailure {
    fn name(self) -> &'static str {
        match self {
            Self::ContentType => "content_type_invalid",
            Self::UnsupportedEncoding => "encoding_unsupported",
            Self::MissingBom => "utf16_bom_required",
            Self::Utf8 => "utf8_invalid",
            Self::Utf16 => "utf16_invalid",
            Self::Xml => "xml_invalid",
            Self::Dtd => "dtd_not_allowed",
            Self::NodeLimit => "xml_node_limit",
            Self::Root => "root_namespace_or_name_invalid",
            Self::Selector => "selector_invalid",
            Self::Property => "property_structure_invalid",
            Self::PropertyLimit => "property_count_limit",
            Self::NameLimit => "property_name_limit",
            Self::Text => "unexpected_text",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum XmlEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl XmlEncoding {
    fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }

    fn decode(self, bytes: &[u8]) -> QueryResult<Cow<'_, str>> {
        if matches!(self, Self::Utf8) {
            return std::str::from_utf8(bytes)
                .map(Cow::Borrowed)
                .map_err(|_| QueryFailure::Utf8);
        }
        if !bytes.len().is_multiple_of(2) {
            return Err(QueryFailure::Utf16);
        }
        let units = bytes.chunks_exact(2).map(|pair| {
            let pair = [pair[0], pair[1]];
            match self {
                Self::Utf16Le => u16::from_le_bytes(pair),
                Self::Utf16Be => u16::from_be_bytes(pair),
                Self::Utf8 => unreachable!(),
            }
        });
        // Input has already passed the 64 KiB control-body limit. UTF-16 to
        // UTF-8 expands by at most 3/2; do not allocate a second code-unit array.
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .map(Cow::Owned)
            .map_err(|_| QueryFailure::Utf16)
    }
}

fn select_encoding(
    bytes: &[u8],
    content_type: Option<&HeaderValue>,
) -> QueryResult<(XmlEncoding, usize)> {
    // RFC 7303: a BOM is authoritative, then the HTTP charset. Recognize
    // unsupported UTF-32 before its LE signature can look like UTF-16.
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        return Err(QueryFailure::UnsupportedEncoding);
    }
    for (bom, encoding) in [
        (&[0xef, 0xbb, 0xbf][..], XmlEncoding::Utf8),
        (&[0xff, 0xfe][..], XmlEncoding::Utf16Le),
        (&[0xfe, 0xff][..], XmlEncoding::Utf16Be),
    ] {
        if bytes.starts_with(bom) {
            return Ok((encoding, bom.len()));
        }
    }
    let Some(content_type) = content_type else {
        return Ok((XmlEncoding::Utf8, 0));
    };
    let mime: mime_guess::Mime = content_type
        .to_str()
        .map_err(|_| QueryFailure::ContentType)?
        .parse()
        .map_err(|_| QueryFailure::ContentType)?;
    let Some(charset) = mime.get_param("charset") else {
        return Ok((XmlEncoding::Utf8, 0));
    };
    let encoding = match charset.as_str().to_ascii_lowercase().as_str() {
        "utf-8" => XmlEncoding::Utf8,
        "utf-16le" => XmlEncoding::Utf16Le,
        "utf-16be" => XmlEncoding::Utf16Be,
        "utf-16" => return Err(QueryFailure::MissingBom),
        _ => return Err(QueryFailure::UnsupportedEncoding),
    };
    Ok((encoding, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_namespaces_selectors_and_deduplicates_requested_names() {
        assert_eq!(
            PropfindQuery::parse(b"", None).unwrap(),
            PropfindQuery::All(vec![])
        );
        assert_eq!(
            PropfindQuery::parse(br#"<propfind xmlns="DAV:"><propname/></propfind>"#, None)
                .unwrap(),
            PropfindQuery::Names
        );
        assert_eq!(PropfindQuery::parse(br#"<d:propfind xmlns:d="DAV:" xmlns:x="urn:client"><d:prop><d:displayname/><d:displayname/><x:label/></d:prop></d:propfind>"#, None).unwrap(),
            PropfindQuery::Selected(vec![PropertyName::dav("displayname"), PropertyName { namespace: "urn:client".into(), local: "label".into() }]));
        assert_eq!(PropfindQuery::parse(br#"<d:propfind xmlns:d="DAV:"><d:allprop/><d:include><d:creationdate/></d:include></d:propfind>"#, None).unwrap(),
            PropfindQuery::All(vec![PropertyName::dav("creationdate")]));
    }

    #[test]
    fn rejects_ambiguous_selectors_and_property_lists_over_the_bound() {
        for body in [
            r#"<propfind xmlns="DAV:"><prop/><prop/></propfind>"#,
            r#"<propfind xmlns="DAV:"><prop><displayname/></prop><prop><resourcetype/></prop></propfind>"#,
            r#"<d:propfind xmlns:d="DAV:" xmlns:p="DAV:"><d:prop/><p:prop/></d:propfind>"#,
            r#"<propfind xmlns="DAV:"><prop/><allprop/></propfind>"#,
            r#"<propfind xmlns="DAV:"><allprop/><prop/></propfind>"#,
            r#"<propfind xmlns="DAV:"><prop/><propname/></propfind>"#,
            r#"<propfind xmlns="DAV:"><propname/><prop/></propfind>"#,
            r#"<propfind xmlns="DAV:"><prop/><include/></propfind>"#,
            r#"<propfind xmlns="DAV:"><include/><prop/></propfind>"#,
            r#"<propfind xmlns="DAV:"><allprop/><allprop/></propfind>"#,
            r#"<propfind xmlns="DAV:"><propname/><propname/></propfind>"#,
            r#"<propfind xmlns="DAV:"><allprop/><include/><include/></propfind>"#,
            r#"<propfind xmlns="DAV:"><allprop/><propname/></propfind>"#,
            r#"<propfind xmlns="DAV:"><include><displayname/></include></propfind>"#,
            r#"<propfind xmlns="urn:other"><allprop/></propfind>"#,
        ] {
            assert!(PropfindQuery::parse(body.as_bytes(), None).is_err());
        }
        let body = format!(
            r#"<propfind xmlns="DAV:"><prop>{}</prop></propfind>"#,
            "<displayname/>".repeat(MAX_PROPERTIES + 1)
        );
        assert!(PropfindQuery::parse(body.as_bytes(), None).is_err());
    }

    fn utf16(text: &str, little_endian: bool, bom: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if bom {
            bytes.extend_from_slice(if little_endian {
                &[0xff, 0xfe]
            } else {
                &[0xfe, 0xff]
            });
        }
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        bytes
    }

    #[test]
    fn utf16_boms_preserve_unicode_properties_and_override_http_charset() {
        let text = r#"<?xml version="1.0" encoding="UTF-16"?><propfind xmlns="DAV:" xmlns:x="urn:照片📷"><prop><displayname/><x:标题/></prop></propfind>"#;
        for little_endian in [true, false] {
            let bytes = utf16(text, little_endian, true);
            for content_type in [
                None,
                Some(HeaderValue::from_static("application/xml; charset=UTF-8")),
                Some(HeaderValue::from_static("text/xml; charset=UTF-16")),
            ] {
                assert_eq!(
                    PropfindQuery::parse(&bytes, content_type.as_ref()).unwrap(),
                    PropfindQuery::Selected(vec![
                        PropertyName::dav("displayname"),
                        PropertyName {
                            namespace: "urn:照片📷".into(),
                            local: "标题".into()
                        }
                    ])
                );
            }
        }
    }

    #[test]
    fn explicit_utf16_endianness_accepts_bomless_xml() {
        for (little_endian, charset) in [(true, "UTF-16LE"), (false, "UTF-16BE")] {
            let text = format!(
                r#"<?xml version="1.0" encoding="{charset}"?><propfind xmlns="DAV:"><propname/></propfind>"#
            );
            let content_type =
                HeaderValue::from_str(&format!(r#"application/xml; charset="{charset}""#)).unwrap();
            assert_eq!(
                PropfindQuery::parse(&utf16(&text, little_endian, false), Some(&content_type))
                    .unwrap(),
                PropfindQuery::Names
            );
        }
    }

    #[test]
    fn utf8_bom_and_legacy_empty_bodies_keep_existing_behavior() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(br#"<propfind xmlns="DAV:"><allprop/></propfind>"#);
        let conflicting = HeaderValue::from_static("application/xml; charset=UTF-16");
        assert_eq!(
            PropfindQuery::parse(&bytes, Some(&conflicting)).unwrap(),
            PropfindQuery::All(vec![])
        );
        let unsupported = HeaderValue::from_static("application/xml; charset=iso-8859-1");
        assert_eq!(
            PropfindQuery::parse(b"", Some(&unsupported)).unwrap(),
            PropfindQuery::All(vec![])
        );
    }

    #[test]
    fn decoding_rejects_incomplete_units_without_lossy_replacement() {
        for bytes in [vec![0xff, 0xfe, 0x3c], vec![0xff, 0xfe, 0x00, 0xd8]] {
            let error = PropfindQuery::parse(&bytes, None).unwrap_err();
            assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
            assert!(error.to_string().contains("utf16_invalid"));
        }
        let missing_bom = HeaderValue::from_static("application/xml; charset=utf-16");
        assert!(PropfindQuery::parse(b"<propfind/>", Some(&missing_bom))
            .unwrap_err()
            .to_string()
            .contains("utf16_bom_required"));
        let unsupported = HeaderValue::from_static("application/xml; charset=iso-8859-1");
        assert!(PropfindQuery::parse(b"<propfind/>", Some(&unsupported))
            .unwrap_err()
            .to_string()
            .contains("encoding_unsupported"));
    }

    #[test]
    fn utf16_selection_still_enforces_the_original_property_budget() {
        let text = format!(
            r#"<propfind xmlns="DAV:"><prop>{}</prop></propfind>"#,
            "<displayname/>".repeat(MAX_PROPERTIES + 1)
        );
        let error = PropfindQuery::parse(&utf16(&text, true, true), None).unwrap_err();
        assert!(error.to_string().contains("property_count_limit"));
    }

    #[test]
    fn rejected_query_logs_only_fixed_metadata_not_client_content() {
        #[derive(Clone, Default)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let body = br#"<propfind xmlns="urn:private-body-marker"><allprop/></propfind>"#;
            let error = PropfindQuery::parse(body, None).unwrap_err();
            assert!(!error.to_string().contains("private-body-marker"));
            let content_type =
                HeaderValue::from_static("application/xml; charset=private-header-marker");
            assert!(PropfindQuery::parse(body, Some(&content_type)).is_err());
        });
        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("root_namespace_or_name_invalid"));
        assert!(logs.contains("encoding_unsupported"));
        assert!(logs.contains("body_bytes="));
        assert!(!logs.contains("private-body-marker"));
        assert!(!logs.contains("private-header-marker"));
        assert!(!logs.contains("<propfind"));
    }
}
