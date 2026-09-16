use std::path::Path;

use chrono::{DateTime, Utc};
use noland_rclone_adapter::EphemeralRcloneSession;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{Config, ProviderAction};
use crate::{AgentError, Result};

pub const CAPABILITY_VERSION: u32 = 1;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageCapability {
    pub version: u32,
    pub instance_id: u64,
    pub profile_id: String,
    pub expires_at: DateTime<Utc>,
    pub session: EphemeralRcloneSession,
    pub master_key_hex: String,
    pub provider: VastCapability,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VastCapability {
    pub kind: VastProviderKind,
    pub api_key: String,
    pub instance_id: u64,
    pub action: ProviderAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VastProviderKind {
    Vast,
}

impl Drop for StorageCapability {
    fn drop(&mut self) {
        self.master_key_hex.clear();
    }
}

impl Drop for VastCapability {
    fn drop(&mut self) {
        self.api_key.clear();
    }
}

impl StorageCapability {
    pub fn load_and_validate(path: &Path, config: &Config, now: DateTime<Utc>) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|error| {
            AgentError::new(format!("unable to read lifecycle capability: {error}"))
        })?;
        let capability: Self = serde_json::from_slice(&bytes)
            .map_err(|error| AgentError::new(format!("invalid lifecycle capability: {error}")))?;
        capability.validate(config, now)?;
        Ok(capability)
    }

    pub fn validate(&self, config: &Config, now: DateTime<Utc>) -> Result<()> {
        if self.version != CAPABILITY_VERSION {
            return Err(AgentError::new("unsupported capability version"));
        }
        if self.instance_id != config.instance_id
            || self.provider.instance_id != config.instance_id
            || self.provider.instance_id != self.instance_id
        {
            return Err(AgentError::new(
                "capability instance does not match configuration",
            ));
        }
        if self.provider.action != config.provider_action {
            return Err(AgentError::new(
                "capability action does not match configuration",
            ));
        }
        if self.expires_at <= now {
            return Err(AgentError::new("lifecycle capability is expired"));
        }
        if self.profile_id.trim().is_empty() {
            return Err(AgentError::new("capability profileId is empty"));
        }
        if self.provider.api_key.trim().is_empty() {
            return Err(AgentError::new("capability provider credential is empty"));
        }
        if self.master_key_hex.len() != 64
            || !self
                .master_key_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AgentError::new(
                "capability masterKeyHex must be 64 hex characters",
            ));
        }
        Ok(())
    }

    pub fn session_for_operation(&self, operation_id: Uuid) -> EphemeralRcloneSession {
        let mut session = self.session.clone();
        session.operation_id = operation_id.to_string();
        session
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use noland_rclone_adapter::{
        session_from_input, AdapterCredential, AdapterInput, ProviderKind, TokenMode,
    };

    use super::*;

    fn capability() -> StorageCapability {
        let session = session_from_input(
            &AdapterInput {
                provider: ProviderKind::Local,
                remote_name: "test".into(),
                credentials: AdapterCredential::LocalPath {
                    path: "/tmp".into(),
                },
                fields: BTreeMap::new(),
                bucket: None,
                prefix: None,
            },
            "seed",
            TokenMode::Operation,
        )
        .unwrap();
        StorageCapability {
            version: 1,
            instance_id: 42,
            profile_id: "profile".into(),
            expires_at: Utc.timestamp_opt(2_000, 0).unwrap(),
            session,
            master_key_hex: "ab".repeat(32),
            provider: VastCapability {
                kind: VastProviderKind::Vast,
                api_key: "secret".into(),
                instance_id: 42,
                action: ProviderAction::Destroy,
            },
        }
    }

    #[test]
    fn rejects_mismatch_expiry_and_bad_key() {
        let now = Utc.timestamp_opt(1_000, 0).unwrap();
        let config = Config {
            enabled: true,
            instance_id: 42,
            ..Config::default()
        };
        assert!(capability().validate(&config, now).is_ok());

        let mut value = capability();
        value.provider.instance_id = 41;
        assert!(value.validate(&config, now).is_err());
        let mut value = capability();
        value.expires_at = now;
        assert!(value.validate(&config, now).is_err());
        let mut value = capability();
        value.master_key_hex = "not-a-key".into();
        assert!(value.validate(&config, now).is_err());
    }
}
