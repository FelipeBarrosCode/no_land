use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use tokio::fs;

use crate::{
    errors::{AppError, AppResult},
    utils::{managed_binaries::configure_bundled_linux_runtime, process::configure_no_window},
};

use super::{os_detection::OsDetection, vast_api::VastApiClient};

fn locate_ssh_key_tool(tool: &str) -> Option<PathBuf> {
    let os = OsDetection::new();

    match tool {
        "ssh" => os.locate_app_managed_binary("ssh", "NOLAND_SSH_BIN", cfg!(target_os = "windows")),
        "ssh-keygen" => os.locate_app_managed_binary(
            "ssh-keygen",
            "NOLAND_SSH_KEYGEN_BIN",
            cfg!(target_os = "windows"),
        ),
        _ => None,
    }
}

fn resolve_ssh_key_tool(tool: &str) -> AppResult<PathBuf> {
    let os = OsDetection::new();
    locate_ssh_key_tool(tool).ok_or_else(|| {
        AppError::Command(format!(
            "`{tool}` is not available in the app bundle. {}",
            os.install_hint_for_tool(tool)
        ))
    })
}

#[derive(Debug, Clone)]
pub struct SshKeyPaths {
    pub private_key_path: PathBuf,
    pub public_key_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SshKeyService {
    pub key_name: String,
}

impl SshKeyService {
    pub fn new(key_name: impl Into<String>) -> Self {
        Self {
            key_name: key_name.into(),
        }
    }

    pub async fn ensure_keypair(&self, root_dir: &Path) -> AppResult<SshKeyPaths> {
        let ssh_keygen_bin = resolve_ssh_key_tool("ssh-keygen")?;

        let keys_dir = root_dir.join("keys");
        fs::create_dir_all(&keys_dir).await?;

        let private_key_path = keys_dir.join(&self.key_name);
        let public_key_path = keys_dir.join(format!("{}.pub", &self.key_name));

        if private_key_path.exists() && public_key_path.exists() {
            return Ok(SshKeyPaths {
                private_key_path,
                public_key_path,
            });
        }

        let private_path_string = private_key_path.display().to_string();
        tokio::task::spawn_blocking(move || {
            let mut command = Command::new(&ssh_keygen_bin);
            configure_bundled_linux_runtime(
                &mut command,
                &ssh_keygen_bin,
                "ssh-runtime",
                OsDetection::new().managed_binary_target_triple(),
            );
            configure_no_window(&mut command);
            let output = command
                .stdin(Stdio::null())
                .arg("-t")
                .arg("ed25519")
                .arg("-f")
                .arg(&private_path_string)
                .arg("-N")
                .arg("")
                .arg("-C")
                .arg("noland-connect")
                .output()
                .map_err(|error| AppError::Command(format!("ssh-keygen failed: {error}")))?;

            if !output.status.success() {
                return Err(AppError::Command(format!(
                    "ssh-keygen exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            Ok::<(), AppError>(())
        })
        .await
        .map_err(|error| AppError::Command(format!("ssh-keygen task join failure: {error}")))??;

        Ok(SshKeyPaths {
            private_key_path,
            public_key_path,
        })
    }

    pub async fn regenerate_keypair(&self, root_dir: &Path) -> AppResult<SshKeyPaths> {
        let keys_dir = root_dir.join("keys");
        fs::create_dir_all(&keys_dir).await?;

        let private_key_path = keys_dir.join(&self.key_name);
        let public_key_path = keys_dir.join(format!("{}.pub", &self.key_name));

        if let Err(error) = fs::remove_file(&private_key_path).await {
            if error.kind() != ErrorKind::NotFound {
                return Err(AppError::from(error));
            }
        }

        if let Err(error) = fs::remove_file(&public_key_path).await {
            if error.kind() != ErrorKind::NotFound {
                return Err(AppError::from(error));
            }
        }

        self.ensure_keypair(root_dir).await
    }

    pub async fn regenerate_and_upload_public_key(
        &self,
        root_dir: &Path,
        vast_api: &VastApiClient,
    ) -> AppResult<(SshKeyPaths, bool)> {
        let key_paths = self.regenerate_keypair(root_dir).await?;
        let uploaded = self
            .upload_public_key_if_missing(vast_api, &key_paths.public_key_path)
            .await?;

        Ok((key_paths, uploaded))
    }

    pub async fn load_key_into_agent(&self, key_path: &Path, _passphrase: &str) -> AppResult<()> {
        if !key_path.exists() {
            return Err(AppError::NotFound(format!(
                "SSH private key not found at {}",
                key_path.display()
            )));
        }

        Ok(())
    }

    pub async fn upload_public_key_if_missing(
        &self,
        vast_api: &VastApiClient,
        public_key_path: &Path,
    ) -> AppResult<bool> {
        let public_key = fs::read_to_string(public_key_path)
            .await?
            .trim()
            .to_string();
        if public_key.is_empty() {
            return Err(AppError::InvalidInput(
                "Generated SSH public key file is empty".to_string(),
            ));
        }

        let keys = vast_api.list_ssh_keys().await?;
        let already_uploaded = keys
            .iter()
            .any(|existing| normalize_key(&existing.key) == normalize_key(&public_key));

        if already_uploaded {
            return Ok(false);
        }

        vast_api.upload_ssh_key(&public_key).await?;
        Ok(true)
    }
}

pub fn normalize_ssh_state_from_disk(
    state: &mut crate::models::app_state::PersistedAppState,
    app_data_dir: &Path,
) -> bool {
    let mut changed = false;
    let key_name = if state.ssh.key_name.trim().is_empty() {
        "nolandConnectSSH"
    } else {
        state.ssh.key_name.as_str()
    };

    let private_key_path = app_data_dir.join("keys").join(key_name);
    let public_key_path = app_data_dir.join("keys").join(format!("{key_name}.pub"));

    if private_key_path.exists() && public_key_path.exists() {
        let private_key_path = private_key_path.display().to_string();
        let public_key_path = public_key_path.display().to_string();

        if state.ssh.key_name != key_name {
            state.ssh.key_name = key_name.to_string();
            changed = true;
        }
        if state.ssh.private_key_path != private_key_path {
            state.ssh.private_key_path = private_key_path;
            changed = true;
        }
        if state.ssh.public_key_path != public_key_path {
            state.ssh.public_key_path = public_key_path;
            changed = true;
        }
    }

    changed
}

fn normalize_key(key: &str) -> String {
    key.trim()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}
