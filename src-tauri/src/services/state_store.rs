use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
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

        let body = serde_json::to_string_pretty(state)?;
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
