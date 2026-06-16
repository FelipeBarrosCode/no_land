use std::path::Path;

use crate::errors::AppResult;

use super::super::core::{config::parse_tunnel_session_from_file, session::TunnelSession};

pub fn build_tunnel_session_from_generated_config(
    config_path: &Path,
    instance_id: Option<u64>,
    sunshine_host: &str,
    sunshine_port: u16,
) -> AppResult<TunnelSession> {
    parse_tunnel_session_from_file(
        config_path,
        instance_id,
        "wg0client",
        sunshine_host,
        sunshine_port,
    )
}
