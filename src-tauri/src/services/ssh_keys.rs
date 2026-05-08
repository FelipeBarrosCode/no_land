use std::{
    path::{Path, PathBuf},
    process::Command,
};

use tokio::fs;

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
        if !command_exists("ssh-keygen") {
            return Err(AppError::Command(
                "`ssh-keygen` is not available in PATH. Install OpenSSH client tools and retry."
                    .to_string(),
            ));
        }

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

        Ok(SshKeyPaths {
            private_key_path,
            public_key_path,
        })
    }

    pub async fn load_key_into_agent(&self, key_path: &Path, passphrase: &str) -> AppResult<()> {
        if !command_exists("ssh-add") {
            return Err(AppError::Command(
                "`ssh-add` is not available in PATH. Install OpenSSH client tools and retry."
                    .to_string(),
            ));
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
                let _ = Command::new("powershell")
                    .args([
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-Command",
                        "if (Get-Service ssh-agent -ErrorAction SilentlyContinue) { Start-Service ssh-agent -ErrorAction SilentlyContinue }",
                    ])
                    .output();

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

fn command_exists(command: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        return Command::new("where")
            .arg(command)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    }

    #[cfg(not(target_os = "windows"))]
    {
        Command::new("sh")
            .arg("-lc")
            .arg(format!("command -v {command} >/dev/null 2>&1"))
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

fn normalize_key(key: &str) -> String {
    key.trim()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}
