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
        // rclone's INI format is line-oriented. Keep values within their entry.
        let key = key.into().replace(['\r', '\n'], "");
        let value = value
            .into()
            .replace("\r\n", "\\n")
            .replace(['\r', '\n'], "\\n");
        self.config_entries.push((key, value));
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
    fn normalized_root(&self) -> &str {
        let root = self.root.trim_end_matches('/');
        if root.is_empty() && self.root.starts_with('/') {
            "/"
        } else {
            root
        }
    }

    pub fn remote_path(&self) -> String {
        if self.root.trim().is_empty() {
            format!("{}:", self.remote_name)
        } else {
            format!("{}:{}", self.remote_name, self.normalized_root())
        }
    }

    pub fn key_path(&self, key: &str) -> String {
        let key = key.trim_start_matches('/');
        if self.root.trim().is_empty() {
            format!("{}:{}", self.remote_name, key)
        } else if self.normalized_root() == "/" {
            format!("{}:/{}", self.remote_name, key)
        } else {
            format!("{}:{}/{}", self.remote_name, self.normalized_root(), key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_and_relative_roots_remain_distinct() {
        for (root, remote, object) in [
            (
                "/srv/backups/",
                "store:/srv/backups",
                "store:/srv/backups/packs/a",
            ),
            (
                "srv/backups/",
                "store:srv/backups",
                "store:srv/backups/packs/a",
            ),
            ("/", "store:/", "store:/packs/a"),
            ("", "store:", "store:packs/a"),
        ] {
            let root = RcloneRoot {
                remote_name: "store".into(),
                root: root.into(),
            };
            assert_eq!(root.remote_path(), remote);
            assert_eq!(root.key_path("packs/a"), object);
        }
    }

    #[test]
    fn config_values_cannot_add_sections() {
        let config = RcloneRemoteConfig::new("store", "sftp")
            .entry("key_pem", "line one\r\n[another]\nline two")
            .to_ini_string();
        assert!(config.contains("key_pem = line one\\n[another]\\nline two\n"));
        assert_eq!(
            config.lines().filter(|line| line.starts_with('[')).count(),
            1
        );
    }
}
