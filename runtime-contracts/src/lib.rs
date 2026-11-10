//! Session identity contract shared by the parent display service and companion.
pub const PROFILE: &str = "ubuntu24-x86_64";
pub const DISPLAY: &str = ":0";
pub const XAUTHORITY: &str = "/etc/X11/.Xauthority-noland";
pub const XSOCKET: &str = "/tmp/.X11-unix/X0";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDescriptor {
    pub display: String,
    pub authority_source: String,
    pub user: String,
    pub uid: u32,
    pub gid: u32,
    pub home: std::path::PathBuf,
    pub pulse_socket: Option<std::path::PathBuf>,
    pub driver_identity: Option<String>,
}
