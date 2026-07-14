use std::time::Duration;

use tokio::process::Command as TokioCommand;
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

const TAILSCALE_INSTALL_TIMEOUT_SECS: u64 = 120;
const TAILSCALE_UP_TIMEOUT_SECS: u64 = 60;
const TAILSCALE_IP_POLL_MAX_ATTEMPTS: u32 = 30;
const TAILSCALE_IP_POLL_INTERVAL_SECS: u64 = 2;

/// Provisions Tailscale on both the remote instance and the local machine.
pub struct TailscaleService;

/// Result of a successful Tailscale provisioning operation.
pub struct TailscaleProvisionResult {
    /// The Tailscale IPv4 address of the remote machine within the tailnet.
    pub remote_tailscale_ip: String,
}

impl TailscaleService {
    /// Install and authenticate Tailscale on the remote Linux instance via SSH.
    /// Returns the Tailscale IP of the remote machine.
    pub async fn provision_remote(
        remote: &RemoteExec,
        api_key: &str,
        instance_id: u64,
    ) -> AppResult<TailscaleProvisionResult> {
        let hostname = format!("noland-{}", instance_id);

        info!(
            "Installing Tailscale on remote instance {} (hostname={})",
            instance_id, hostname
        );

        // Step 1: Install Tailscale
        let install_script = r#"
            set -euo pipefail
            if command -v tailscale >/dev/null 2>&1; then
                echo "tailscale_already_installed"
                exit 0
            fi
            curl -fsSL https://tailscale.com/install.sh | sh
            echo "tailscale_installed"
        "#;

        let output = remote.ssh(
            install_script,
            Duration::from_secs(TAILSCALE_INSTALL_TIMEOUT_SECS),
        )?;

        info!(
            "Tailscale remote install output (instance {}): {}",
            instance_id,
            output.stdout.trim()
        );

        // Step 2: Authenticate and bring up Tailscale
        let auth_script = format!(
            r#"
            set -euo pipefail
            tailscale up --authkey="{}" --hostname="{}" --accept-routes=false --ssh=false
            echo "tailscale_up_complete"
        "#,
            api_key, hostname
        );

        let output = remote.ssh(&auth_script, Duration::from_secs(TAILSCALE_UP_TIMEOUT_SECS))?;

        info!(
            "Tailscale remote auth output (instance {}): {}",
            instance_id,
            output.stdout.trim()
        );

        // Step 3: Wait for Tailscale to connect and get an IP
        let remote_ip = Self::wait_for_remote_ip(remote, instance_id)?;

        info!(
            "Tailscale remote IP for instance {}: {}",
            instance_id, remote_ip
        );

        Ok(TailscaleProvisionResult {
            remote_tailscale_ip: remote_ip,
        })
    }

    /// Poll the remote machine for its Tailscale IPv4 address.
    fn wait_for_remote_ip(remote: &RemoteExec, instance_id: u64) -> AppResult<String> {
        let script = "tailscale ip -4 2>/dev/null || echo \"\"";

        for attempt in 1..=TAILSCALE_IP_POLL_MAX_ATTEMPTS {
            match remote.ssh(script, Duration::from_secs(10)) {
                Ok(output) => {
                    let ip = output.stdout.trim().to_string();
                    if !ip.is_empty() && Self::is_valid_ipv4(&ip) {
                        return Ok(ip);
                    }
                    info!(
                        "Waiting for Tailscale IP (attempt {}/{}): got '{}'",
                        attempt, TAILSCALE_IP_POLL_MAX_ATTEMPTS, ip
                    );
                }
                Err(err) => {
                    warn!(
                        "Tailscale IP check attempt {}/{} failed for instance {}: {}",
                        attempt, TAILSCALE_IP_POLL_MAX_ATTEMPTS, instance_id, err
                    );
                }
            }

            std::thread::sleep(Duration::from_secs(TAILSCALE_IP_POLL_INTERVAL_SECS));
        }

        Err(AppError::Provisioning(format!(
            "Tailscale did not obtain an IP address for instance {} after {} attempts",
            instance_id, TAILSCALE_IP_POLL_MAX_ATTEMPTS
        )))
    }

    /// Install and authenticate Tailscale on the local machine.
    pub async fn provision_local(api_key: &str) -> AppResult<()> {
        let hostname = std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "noland-local".to_string());

        info!(
            "Installing Tailscale on local machine (hostname={})",
            hostname
        );

        let os = std::env::consts::OS;

        // Step 1: Install Tailscale if not present
        if !Self::local_tailscale_installed().await {
            Self::install_local_tailscale(os).await?;
        }

