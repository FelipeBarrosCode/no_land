use roxmltree::{Document, Node};

use crate::moonlight::domain::MoonlightError;

pub fn parse_document<'a>(xml: &'a str, endpoint: &str) -> Result<Document<'a>, MoonlightError> {
    Document::parse(xml).map_err(|error| {
        MoonlightError::Validation(format!("failed to parse XML for {endpoint}: {error}"))
    })
}

pub fn first_text(document: &Document<'_>, tag_name: &str) -> Option<String> {
    document
        .descendants()
        .find(|node| node.has_tag_name(tag_name))
        .and_then(node_text)
        .map(ToOwned::to_owned)
}

pub fn node_text<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.text().map(str::trim).filter(|text| !text.is_empty())
}

pub fn required_text(
    document: &Document<'_>,
    endpoint: &str,
    tag_name: &str,
) -> Result<String, MoonlightError> {
    first_text(document, tag_name).ok_or_else(|| {
        MoonlightError::Validation(format!(
            "missing required XML tag `{tag_name}` in {endpoint} response"
        ))
    })
}

pub fn parse_success_status(document: &Document<'_>, endpoint: &str) -> Result<(), MoonlightError> {
    let root = document
        .descendants()
        .find(|node| node.has_tag_name("root"));
    let status_code = root
        .and_then(|node| node.attribute("status_code").map(ToOwned::to_owned))
        .or_else(|| first_text(document, "status_code"))
        .ok_or_else(|| {
            MoonlightError::Validation(format!("missing status code in {endpoint} response"))
        })?;
    let status_message = root
        .and_then(|node| node.attribute("status_message").map(ToOwned::to_owned))
        .or_else(|| first_text(document, "status_message"))
        .unwrap_or_default();
    if status_code != "200" {
        return Err(MoonlightError::Validation(format!(
            "{endpoint} returned status_code={status_code} status_message={status_message}"
        )));
    }
    Ok(())
}

pub fn parse_optional_u16(
    document: &Document<'_>,
    tag_name: &str,
) -> Result<Option<u16>, MoonlightError> {
    match first_text(document, tag_name) {
        Some(value) => value
            .parse::<u16>()
            .map(Some)
            .map_err(|error| MoonlightError::Validation(format!("invalid {tag_name}: {error}"))),
        None => Ok(None),
    }
}

pub fn parse_optional_u32(
    document: &Document<'_>,
    tag_name: &str,
) -> Result<Option<u32>, MoonlightError> {
    match first_text(document, tag_name) {
        Some(value) => value
            .parse::<u32>()
            .map(Some)
            .map_err(|error| MoonlightError::Validation(format!("invalid {tag_name}: {error}"))),
        None => Ok(None),
    }
}
