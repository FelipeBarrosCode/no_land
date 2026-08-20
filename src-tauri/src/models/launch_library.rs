use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchLibraryResponse {
    pub instance_id: u64,
    pub launch_pc_available: bool,
    pub items: Vec<LaunchLibraryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchLibraryItem {
    pub app_id: String,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub installed: bool,
    pub in_shared_storage: bool,
    pub latest_bundle_id: Option<String>,
    pub source_labels: Vec<String>,
    pub launchable: bool,
    pub launch_method: String,
    pub restore_required: bool,
    pub artwork_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSoftwareJob {
    pub job_id: String,
    pub instance_id: u64,
    pub app_id: String,
    pub status: String,
    pub restore_performed: bool,
    pub stream_started: bool,
    pub message: String,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SoftwareArtworkResult {
    pub key: String,
    pub image_url: Option<String>,
    pub source: String,
}

pub fn software_artwork_key(name: &str) -> String {
    let mut key = String::new();
    let mut previous_separator = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            key.push(ch);
            previous_separator = false;
        } else if !previous_separator && !key.is_empty() {
            key.push('-');
            previous_separator = true;
        }
    }
    while key.ends_with('-') {
        key.pop();
    }
    if key.is_empty() {
        "software".to_string()
    } else {
        key
    }
}
