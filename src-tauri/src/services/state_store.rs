use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::fs;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::PersistedAppState,
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
        merge_json(&mut baseline, &raw_value);

        let mut migrated: PersistedAppState = serde_json::from_value(baseline)?;
        migrated.version = self.current_version;
        migrated.server_preferences.template_hash =
            if migrated.server_preferences.template_hash.is_empty() {
                "566868bff8b15eef891ee706acbbb5e5".to_string()
            } else {
                migrated.server_preferences.template_hash
            };

        Ok(migrated)
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
        let raw_value: Value = serde_json::from_str(&contents).map_err(|error| {
            AppError::Serialization(format!(
                "Failed to parse state file at {}: {error}",
                self.state_path.display()
            ))
        })?;

        let migrated = self.migrate_value(raw_value)?;
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

fn merge_json(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                match target_map.get_mut(key) {
                    Some(target_value) => merge_json(target_value, source_value),
                    None => {
                        target_map.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (target_slot, source_value) => {
            *target_slot = source_value.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tokio::fs;

    use super::{JsonStateStore, StateStore};
    use crate::models::app_state::PersistedAppState;

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-json-state-store-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base.join("state.json")
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
