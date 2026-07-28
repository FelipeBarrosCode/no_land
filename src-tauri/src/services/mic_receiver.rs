use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::remote_exec::RemoteExec;

const INSTALL_SCRIPT: &str = include_str!("../../scripts/install_cloud_mic_agent.sh");

pub struct MicReceiverProvisioner;

impl MicReceiverProvisioner {
    /// Install and start the Noland microphone receiver on the remote VM.
    pub async fn install(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let normalized_script = INSTALL_SCRIPT.replace("\r\n", "\n").replace('\r', "\n");
        let encoded = STANDARD.encode(normalized_script.as_bytes());
        let safe_user = sanitize_username(target_user)?;

        info!(
            target_user = safe_user,
            "Installing Noland microphone receiver"
        );

        let command = format!(
            "sudo bash -lc 'set -euo pipefail; base64 -d > /tmp/noland-mic-install.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/noland-mic-install.sh\n/tmp/noland-mic-install.sh \"{}\"\nrm -f /tmp/noland-mic-install.sh'",
            encoded, safe_user,
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&command, Duration::from_secs(300)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        let stdout = output.stdout.trim();
        let stderr = output.stderr.trim();

        if !stdout.is_empty() {
            info!(
                target_user = safe_user,
                "Mic receiver install stdout:\n{stdout}"
            );
        }
        if !stderr.is_empty() {
            warn!(
                target_user = safe_user,
                "Mic receiver install stderr:\n{stderr}"
            );
        }

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Mic receiver install failed: {stderr} {stdout}"
            )));
        }

        info!(
            target_user = safe_user,
            "Mic receiver installed successfully"
        );
        Ok(stdout.to_string())
    }
}

fn sanitize_username(value: &str) -> AppResult<&str> {
    if value.is_empty() {
        return Err(AppError::Provisioning("Target user is empty".to_string()));
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(value)
    } else {
        Err(AppError::Provisioning(format!(
            "Invalid target user '{value}': only letters, numbers, '-' and '_' are allowed"
        )))
    }
}