        // Step 2: Authenticate
        let status = TokioCommand::new("tailscale")
            .args([
                "up",
                "--authkey",
                api_key,
                "--hostname",
                &hostname,
                "--accept-routes=false",
            ])
            .output()
            .await
            .map_err(|err| {
                AppError::Command(format!("Failed to run tailscale up locally: {}", err))
            })?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(AppError::Command(format!(
                "tailscale up failed: {}",
                stderr.trim()
            )));
        }

        info!("Tailscale local authentication successful");
        Ok(())
    }

    /// Check if tailscale CLI is installed locally.
    async fn local_tailscale_installed() -> bool {
        let which_result = if cfg!(target_os = "windows") {
            TokioCommand::new("where").arg("tailscale").output().await
        } else {
            TokioCommand::new("which").arg("tailscale").output().await
        };

        match which_result {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    /// Install Tailscale on the local machine.
    async fn install_local_tailscale(os: &str) -> AppResult<()> {
        match os {
            "macos" => {
                let status = TokioCommand::new("brew")
                    .args(["install", "tailscale"])
                    .output()
                    .await
                    .map_err(|err| {
                        AppError::Command(format!(
                            "Failed to install Tailscale via Homebrew: {}",
                            err
                        ))
                    })?;

                if !status.status.success() {
                    let stderr = String::from_utf8_lossy(&status.stderr);
                    return Err(AppError::Command(format!(
                        "brew install tailscale failed: {}",
                        stderr.trim()
                    )));
                }
            }
            "linux" => {
                let status = TokioCommand::new("sh")
                    .arg("-c")
                    .arg("curl -fsSL https://tailscale.com/install.sh | sh")
                    .output()
                    .await
                    .map_err(|err| {
                        AppError::Command(format!(
                            "Failed to run Tailscale install script: {}",
                            err
                        ))
                    })?;

                if !status.status.success() {
                    let stderr = String::from_utf8_lossy(&status.stderr);
                    return Err(AppError::Command(format!(
                        "Tailscale install script failed: {}",
                        stderr.trim()
                    )));
                }
            }
            "windows" => {
                let status = TokioCommand::new("winget")
                    .args(["install", "--id", "tailscale.tailscale", "--silent"])
                    .output()
                    .await
                    .map_err(|err| {
                        AppError::Command(format!(
                            "Failed to install Tailscale via winget: {}",
                            err
                        ))
                    })?;

                if !status.status.success() {
                    let stderr = String::from_utf8_lossy(&status.stderr);
                    return Err(AppError::Command(format!(
                        "winget install tailscale failed: {}",
                        stderr.trim()
                    )));
                }
            }
            _ => {
                return Err(AppError::Command(format!(
                    "Unsupported OS for local Tailscale installation: {}",
                    os
                )));
            }
        }

        // Give it a moment to register
        std::thread::sleep(Duration::from_secs(3));
        Ok(())
    }

    /// Verify Tailscale connectivity between local and remote.
    pub async fn verify_connectivity(remote_ip: &str) -> AppResult<bool> {
        let output = TokioCommand::new("tailscale")
            .args(["ping", "--c", "1", remote_ip])
            .output()
            .await
            .map_err(|err| AppError::Command(format!("tailscale ping failed: {}", err)))?;

        Ok(output.status.success())
    }

    /// Get the local machine's Tailscale IPv4 address.
    pub async fn get_local_ip() -> AppResult<String> {
        let output = TokioCommand::new("tailscale")
            .args(["ip", "-4"])
            .output()
            .await
            .map_err(|err| AppError::Command(format!("tailscale ip failed: {}", err)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::Command(format!(
                "tailscale ip command failed: {}",
                stderr.trim()
            )));
        }

        let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !Self::is_valid_ipv4(&ip) {
            return Err(AppError::Command(format!(
                "Unexpected tailscale IP output: '{}'",
                ip
            )));
        }

        Ok(ip)
    }

    fn is_valid_ipv4(s: &str) -> bool {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 4 {
            return false;
        }
        parts.iter().all(|p| {
            p.parse::<u8>().is_ok() && (p.len() == 1 || (!p.starts_with('0') && p.len() <= 3))
        })
    }
}

/// Remove Tailscale from the remote instance (for cleanup).
pub fn remove_remote_tailscale(remote: &RemoteExec) -> AppResult<()> {
    let script = r#"
        set -euo pipefail
        if command -v tailscale >/dev/null 2>&1; then
            tailscale logout 2>/dev/null || true
            systemctl stop tailscaled 2>/dev/null || true
            systemctl disable tailscaled 2>/dev/null || true
        fi
    "#;

    remote.ssh(script, Duration::from_secs(30))?;
    Ok(())
}
