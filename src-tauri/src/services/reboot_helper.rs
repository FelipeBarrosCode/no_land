use std::time::Duration;

use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

pub struct RebootHelperService;

impl RebootHelperService {
    pub async fn reboot_and_reinitialize(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let script = format!(
            "sudo bash -lc 'set -euo pipefail; TARGET_USER=\"{target_user}\"; TARGET_UID=$(id -u \"$TARGET_USER\"); RUNTIME_DIR=\"/run/user/$TARGET_UID\"; if ! id \"$TARGET_USER\" >/dev/null 2>&1; then echo \"Target user not found: $TARGET_USER\" >&2; exit 2; fi; systemctl daemon-reload; systemctl restart docker || true; systemctl restart tailscaled || true; systemctl restart ssh || systemctl restart sshd || true; systemctl restart cron || true; systemctl restart NetworkManager || true; mkdir -p \"$RUNTIME_DIR\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$RUNTIME_DIR\"; chmod 700 \"$RUNTIME_DIR\"; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user daemon-reload || true; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user restart pipewire pipewire-pulse wireplumber || true; systemctl restart sunshine || true; if [ -x /tmp/noland-post-provision.sh ]; then ENABLE_GITHUB_GAME_COMPAT=0 /tmp/noland-post-provision.sh \"$TARGET_USER\" || true; fi; systemctl --failed || true; sync; nohup sh -c \"sleep 3; reboot\" >/dev/null 2>&1 & echo REBOOT_SCHEDULED'"
        );

        info!(
            event = "instance_reboot_start",
            target_user = target_user,
            "Reboot helper started"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&script, Duration::from_secs(300)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        let stdout = output.stdout.trim();
        let stderr = output.stderr.trim();

        info!(
            event = "instance_reboot_output",
            target_user = target_user,
            status_code = output.status_code,
            stdout = %stdout,
            stderr = %stderr,
            "Reboot helper command output"
        );

        if output.status_code != 0 || !output.stdout.contains("REBOOT_SCHEDULED") {
            warn!(
                event = "instance_reboot_failure",
                target_user = target_user,
                status_code = output.status_code,
                "Reboot helper failed"
            );
            return Err(AppError::Provisioning(format!(
                "Reboot helper failed: stdout: {} | stderr: {}",
                stdout, stderr
            )));
        }

        info!(
            event = "instance_reboot_success",
            target_user = target_user,
            "Reboot helper scheduled successfully"
        );

        Ok("Reboot and service reinitialization scheduled".to_string())
    }
}
