use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteApp {
    pub id: u32,
    pub name: String,
    pub hdr_supported: bool,
}
