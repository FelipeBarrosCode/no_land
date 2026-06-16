use crate::errors::AppResult;

use super::super::core::session::{TunnelDriverKind, TunnelSession};

#[derive(Debug, Default, Clone, Copy)]
pub struct ManualAppFallbackDriver;

impl ManualAppFallbackDriver {
    pub fn kind(&self) -> TunnelDriverKind {
        TunnelDriverKind::ManualApp
    }

    pub fn prepare(&self, _session: &TunnelSession) -> AppResult<()> {
        Ok(())
    }

    pub fn start(&self, _session: &TunnelSession) -> AppResult<()> {
        Ok(())
    }

    pub fn stop(&self, _session: &TunnelSession) -> AppResult<()> {
        Ok(())
    }
}
