use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Map, Value};

use super::{
    atomic_file::write_atomically,
    migrations::migrate,
    schema::{MoonlightRootDocument, MOONLIGHT_CONFIG_KEY},
};
use crate::moonlight::domain::{MoonlightConfiguration, MoonlightError, PersistedHost};

pub trait MoonlightStateRepository: Send + Sync {
    fn snapshot(&self) -> Result<MoonlightConfiguration, MoonlightError>;

    fn update<R>(
        &self,
        update: impl FnOnce(&mut MoonlightConfiguration) -> Result<R, MoonlightError>,
    ) -> Result<R, MoonlightError>;

    fn get_host(&self, host_id: &str) -> Result<PersistedHost, MoonlightError>;
}

#[derive(Debug)]
pub struct JsonMoonlightStateRepository {
    state_path: PathBuf,
    process_mutex: Mutex<()>,
}

impl JsonMoonlightStateRepository {
    pub fn new(state_path: PathBuf) -> Self {
        Self {
            state_path,
            process_mutex: Mutex::new(()),
        }
    }

    fn ensure_parent_exists(&self) -> Result<(), MoonlightError> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    fn recover_corrupt_state(&self) -> Result<(), MoonlightError> {
        if !self.state_path.exists() {
            return Ok(());
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| MoonlightError::Persistence(error.to_string()))?
            .as_secs();
        let backup_name = format!("state.corrupt.{timestamp}.json");
        let backup_path = self
            .state_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(backup_name);
        fs::rename(&self.state_path, backup_path)?;
        Ok(())
    }

    fn load_root_document(&self) -> Result<MoonlightRootDocument, MoonlightError> {
        self.ensure_parent_exists()?;

        if !self.state_path.exists() {
            return Ok(MoonlightRootDocument::default());
        }

        let contents = fs::read_to_string(&self.state_path)?;
        let raw_value: Value = match serde_json::from_str(&contents) {
            Ok(value) => value,
            Err(_) => {
                self.recover_corrupt_state()?;
                return Ok(MoonlightRootDocument::default());
            }
        };

        let Value::Object(mut root_map) = raw_value else {
            self.recover_corrupt_state()?;
            return Ok(MoonlightRootDocument::default());
        };

        let moonlight_value = root_map.remove(MOONLIGHT_CONFIG_KEY).unwrap_or(Value::Null);
        let moonlight_conf = migrate(moonlight_value)?;
        let other_categories = BTreeMap::from_iter(root_map.into_iter());

        Ok(MoonlightRootDocument {
            moonlight_conf,
            other_categories,
        })
    }

    fn write_root_document(&self, document: &MoonlightRootDocument) -> Result<(), MoonlightError> {
        let mut root_map = Map::from_iter(document.other_categories.clone().into_iter());
        root_map.insert(
            MOONLIGHT_CONFIG_KEY.to_string(),
            serde_json::to_value(&document.moonlight_conf)?,
        );
        let serialized = serde_json::to_vec_pretty(&Value::Object(root_map))?;
        write_atomically(&self.state_path, &serialized)
    }
}

impl MoonlightStateRepository for JsonMoonlightStateRepository {
    fn snapshot(&self) -> Result<MoonlightConfiguration, MoonlightError> {
        let _guard = self.process_mutex.lock().map_err(|_| {
            MoonlightError::Persistence("moonlight state mutex poisoned".to_string())
        })?;
        Ok(self.load_root_document()?.moonlight_conf)
    }

    fn update<R>(
        &self,
        update: impl FnOnce(&mut MoonlightConfiguration) -> Result<R, MoonlightError>,
    ) -> Result<R, MoonlightError> {
        let _guard = self.process_mutex.lock().map_err(|_| {
            MoonlightError::Persistence("moonlight state mutex poisoned".to_string())
        })?;
        let mut document = self.load_root_document()?;
        let result = update(&mut document.moonlight_conf)?;
        self.write_root_document(&document)?;
        Ok(result)
    }

    fn get_host(&self, host_id: &str) -> Result<PersistedHost, MoonlightError> {
        self.snapshot()?
            .hosts
            .get(host_id)
            .cloned()
            .ok_or_else(|| MoonlightError::Persistence(format!("host {host_id} not found")))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde_json::json;

    use super::{JsonMoonlightStateRepository, MoonlightStateRepository};

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-moonlight-tests-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&base).unwrap();
        base.join("state.json")
    }

    #[test]
    fn creates_new_state() {
        let path = temp_state_path("create");
        let repo = JsonMoonlightStateRepository::new(path.clone());
        let snapshot = repo.snapshot().unwrap();
        assert_eq!(snapshot.schema_version, 1);
        assert!(!path.exists());
    }

    #[test]
    fn preserves_unrelated_categories() {
        let path = temp_state_path("preserve");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({"version":1,"moonligConf":{"schemaVersion":1}}))
                .unwrap(),
        )
        .unwrap();
        let repo = JsonMoonlightStateRepository::new(path.clone());
        repo.update(|config| {
            config.last_selected_host_id = Some("host-1".to_string());
            Ok(())
        })
        .unwrap();

        let root: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(root.get("version").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(
            root.get("moonligConf")
                .and_then(|v| v.get("lastSelectedHostId"))
                .and_then(|v| v.as_str()),
            Some("host-1")
        );
    }

    #[test]
    fn recovers_malformed_json() {
        let path = temp_state_path("corrupt");
        fs::write(&path, b"{").unwrap();
        let repo = JsonMoonlightStateRepository::new(path.clone());
        let snapshot = repo.snapshot().unwrap();
        assert_eq!(snapshot.schema_version, 1);
        let dir_entries = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert!(dir_entries >= 1);
    }
}
