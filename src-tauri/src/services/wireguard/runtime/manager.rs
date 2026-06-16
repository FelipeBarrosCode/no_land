use crate::errors::AppResult;

use super::super::core::session::{TunnelDriverKind, TunnelSession};
use super::super::platform::linux::LinuxNativeDriver;
use super::super::platform::macos::MacosNativeDriver;
use super::super::platform::manual::ManualAppFallbackDriver;

#[derive(Debug, Clone)]
pub struct TunnelManager {
    manual_driver: ManualAppFallbackDriver,
    linux_driver: LinuxNativeDriver,
    macos_driver: MacosNativeDriver,
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self {
            manual_driver: ManualAppFallbackDriver,
            linux_driver: LinuxNativeDriver,
            macos_driver: MacosNativeDriver,
        }
    }
}

impl TunnelManager {
    pub fn select_driver(&self, _session: &TunnelSession) -> TunnelDriverKind {
        if cfg!(target_os = "linux") {
            TunnelDriverKind::LinuxNative
        } else if cfg!(target_os = "macos") {
            TunnelDriverKind::MacosNative
        } else {
            self.manual_driver.kind()
        }
    }

    pub fn prepare(&self, session: &TunnelSession) -> AppResult<TunnelDriverKind> {
        match self.select_driver(session) {
            TunnelDriverKind::LinuxNative => Ok(TunnelDriverKind::LinuxNative),
            TunnelDriverKind::MacosNative => Ok(TunnelDriverKind::MacosNative),
            _ => {
                self.manual_driver.prepare(session)?;
                Ok(self.manual_driver.kind())
            }
        }
    }

    pub fn start(&self, session: &TunnelSession) -> AppResult<TunnelDriverKind> {
        match self.select_driver(session) {
            TunnelDriverKind::LinuxNative => {
                self.linux_driver.start(session)?;
                Ok(TunnelDriverKind::LinuxNative)
            }
            TunnelDriverKind::MacosNative => {
                self.macos_driver.start(session)?;
                Ok(TunnelDriverKind::MacosNative)
            }
            _ => {
                self.manual_driver.start(session)?;
                Ok(self.manual_driver.kind())
            }
        }
    }
}
