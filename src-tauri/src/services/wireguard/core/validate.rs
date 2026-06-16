use crate::errors::{AppError, AppResult};

use super::session::TunnelSession;

pub fn validate_tunnel_session(session: &TunnelSession) -> AppResult<()> {
    if session.interface_name.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Tunnel session interface name is empty".to_string(),
        ));
    }
    if session.allowed_ips.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Tunnel session allowed IPs are empty".to_string(),
        ));
    }
    if session.endpoint_host.trim().is_empty() || session.endpoint_port == 0 {
        return Err(AppError::InvalidInput(
            "Tunnel session endpoint is incomplete".to_string(),
        ));
    }
    Ok(())
}
