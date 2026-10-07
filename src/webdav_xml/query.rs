//! Bounded, namespace-aware PROPFIND request selection.
use std::collections::HashSet;

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
    pub fn parse(bytes: &[u8]) -> AppResult<Self> {
        if bytes.is_empty() {
            return Ok(Self::All(Vec::new()));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid_query())?;
        let document = roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_XML_NODES,
                ..Default::default()
            },
        )
        .map_err(|_| invalid_query())?;
        let root = document.root_element();
        if !root.has_tag_name(("DAV:", "propfind")) {
            return Err(invalid_query());
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
                        return Err(invalid_query());
                    }
                    continue;
                }
                _ => continue,
            };
            if selection.replace(selected).is_some() {
                return Err(invalid_query());
            }
        }
        match (selection, include) {
            (Some(Self::All(_)), include) => Ok(Self::All(include.unwrap_or_default())),
            (Some(selection), None) => Ok(selection),
            _ => Err(invalid_query()),
        }
    }
}

fn property_names(parent: roxmltree::Node<'_, '_>) -> AppResult<Vec<PropertyName>> {
    require_no_text(parent)?;
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut count = 0;
    for node in parent.children().filter(|node| node.is_element()) {
        count += 1;
        if count > MAX_PROPERTIES {
            return Err(invalid_query());
        }
        require_empty(node)?;
        let tag = node.tag_name();
        let namespace = tag.namespace().unwrap_or_default();
        if namespace.len() > MAX_NAMESPACE_BYTES || tag.name().len() > MAX_NAME_BYTES {
            return Err(invalid_query());
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

fn require_empty(node: roxmltree::Node<'_, '_>) -> AppResult<()> {
    require_no_text(node)?;
    if node.children().any(|child| child.is_element()) {
        return Err(invalid_query());
    }
    Ok(())
}

fn invalid_query() -> AppError {
    AppError::BadRequest("PROPFIND 需要有效的 UTF-8 XML、单个属性选择器及有界属性清单".into())
}

fn require_no_text(node: roxmltree::Node<'_, '_>) -> AppResult<()> {
    if node
        .children()
        .any(|child| child.is_text() && child.text().is_some_and(|text| !text.trim().is_empty()))
    {
        return Err(invalid_query());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_namespaces_selectors_and_deduplicates_requested_names() {
        assert_eq!(
            PropfindQuery::parse(b"").unwrap(),
            PropfindQuery::All(vec![])
        );
        assert_eq!(
            PropfindQuery::parse(br#"<propfind xmlns="DAV:"><propname/></propfind>"#).unwrap(),
            PropfindQuery::Names
        );
        assert_eq!(PropfindQuery::parse(br#"<d:propfind xmlns:d="DAV:" xmlns:x="urn:client"><d:prop><d:displayname/><d:displayname/><x:label/></d:prop></d:propfind>"#).unwrap(),
            PropfindQuery::Selected(vec![PropertyName::dav("displayname"), PropertyName { namespace: "urn:client".into(), local: "label".into() }]));
        assert_eq!(PropfindQuery::parse(br#"<d:propfind xmlns:d="DAV:"><d:allprop/><d:include><d:creationdate/></d:include></d:propfind>"#).unwrap(),
            PropfindQuery::All(vec![PropertyName::dav("creationdate")]));
    }

    #[test]
    fn rejects_ambiguous_selectors_and_property_lists_over_the_bound() {
        for body in [
            r#"<propfind xmlns="DAV:"><allprop/><propname/></propfind>"#,
            r#"<propfind xmlns="DAV:"><include><displayname/></include></propfind>"#,
            r#"<propfind xmlns="urn:other"><allprop/></propfind>"#,
        ] {
            assert!(PropfindQuery::parse(body.as_bytes()).is_err());
        }
        let body = format!(
            r#"<propfind xmlns="DAV:"><prop>{}</prop></propfind>"#,
            "<displayname/>".repeat(MAX_PROPERTIES + 1)
        );
        assert!(PropfindQuery::parse(body.as_bytes()).is_err());
    }
}
