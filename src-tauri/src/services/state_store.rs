use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::fs;
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::{ConnectionProvider, PersistedAppState},
};

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load_state(&self) -> AppResult<PersistedAppState>;
    async fn save_state(&self, state: &PersistedAppState) -> AppResult<()>;
    fn path(&self) -> &Path;
}

#[derive(Debug, Clone)]
pub struct JsonStateStore {
    state_path: PathBuf,
    current_version: u32,
}

impl JsonStateStore {
    pub fn new(state_path: PathBuf, current_version: u32) -> Self {
        Self {
            state_path,
            current_version,
        }
    }

    async fn ensure_parent_exists(&self) -> AppResult<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    fn migrate_value(&self, raw_value: Value) -> AppResult<PersistedAppState> {
        let mut baseline = serde_json::to_value(PersistedAppState::default())?;
        reconcile_json(&mut baseline, &raw_value);

        let mut migrated: PersistedAppState = serde_json::from_value(baseline)?;
        for server in &mut migrated.provisioned_servers {
            server.connection_provider = ConnectionProvider::Wireguard;
            server.embedded_moonlight_pipeline_enabled = true;
            if server.embedded_moonlight_host_id.trim().is_empty() {
                server.embedded_moonlight_host_id = format!("instance-{}", server.instance_id);
            }
        }
        migrated.connection_provider = ConnectionProvider::Wireguard;
        migrated.version = self.current_version;
        migrated.server_preferences.template_hash =
            if migrated.server_preferences.template_hash.is_empty() {
                "566868bff8b15eef891ee706acbbb5e5".to_string()
            } else {
                migrated.server_preferences.template_hash
            };

        Ok(migrated)
    }

