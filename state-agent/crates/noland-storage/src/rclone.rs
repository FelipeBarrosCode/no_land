use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use bytes::Bytes;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_state_core::{Result, StateError};
use tokio::process::Command;

use crate::{
    forbid_rclone_sync, Health, RemoteEntry, RemoteKey, RemoteMeta, SharedStorageProvider,
};

/// rclone-backed provider. Backup transfers use `copy` / `copyto` only.
/// Provider-specific remotes are created by `noland-rclone-adapter`.
pub struct RcloneStorage {
    pub remote: String,
    pub root: String,
    pub extra_args: Vec<String>,
    pub backend_type: String,
}

impl RcloneStorage {
    pub fn new(remote: impl Into<String>, root: impl Into<String>) -> Self {
        Self {
            remote: remote.into(),
            root: root.into(),
            extra_args: Vec::new(),
            backend_type: "unknown".into(),
        }
    }

    pub fn from_session(session: &EphemeralRcloneSession, config_path: &Path) -> Self {
        Self {
            remote: session.remote_name.clone(),
            root: session.root.clone(),
            backend_type: session.backend_type.clone(),
            extra_args: vec!["--config".into(), config_path.display().to_string()],
        }
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args.extend(args);
        self
    }

    pub fn provider_label(&self) -> String {
        format!("rclone:{}", self.backend_type)
    }

    pub fn remote_path(&self, key: &RemoteKey) -> String {
        format!(
            "{}:{}/{}",
            self.remote,
            self.root.trim_end_matches('/'),
            key.as_str().trim_start_matches('/')
        )
    }

    fn root_remote(&self) -> String {
        if self.root.trim().is_empty() {
            format!("{}:", self.remote)
        } else {
            format!("{}:{}", self.remote, self.root.trim_end_matches('/'))
        }
    }

    async fn run(&self, args: Vec<String>) -> Result<String> {
        forbid_rclone_sync(&args)?;
        let mut cmd = Command::new("rclone");
        cmd.args(&self.extra_args)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = cmd
            .output()
            .await
            .map_err(|e| StateError::Storage(e.to_string()))?;
        if !output.status.success() {
            return Err(StateError::Storage(format!(
                "rclone failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[async_trait]
impl SharedStorageProvider for RcloneStorage {
    async fn health_check(&self) -> Result<Health> {
        match self.run(vec!["lsd".into(), self.root_remote()]).await {
            Ok(detail) => Ok(Health {
                ok: true,
                provider: self.provider_label(),
                detail,
            }),
            Err(err) => Ok(Health {
                ok: false,
                provider: self.provider_label(),
                detail: err.to_string(),
            }),
        }
    }

    async fn ensure_root(&self) -> Result<()> {
        self.run(vec!["mkdir".into(), self.root_remote()]).await?;
        for child in ["catalog", "bundles", "packs", "checkpoints", "instances"] {
            let _ = self
                .run(vec![
                    "mkdir".into(),
                    format!("{}/{}", self.root_remote(), child),
                ])
                .await;
        }
        Ok(())
    }

    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        match self
            .run(vec![
                "lsf".into(),
                "--format".into(),
                "s".into(),
                self.remote_path(key),
            ])
            .await
        {
            Ok(out) => {
                let size = out
                    .lines()
                    .next()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                if out.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(RemoteMeta {
                        key: key.clone(),
                        size,
                    }))
                }
            }
            Err(_) => Ok(None),
        }
    }

    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        if self.stat(key).await?.is_some() {
            return Ok(RemoteMeta {
                key: key.clone(),
                size: std::fs::metadata(local)?.len(),
            });
        }
        self.run(vec![
            "copyto".into(),
            local.display().to_string(),
            self.remote_path(key),
        ])
        .await?;
        Ok(RemoteMeta {
            key: key.clone(),
            size: std::fs::metadata(local)?.len(),
        })
    }

    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.run(vec![
            "copyto".into(),
            self.remote_path(key),
            dest.display().to_string(),
        ])
        .await?;
        Ok(())
    }

    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        let out = self
            .run(vec![
                "lsf".into(),
                "-R".into(),
                "--format".into(),
                "ps".into(),
                self.remote_path(prefix),
            ])
            .await
            .unwrap_or_default();
        let mut entries = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(2, ';');
            let name = parts.next().unwrap_or_default();
            let size = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if name.is_empty() {
                continue;
            }
            entries.push(RemoteEntry {
                key: prefix.join(name.trim_end_matches('/')),
                size,
                is_prefix: name.ends_with('/'),
            });
        }
        Ok(entries)
    }

    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        let tmp =
            std::env::temp_dir().join(format!("noland-rclone-{}", key.as_str().replace('/', "_")));
        std::fs::write(&tmp, &bytes)?;
        let result = self.upload_immutable(&tmp, key).await;
        let _ = std::fs::remove_file(tmp);
        result
    }
}

pub fn write_ephemeral_session(
    run_root: &Path,
    session: &EphemeralRcloneSession,
) -> Result<PathBuf> {
    let dir = run_root.join("storage").join(&session.operation_id);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let config_path = dir.join("rclone.conf");
    std::fs::write(&config_path, session.config_ini.as_bytes())?;
    let session_path = dir.join("session.json");
    std::fs::write(&session_path, serde_json::to_vec(session)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(config_path)
}

pub fn shred_ephemeral_session(run_root: &Path, operation_id: &str) -> Result<()> {
    crate::shred_ephemeral_auth(run_root, operation_id)
}

#[cfg(test)]
fn assert_copy_only(command: &[String]) -> Result<()> {
    forbid_rclone_sync(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_rclone_adapter::{
        session_from_input, AdapterCredential, AdapterInput, ProviderKind, TokenMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn rejects_sync() {
        assert!(assert_copy_only(&["sync".into(), "src".into(), "dst".into()]).is_err());
        assert!(assert_copy_only(&["copy".into(), "src".into(), "dst".into()]).is_ok());
    }

    #[test]
    fn from_session_uses_config_flag_and_generic_paths() {
        let input = AdapterInput {
            provider: ProviderKind::Local,
            remote_name: "noland_local".into(),
            credentials: AdapterCredential::LocalPath {
                path: "/tmp/cloud".into(),
            },
            fields: BTreeMap::new(),
            bucket: None,
            prefix: Some("Noland Shared Storage".into()),
        };
        let session = session_from_input(&input, "op-42", TokenMode::Ephemeral).unwrap();
        let storage = RcloneStorage::from_session(
            &session,
            Path::new("/run/noland/storage/op-42/rclone.conf"),
        );
        assert_eq!(storage.provider_label(), "rclone:local");
        assert_eq!(
            storage.remote_path(&RemoteKey::new("packs/ab/id.pack")),
            "noland_local:Noland Shared Storage/packs/ab/id.pack"
        );
        assert!(storage.extra_args.contains(&"--config".to_string()));
        assert!(!session.config_ini.contains("sync"));
    }
}
