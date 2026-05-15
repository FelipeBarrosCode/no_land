use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

const POST_PROVISION_SCRIPT: &str = include_str!("../../scripts/post_provision.sh");

pub struct PostProvisionService;

impl PostProvisionService {
    pub async fn run(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let encoded_script = STANDARD.encode(POST_PROVISION_SCRIPT.as_bytes());
        let safe_user = sanitize_username(target_user)?;
        info!(
            event = "post_provision_start",
            target_user = safe_user,
            "Post-provision started"
        );
        let command = format!(
            "sudo bash -lc 'set -euo pipefail; base64 -d > /tmp/noland-post-provision.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/noland-post-provision.sh\n/tmp/noland-post-provision.sh {}\nrm -f /tmp/noland-post-provision.sh'",
            encoded_script,
            safe_user,
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(1800)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        let stdout = output.stdout.trim();
        let stderr = output.stderr.trim();

        if !stdout.is_empty() {
            info!(
                target_user = safe_user,
                "post-provision stdout:\n{}", stdout
            );
        }
        if !stderr.is_empty() {
            warn!(
                target_user = safe_user,
                "post-provision stderr:\n{}", stderr
            );
        }

        if output.status_code != 0 {
            warn!(
                event = "post_provision_failure",
                target_user = safe_user,
                status_code = output.status_code,
                "Post-provision failed"
            );
            return Err(AppError::Provisioning(format!(
                "Post-provision setup failed: {} {}",
                stderr, stdout
            )));
        }

        info!(
            event = "post_provision_success",
            target_user = safe_user,
            status_code = output.status_code,
            "Post-provision completed successfully"
        );

        Ok(output.stdout)
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
            "Invalid target user '{}': only letters, numbers, '-' and '_' are allowed",
            value
        )))
    }
}