    async fn recover_invalid_state(&self, error: &AppError) -> AppResult<PersistedAppState> {
        let backup_path = self.state_path.with_extension(format!(
            "invalid-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));

        fs::rename(&self.state_path, &backup_path).await?;
        warn!(
            state_path = %self.state_path.display(),
            backup_path = %backup_path.display(),
            %error,
            "Recovered an invalid persisted state file"
        );

        let state = PersistedAppState::default();
        self.save_state(&state).await?;
        Ok(state)
    }

    async fn load_existing_root_map(&self) -> AppResult<Map<String, Value>> {
        if !self.state_path.exists() {
            return Ok(Map::new());
        }

        let contents = fs::read_to_string(&self.state_path).await?;
        let raw_value: Value = serde_json::from_str(&contents).map_err(|error| {
            AppError::Serialization(format!(
                "Failed to parse state file at {}: {error}",
                self.state_path.display()
            ))
        })?;

        match raw_value {
            Value::Object(map) => Ok(map),
            _ => Err(AppError::Serialization(format!(
                "State file at {} is not a JSON object",
                self.state_path.display()
            ))),
        }
    }
}

#[async_trait]
impl StateStore for JsonStateStore {
    async fn load_state(&self) -> AppResult<PersistedAppState> {
        self.ensure_parent_exists().await?;

        if !self.state_path.exists() {
            let state = PersistedAppState::default();
            self.save_state(&state).await?;
            return Ok(state);
        }

        let contents = fs::read_to_string(&self.state_path).await?;
        let raw_value: Value = match serde_json::from_str(&contents) {
            Ok(value) => value,
            Err(error) => {
                let error = AppError::Serialization(format!(
                    "Failed to parse state file at {}: {error}",
                    self.state_path.display()
                ));
                return self.recover_invalid_state(&error).await;
            }
        };

        let migrated = match self.migrate_value(raw_value) {
            Ok(state) => state,
            Err(error @ AppError::Serialization(_)) => {
                return self.recover_invalid_state(&error).await;
            }
            Err(error) => return Err(error),
        };
        self.save_state(&migrated).await?;
        Ok(migrated)
    }

    async fn save_state(&self, state: &PersistedAppState) -> AppResult<()> {
        self.ensure_parent_exists().await?;

        let mut root_map = match self.load_existing_root_map().await {
            Ok(map) => map,
            Err(AppError::Serialization(_)) if self.state_path.exists() => Map::new(),
            Err(error) => return Err(error),
        };

        let next_value = serde_json::to_value(state)?;
        let next_map = match next_value {
            Value::Object(map) => map,
            _ => {
                return Err(AppError::Serialization(
                    "PersistedAppState did not serialize to a JSON object".to_string(),
                ))
            }
        };

        for (key, value) in next_map {
            root_map.insert(key, value);
        }

        let body = serde_json::to_string_pretty(&Value::Object(root_map))?;
        fs::write(&self.state_path, body).await?;
        Ok(())
    }

    fn path(&self) -> &Path {
        &self.state_path
    }
}

fn reconcile_json(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                match target_map.get_mut(key) {
                    Some(target_value) => reconcile_json(target_value, source_value),
                    None => {
                        // Preserve unknown top-level fields when saving later, but never let
                        // unknown fields influence typed deserialization of PersistedAppState.
                        target_map.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (Value::Array(target_array), Value::Array(source_array)) => {
            if let Some(template) = target_array.first().cloned() {
                *target_array = source_array
                    .iter()
                    .map(|source_item| {
                        let mut item = template.clone();
                        reconcile_json(&mut item, source_item);
                        item
                    })
                    .collect();
            } else {
                *target_array = source_array.clone();
            }
        }
        (target_slot @ Value::Null, source_value) => {
            *target_slot = source_value.clone();
        }
        (target_slot @ Value::Bool(_), source_value @ Value::Bool(_))
        | (target_slot @ Value::Number(_), source_value @ Value::Number(_))
        | (target_slot @ Value::String(_), source_value @ Value::String(_)) => {
            *target_slot = source_value.clone();
        }
        // If an older or manually edited state file has the wrong shape for a
        // known field, keep the current default for that field instead of making
        // the whole state fail to deserialize and resetting the user.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tokio::fs;

    use super::{JsonStateStore, StateStore};
    use crate::models::app_state::{
        ConnectionProvider, OrchestrationState, PersistedAppState, ProvisionedServerState,
    };

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-json-state-store-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base.join("state.json")
    }

    #[test]
    fn every_state_enables_embedded_moonlight_for_existing_servers() {
        let store = JsonStateStore::new(PathBuf::from("state.json"), 2);
        let mut state = PersistedAppState::default();
        state.version = 2;
        let mut server = ProvisionedServerState::new(42);
        server.embedded_moonlight_pipeline_enabled = false;
        server.embedded_moonlight_host_id.clear();
        state.provisioned_servers.push(server);

        let migrated = store
            .migrate_value(serde_json::to_value(state).unwrap())
            .unwrap();
        let migrated_server = migrated.provisioned_servers.first().unwrap();

        assert_eq!(migrated.version, 2);
        assert!(migrated_server.embedded_moonlight_pipeline_enabled);
        assert_eq!(migrated_server.embedded_moonlight_host_id, "instance-42");
    }

    #[test]
    fn retired_provider_state_maps_to_managed_tunnel_state() {
        let store = JsonStateStore::new(PathBuf::from("state.json"), 2);
        let migrated = store
            .migrate_value(json!({
                "version": 2,
                "connectionProvider": "tailscale",
                "orchestrationState": "TailscaleConnected"
            }))
            .unwrap();

        assert_eq!(migrated.connection_provider, ConnectionProvider::Wireguard);
        assert_eq!(
            migrated.orchestration_state,
            OrchestrationState::WireGuardConnected
        );
    }

    #[tokio::test]
    async fn invalid_state_is_backed_up_and_replaced() {
        let path = temp_state_path("recover-invalid");
        fs::write(&path, b"{ truncated").await.unwrap();

        let store = JsonStateStore::new(path.clone(), 3);
        let recovered = store.load_state().await.unwrap();

        assert_eq!(recovered.version, PersistedAppState::default().version);
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
        assert!(persisted.is_object());

        let backup_count = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("state.invalid-")
            })
            .count();
        assert_eq!(backup_count, 1);
    }

    #[tokio::test]
    async fn incompatible_state_fields_are_reconciled_without_resetting_user_state() {
        let path = temp_state_path("reconcile-incompatible");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1,
                "onboardingCompleted": true,
                "credentials": {
                    "appUsername": "felipe",
                    "appPassword": "secret",
                    "vastApiKey": "vast-key"
                },
                "moonlightPreferences": {
                    "width": "invalid",
                    "height": 1664
                },
                "serverPreferences": {
                    "storageGb": "bad",
                    "templateHash": "abc123"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let store = JsonStateStore::new(path.clone(), 3);
        let recovered = store.load_state().await.unwrap();

        assert!(recovered.onboarding_completed);
        assert_eq!(recovered.credentials.app_username, "felipe");
        assert_eq!(recovered.credentials.vast_api_key, "vast-key");
        assert_eq!(recovered.version, 3);
        assert_eq!(
            recovered.moonlight_preferences.width,
            PersistedAppState::default().moonlight_preferences.width
        );
        assert_eq!(recovered.moonlight_preferences.height, 1664);
        assert_eq!(
            recovered.server_preferences.storage_gb,
            PersistedAppState::default().server_preferences.storage_gb
        );
        assert_eq!(recovered.server_preferences.template_hash, "abc123");
    }

    #[tokio::test]
    async fn save_state_preserves_moonlight_category() {
        let path = temp_state_path("preserve-moonlight");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "moonligConf": {
                    "schemaVersion": 1,
                    "hosts": {
                        "instance-1": {
                            "hostId": "instance-1"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let store = JsonStateStore::new(path.clone(), 1);
        let mut state = PersistedAppState::default();
        state.onboarding_completed = true;
        store.save_state(&state).await.unwrap();

        let root: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(
            root.get("moonligConf")
                .and_then(|value| value.get("hosts"))
                .and_then(|value| value.get("instance-1"))
                .and_then(|value| value.get("hostId"))
                .and_then(|value| value.as_str()),
            Some("instance-1")
        );
        assert_eq!(
            root.get("onboardingCompleted")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }
}
