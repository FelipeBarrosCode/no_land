use crate::moonlight::{
    domain::{MoonlightError, RemoteApp},
    infrastructure::gamestream::xml::{first_text, parse_document, parse_success_status},
};

pub fn parse_app_list_response(xml: &str) -> Result<Vec<RemoteApp>, MoonlightError> {
    let endpoint = "/applist";
    let document = parse_document(xml, endpoint)?;
    parse_success_status(&document, endpoint)?;

    let mut apps = Vec::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name("App"))
    {
        let id = child_text(node, "ID")
            .ok_or_else(|| MoonlightError::Validation("missing App/ID".to_string()))?
            .parse::<u32>()
            .map_err(|error| MoonlightError::Validation(format!("invalid App/ID: {error}")))?;
        let name = child_text(node, "AppTitle")
            .or_else(|| child_text(node, "Name"))
            .ok_or_else(|| MoonlightError::Validation("missing App/AppTitle".to_string()))?;
        let hdr_supported = child_text(node, "IsHdrSupported")
            .map(|value| matches!(value.as_str(), "1" | "true" | "True"))
            .unwrap_or(false);
        apps.push(RemoteApp {
            id,
            name,
            hdr_supported,
        });
    }

    if apps.is_empty() {
        if let Some(single_name) = first_text(&document, "AppTitle") {
            let id = first_text(&document, "ID")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            apps.push(RemoteApp {
                id,
                name: single_name,
                hdr_supported: false,
            });
        }
    }

    Ok(apps)
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, child_name: &str) -> Option<String> {
    node.children()
        .find(|child| child.has_tag_name(child_name))
        .and_then(|child| child.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::parse_app_list_response;

    #[test]
    fn parses_multiple_apps() {
        let xml = r#"<root><status_code>200</status_code><status_message>OK</status_message><App><ID>1</ID><AppTitle>Steam</AppTitle><IsHdrSupported>1</IsHdrSupported></App><App><ID>2</ID><AppTitle>Desktop</AppTitle></App></root>"#;
        let apps = parse_app_list_response(xml).unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Steam");
        assert!(apps[0].hdr_supported);
    }
}
