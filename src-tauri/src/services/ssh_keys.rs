use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use tokio::fs;
use rand_core::OsRng;
use ssh_key::{LineEnding, private::PrivateKey};
use tracing::warn;

use crate::errors::{AppError, AppResult};

use super::{os_detection::OsDetection, vast_api::VastApiClient};

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

        let os = OsDetection::new();
        if os.command_exists("ssh-keygen") {
            self.generate_with_ssh_keygen(&private_key_path).await?;
        } else {
            self.generate_with_internal_rust(&private_key_path, &public_key_path)
                .await?;
        }

        Ok(SshKeyPaths {
            private_key_path,
            public_key_path,
        })
    }

    async fn generate_with_ssh_keygen(&self, private_key_path: &Path) -> AppResult<()> {
        let private_path_string = private_key_path.display().to_string();
        tokio::task::spawn_blocking(move || {
            let output = Command::new("ssh-keygen")
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

        Ok(())
    }

    async fn generate_with_internal_rust(
        &self,
        private_key_path: &Path,
        public_key_path: &Path,
    ) -> AppResult<()> {
        let private_key_path = private_key_path.to_path_buf();
        let public_key_path = public_key_path.to_path_buf();

        tokio::task::spawn_blocking(move || {
            let mut rng = OsRng;
            let mut private_key = PrivateKey::random(&mut rng, ssh_key::Algorithm::Ed25519)
                .map_err(|error| AppError::Command(format!("internal ssh keygen failed: {error}")))?;
            private_key.set_comment("noland-connect");

            let private_pem = private_key
                .to_openssh(LineEnding::LF)
                .map_err(|error| AppError::Command(format!("failed encoding private key: {error}")))?;
            let public_key = private_key.public_key().to_openssh().map_err(|error| {
                AppError::Command(format!("failed encoding public key: {error}"))
            })?;

            std::fs::write(&private_key_path, private_pem.as_bytes()).map_err(|error| {
                AppError::Command(format!(
                    "failed writing private key {}: {error}",
                    private_key_path.display()
                ))
            })?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&private_key_path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| {
                    AppError::Command(format!(
                        "failed setting private key permissions {}: {error}",
                        private_key_path.display()
                    ))
                })?;
            }

            std::fs::write(&public_key_path, format!("{}\n", public_key)).map_err(|error| {
                AppError::Command(format!(
                    "failed writing public key {}: {error}",
                    public_key_path.display()
                ))
            })?;

            Ok::<(), AppError>(())
        })
        .await
        .map_err(|error| AppError::Command(format!("internal ssh keygen task join failure: {error}")))??;

        Ok(())
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

    pub async fn load_key_into_agent(&self, key_path: &Path, passphrase: &str) -> AppResult<()> {
        let os = OsDetection::new();
        if !os.command_exists("ssh-add") {
            warn!(
                "ssh-add is unavailable; continuing without agent key loading (key_path={})",
                key_path.display()
            );
            return Ok(());
        }

        let (auth_sock, agent_pid) = self.start_or_get_ssh_agent().await?;
        if let Some(sock) = auth_sock.as_deref() {
            std::env::set_var("SSH_AUTH_SOCK", sock);
        }
        if let Some(pid) = agent_pid.as_deref() {
            std::env::set_var("SSH_AGENT_PID", pid);
        }

        let key_path_str = key_path.display().to_string();
        let passphrase_owned = passphrase.to_string();
        let auth_sock_owned = auth_sock.clone();
        let os = OsDetection::new();

        tokio::task::spawn_blocking(move || {
            use std::io::Write;

            if let Some(sock) = auth_sock_owned.as_deref() {
                std::env::set_var("SSH_AUTH_SOCK", sock);
            }
            if let Some(ref pid_str) = agent_pid {
                std::env::set_var("SSH_AGENT_PID", pid_str);
            }

            let mut add_command = Command::new("ssh-add");
            add_command.args(os.ssh_add_args_for_key(&key_path_str));

            let output = add_command
                .output()
                .map_err(|error| AppError::Command(format!("Failed to spawn ssh-add: {error}")))?;

            if output.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("passphrase") || stderr.contains("bad passphrase") {
                let mut child = Command::new("ssh-add")
                    .args(os.ssh_add_stdin_args())
                    .stdin(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|error| {
                        AppError::Command(format!("Failed to spawn ssh-add stdin mode: {error}"))
                    })?;

                if let Some(ref mut stdin) = child.stdin {
                    let payload = format!("{}\n", passphrase_owned);
                    stdin.write_all(payload.as_bytes()).map_err(|error| {
                        AppError::Command(format!("Failed to write passphrase: {error}"))
                    })?;
                }

                let status = child
                    .wait()
                    .map_err(|error| AppError::Command(format!("ssh-add wait failed: {error}")))?;

                if !status.success() {
                    let output = child.wait_with_output().map_err(|error| {
                        AppError::Command(format!("Failed to get ssh-add stderr: {error}"))
                    })?;
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(AppError::Command(format!(
                        "ssh-add failed with {}: {}",
                        status, stderr
                    )));
                }

                return Ok(());
            }

            return Err(AppError::Command(format!("ssh-add failed: {}", stderr)));
        })
        .await
        .map_err(|error| AppError::Command(format!("ssh-add task join failure: {error}")))?
    }

    async fn start_or_get_ssh_agent(&self) -> AppResult<(Option<String>, Option<String>)> {
        tokio::task::spawn_blocking(|| {
            let os = OsDetection::new();

            if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
                if !sock.is_empty() {
                    let pid = std::env::var("SSH_AGENT_PID")
                        .ok()
                        .filter(|p| !p.is_empty());
                    return Ok((Some(sock), pid));
                }
            }

            if os.is_windows() {
                let ensure_agent_running = Command::new("powershell")
                    .args([
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        "$svc = Get-Service ssh-agent -ErrorAction SilentlyContinue; if (-not $svc) { Write-Error 'OpenSSH Authentication Agent service (ssh-agent) not found'; exit 2 }; if ($svc.Status -ne 'Running') { Start-Service ssh-agent -ErrorAction Stop; Start-Sleep -Milliseconds 300; $svc = Get-Service ssh-agent -ErrorAction Stop }; if ($svc.Status -ne 'Running') { Write-Error 'OpenSSH Authentication Agent service is not running'; exit 3 }; Write-Output 'running'",
                    ])
                    .output()
                    .map_err(|error| AppError::Command(format!(
                        "Failed to verify/start Windows ssh-agent service: {error}"
                    )))?;

                if !ensure_agent_running.status.success() {
                    return Err(AppError::Command(format!(
                        "Windows OpenSSH Agent service is unavailable or could not be started. Run Noland Connect as administrator once, or manually set OpenSSH Authentication Agent to Automatic and start it. Details: {}",
                        String::from_utf8_lossy(&ensure_agent_running.stderr).trim()
                    )));
                }

                let mut ready = false;
                for _ in 0..6 {
                    let probe = Command::new("ssh-add").arg("-l").output();
                    if let Ok(output) = probe {
                        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
                        if output.status.success()
                            || stderr.contains("the agent has no identities")
                            || stderr.contains("no identities")
                        {
                            ready = true;
                            break;
                        }
                    }
                    thread::sleep(Duration::from_millis(250));
                }

                if !ready {
                    return Err(AppError::Command(
                        "Windows OpenSSH Agent service started but is not ready to accept ssh-add requests. Please restart the ssh-agent service or Windows and retry.".to_string(),
                    ));
                }

                return Ok((None, None));
            }

            let output = Command::new("ssh-agent")
                .arg("-s")
                .output()
                .map_err(|error| {
                    AppError::Command(format!("Failed to start ssh-agent: {error}"))
                })?;

            if !output.status.success() {
                return Err(AppError::Command(format!(
                    "ssh-agent failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut auth_sock = None;
            let mut agent_pid = None;

            for chunk in stdout.split(';') {
                let trimmed = chunk.trim();
                if let Some((key, value)) = trimmed.split_once('=') {
                    let value = value.trim().trim_matches('"').trim_matches('\'');
                    if key == "SSH_AUTH_SOCK" && !value.is_empty() {
                        auth_sock = Some(value.to_string());
                    } else if key == "SSH_AGENT_PID" && !value.is_empty() {
                        agent_pid = Some(value.to_string());
                    }
                }
            }

            if auth_sock.is_none() {
                for line in stdout.lines() {
                    if let Some(sock) = line.strip_prefix("SSH_AUTH_SOCK=") {
                        auth_sock = Some(sock.trim_matches(';').trim().to_string());
                    } else if let Some(pid) = line.strip_prefix("SSH_AGENT_PID=") {
                        let p = pid.trim_matches(';').trim();
                        if !p.is_empty() {
                            agent_pid = Some(p.to_string());
                        }
                    }
                }
            }

            match auth_sock {
                Some(sock) => Ok((Some(sock), agent_pid)),
                None => Err(AppError::Command(
                    "ssh-agent did not provide SSH_AUTH_SOCK".to_string(),
                )),
            }
        })
        .await
        .map_err(|error| AppError::Command(format!("ssh-agent task join failure: {error}")))?
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
