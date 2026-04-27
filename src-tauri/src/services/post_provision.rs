use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

const POST_PROVISION_SCRIPT: &str = include_str!("../../scripts/post_provision.sh");

pub struct PostProvisionService;

impl PostProvisionService {
    pub async fn run(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let encoded_script = STANDARD.encode(POST_PROVISION_SCRIPT.as_bytes());
        let command = format!(
            "sudo bash -lc 'set -euo pipefail; base64 -d > /tmp/noland-post-provision.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/noland-post-provision.sh\n/tmp/noland-post-provision.sh {}'",
            encoded_script,
            shell_single_quote(target_user),
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(1800)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Post-provision setup failed: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        Ok(output.stdout)
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
