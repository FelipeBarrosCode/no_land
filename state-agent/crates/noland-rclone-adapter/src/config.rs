use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RcloneRemoteConfig {
    pub name: String,
    pub backend_type: String,
    pub config_entries: Vec<(String, String)>,
}

impl RcloneRemoteConfig {
    pub fn new(name: impl Into<String>, backend_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backend_type: backend_type.into(),
            config_entries: Vec::new(),
        }
    }

    pub fn entry(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config_entries.push((key.into(), value.into()));
        self
    }

    pub fn to_ini_string(&self) -> String {
        let mut output = format!("[{}]\n", self.name);
        output.push_str(&format!("type = {}\n", self.backend_type));
        for (key, value) in &self.config_entries {
            output.push_str(&format!("{key} = {value}\n"));
        }
        output
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RcloneRoot {
    pub remote_name: String,
    pub root: String,
}

impl RcloneRoot {
    pub fn remote_path(&self) -> String {
        if self.root.trim().is_empty() {
            format!("{}:", self.remote_name)
        } else {
            format!("{}:{}", self.remote_name, self.root.trim_matches('/'))
        }
    }

    pub fn key_path(&self, key: &str) -> String {
        let key = key.trim_start_matches('/');
        if self.root.trim().is_empty() {
            format!("{}:{}", self.remote_name, key)
        } else {
            format!("{}:{}/{}", self.remote_name, self.root.trim_matches('/'), key)
        }
    }
}
