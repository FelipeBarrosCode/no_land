use serde::{Deserialize, Serialize};

use super::xml::{
    first_text, parse_document, parse_optional_u16, parse_optional_u32, parse_success_status,
    required_text,
};
use crate::moonlight::domain::MoonlightError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairStatus {
    Paired,
    Unpaired,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_rate: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub app_version: String,
    pub gfe_version: Option<String>,
    pub https_port: Option<u16>,
    pub pair_status: PairStatus,
    pub current_game_id: Option<u32>,
    pub server_codec_mode_support: u32,
    pub state: Option<String>,
    pub display_modes: Vec<DisplayMode>,
}

pub fn parse_server_info_response(xml: &str) -> Result<ServerInfo, MoonlightError> {
    let endpoint = "/serverinfo";
    let document = parse_document(xml, endpoint)?;
    parse_success_status(&document, endpoint)?;

    let app_version = required_text(&document, endpoint, "appversion")?;
    let gfe_version = first_text(&document, "GfeVersion");
    let https_port = parse_optional_u16(&document, "HttpsPort")?;
    let pair_status = parse_pair_status(first_text(&document, "PairStatus"));
    let current_game_id = parse_optional_u32(&document, "currentgame")?;
    let server_codec_mode_support =
        parse_optional_u32(&document, "ServerCodecModeSupport")?.unwrap_or(0);
    let state = first_text(&document, "state");
    let display_modes = parse_display_modes(&document)?;

    Ok(ServerInfo {
        app_version,
        gfe_version,
        https_port,
        pair_status,
        current_game_id,
        server_codec_mode_support,
        state,
        display_modes,
    })
}

fn parse_pair_status(raw: Option<String>) -> PairStatus {
    match raw.as_deref() {
        Some("1") | Some("true") | Some("paired") | Some("Paired") => PairStatus::Paired,
        Some("0") | Some("false") | Some("unpaired") | Some("Unpaired") => PairStatus::Unpaired,
        _ => PairStatus::Unknown,
    }
}

fn parse_display_modes(
    document: &roxmltree::Document<'_>,
) -> Result<Vec<DisplayMode>, MoonlightError> {
    let mut modes = Vec::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name("DisplayMode"))
    {
        let width = child_text(node, "Width").and_then(|value| value.parse::<u32>().ok());
        let height = child_text(node, "Height").and_then(|value| value.parse::<u32>().ok());
        let refresh_rate =
            child_text(node, "RefreshRate").and_then(|value| value.parse::<u32>().ok());
        if let (Some(width), Some(height)) = (width, height) {
            modes.push(DisplayMode {
                width,
                height,
                refresh_rate,
            });
        }
    }
    Ok(modes)
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
    use super::{parse_server_info_response, PairStatus};

    #[test]
    fn parses_server_info_response() {
        let xml = r#"
        <root>
            <status_code>200</status_code>
            <status_message>OK</status_message>
            <appversion>7.1.431.-1</appversion>
            <GfeVersion></GfeVersion>
            <HttpsPort>47984</HttpsPort>
            <PairStatus>1</PairStatus>
            <currentgame>0</currentgame>
            <ServerCodecModeSupport>197889</ServerCodecModeSupport>
            <state>MJOLNIR_SERVER_AVAILABLE</state>
            <DisplayMode><Width>1920</Width><Height>1080</Height><RefreshRate>60</RefreshRate></DisplayMode>
        </root>
        "#;

        let parsed = parse_server_info_response(xml).unwrap();
        assert_eq!(parsed.app_version, "7.1.431.-1");
        assert_eq!(parsed.https_port, Some(47984));
        assert_eq!(parsed.pair_status, PairStatus::Paired);
        assert_eq!(parsed.display_modes.len(), 1);
    }

    #[test]
    fn rejects_unsuccessful_status() {
        let xml = r#"<root><status_code>503</status_code><status_message>down</status_message><appversion>x</appversion></root>"#;
        assert!(parse_server_info_response(xml).is_err());
    }
}
