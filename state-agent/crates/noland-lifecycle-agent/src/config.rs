use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AgentError, Result};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/noland/lifecycle/config.json";
pub const EVALUATION_TICK_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderAction {
    Destroy,
    Stop,
}

impl ProviderAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Destroy => "destroy",
            Self::Stop => "stop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub instance_id: u64,
    pub inactivity_seconds: u64,
    pub backup_app_limit: usize,
    pub controller_dead_zone: f32,
    pub state_agent_socket: PathBuf,
    pub status_socket: PathBuf,
    pub activity_socket: PathBuf,
    pub database_path: PathBuf,
    pub capability_path: PathBuf,
    pub vast_base_url: String,
    pub provider_action: ProviderAction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            instance_id: 0,
            inactivity_seconds: 3 * 60 * 60,
            backup_app_limit: 3,
            controller_dead_zone: 0.15,
            state_agent_socket: "/run/noland/state-agent.sock".into(),
            status_socket: "/run/noland/lifecycle/agent.sock".into(),
            activity_socket: "/run/noland/sunshine-events.sock".into(),
            database_path: "/var/lib/noland/lifecycle/runtime.db".into(),
            capability_path: "/var/lib/noland/lifecycle/storage-capability.json".into(),
            vast_base_url: "https://console.vast.ai".into(),
            provider_action: ProviderAction::Destroy,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let config: Self = serde_json::from_slice(&bytes)?;
                config.validate()?;
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !(300..=86_400).contains(&self.inactivity_seconds) {
            return Err(AgentError::new(
                "inactivitySeconds must be between 300 and 86400",
            ));
        }
        if !(1..=10).contains(&self.backup_app_limit) {
            return Err(AgentError::new("backupAppLimit must be between 1 and 10"));
        }
        if !self.controller_dead_zone.is_finite()
            || !(0.0..=0.95).contains(&self.controller_dead_zone)
        {
            return Err(AgentError::new(
                "controllerDeadZone must be between 0 and 0.95",
            ));
        }
        if self.enabled && self.instance_id == 0 {
            return Err(AgentError::new(
                "instanceId must be non-zero when lifecycle is enabled",
            ));
        }
        let base = self.vast_base_url.trim();
        if !base.starts_with("https://")
            || base.len() <= "https://".len()
            || base.chars().any(char::is_whitespace)
            || base.contains(['\n', '\r'])
        {
            return Err(AgentError::new("vastBaseUrl must be an HTTPS URL"));
        }
        for (name, path) in [
            ("stateAgentSocket", &self.state_agent_socket),
            ("statusSocket", &self.status_socket),
            ("activitySocket", &self.activity_socket),
            ("databasePath", &self.database_path),
            ("capabilityPath", &self.capability_path),
        ] {
            if !path.is_absolute() {
                return Err(AgentError::new(format!("{name} must be an absolute path")));
            }
        }
        Ok(())
    }

    pub fn timeout_ms(&self) -> u64 {
        self.inactivity_seconds.saturating_mul(1_000)
    }

    pub fn runtime_paths_match(&self, other: &Self) -> bool {
        self.state_agent_socket == other.state_agent_socket
            && self.status_socket == other.status_socket
            && self.activity_socket == other.activity_socket
            && self.database_path == other.database_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_contract() {
        let config = Config::default();
        assert!(!config.enabled);
        assert_eq!(config.inactivity_seconds, 10_800);
        assert_eq!(config.backup_app_limit, 3);
        assert_eq!(config.controller_dead_zone, 0.15);
        assert_eq!(config.provider_action, ProviderAction::Destroy);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validates_policy_bounds_and_enabled_instance() {
        let mut config = Config {
            enabled: true,
            ..Config::default()
        };
        assert!(config.validate().is_err());
        config.instance_id = 1;
        config.inactivity_seconds = 299;
        assert!(config.validate().is_err());
        config.inactivity_seconds = 300;
        config.backup_app_limit = 11;
        assert!(config.validate().is_err());
        config.backup_app_limit = 1;
        config.controller_dead_zone = 0.951;
        assert!(config.validate().is_err());
    }
}
