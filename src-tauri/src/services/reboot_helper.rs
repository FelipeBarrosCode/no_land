use std::time::Duration;

use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

pub struct RebootHelperService;

impl RebootHelperService {
    pub async fn reboot_and_reinitialize(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<String> {
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

        Self::wait_for_reboot_disconnect(remote).await?;
        Self::wait_for_reboot_reconnect(remote).await?;
        Self::wait_for_system_ready(remote).await?;

        info!(
            event = "instance_reboot_success",
            target_user = target_user,
            "Reboot helper completed successfully"
        );

        Ok("Instance reboot completed and services are back online".to_string())
    }

    async fn wait_for_reboot_disconnect(remote: &RemoteExec) -> AppResult<()> {
        const DISCONNECT_ATTEMPTS: usize = 20;
        const DISCONNECT_INTERVAL: Duration = Duration::from_secs(2);

        for attempt in 1..=DISCONNECT_ATTEMPTS {
            match Self::probe_ssh(
                remote,
                "echo reboot-disconnect-probe",
                Duration::from_secs(8),
            )
            .await
            {
                Ok(probe) => {
                    if probe.status_code != 0 || looks_like_reboot_disconnect(&probe.stderr) {
                        info!(
                            attempt = attempt,
                            "Observed SSH disconnect after reboot trigger"
                        );
                        return Ok(());
                    }
                }
                Err(error) => {
                    info!(
                        attempt = attempt,
                        error = %error,
                        "Treating SSH probe failure as reboot disconnect"
                    );
                    return Ok(());
                }
            }

            sleep(DISCONNECT_INTERVAL).await;
        }

        warn!(
            "Did not observe an SSH disconnect after scheduling reboot; continuing to reconnect wait"
        );
        Ok(())
    }

    async fn wait_for_reboot_reconnect(remote: &RemoteExec) -> AppResult<()> {
        const RECONNECT_ATTEMPTS: usize = 36;
        const RECONNECT_INTERVAL: Duration = Duration::from_secs(10);

        for attempt in 1..=RECONNECT_ATTEMPTS {
            match Self::probe_ssh(remote, "echo reboot-online", Duration::from_secs(15)).await {
                Ok(probe) => {
                    if probe.status_code == 0 {
                        info!(attempt = attempt, "SSH is back online after reboot");
                        return Ok(());
                    }

                    warn!(
                        attempt = attempt,
                        status_code = probe.status_code,
                        stderr = %filtered_probe_stderr(&probe.stderr),
                        "Waiting for SSH to return after reboot"
                    );
                }
                Err(error) => {
                    warn!(
                        attempt = attempt,
                        error = %error,
                        "Waiting for SSH to return after reboot"
                    );
                }
            }
            sleep(RECONNECT_INTERVAL).await;
        }

        Err(AppError::Timeout(
            "Timed out waiting for the instance to reconnect after reboot.".to_string(),
        ))
    }

    async fn wait_for_system_ready(remote: &RemoteExec) -> AppResult<()> {
        const SYSTEM_STATE_ATTEMPTS: usize = 30;
        const SYSTEM_STATE_INTERVAL: Duration = Duration::from_secs(2);

        for attempt in 1..=SYSTEM_STATE_ATTEMPTS {
            match Self::probe_ssh(
                remote,
                "systemctl is-system-running 2>/dev/null",
                Duration::from_secs(10),
            )
            .await
            {
                Ok(state) => {
                    let stdout = state.stdout.trim();
                    if is_ready_system_state(stdout) {
                        info!(
                            attempt = attempt,
                            system_state = stdout,
                            status_code = state.status_code,
                            "Systemd reached a ready state"
                        );
                        return Ok(());
                    }

                    warn!(
                        attempt = attempt,
                        system_state = stdout,
                        status_code = state.status_code,
                        stderr = %filtered_probe_stderr(&state.stderr),
                        "Waiting for systemd to finish booting after reboot"
                    );
                }
                Err(error) => {
                    warn!(
                        attempt = attempt,
                        error = %error,
                        "Waiting for systemd to finish booting after reboot"
                    );
                }
            }
            sleep(SYSTEM_STATE_INTERVAL).await;
        }

        let failed_units = Self::collect_failed_units(remote).await;

        Err(AppError::Timeout(format!(
            "SSH came back after reboot, but the system never reached a ready state. Failed units: {}",
            failed_units.trim()
        )))
    }

    async fn collect_failed_units(remote: &RemoteExec) -> String {
        match Self::probe_ssh(
            remote,
            "systemctl --failed --no-pager --plain 2>/dev/null || true",
            Duration::from_secs(10),
        )
        .await
        {
            Ok(output) => {
                let trimmed = output.stdout.trim();
                if trimmed.is_empty() {
                    "<none>".to_string()
                } else {
                    trimmed.to_string()
                }
            }
            Err(error) => format!("<unavailable: {error}>"),
        }
    }

    async fn probe_ssh(
        remote: &RemoteExec,
        command: &str,
        timeout: Duration,
    ) -> AppResult<crate::services::remote_exec::ExecOutput> {
        let remote = remote.clone();
        let command = command.to_string();
        tokio::task::spawn_blocking(move || remote.ssh(&command, timeout))
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))?
    }
}

fn is_ready_system_state(stdout: &str) -> bool {
    matches!(stdout.trim(), "running" | "degraded")
}

fn filtered_probe_stderr(stderr: &str) -> String {
    let filtered = stderr
        .lines()
        .filter(|line| !(line.contains("Permanently added") && line.contains("known hosts")))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if filtered.is_empty() {
        stderr.trim().to_string()
    } else {
        filtered
    }
}

fn looks_like_reboot_disconnect(stderr: &str) -> bool {
    let normalized = filtered_probe_stderr(stderr).to_ascii_lowercase();
    normalized.contains("connection refused")
        || normalized.contains("connection reset")
        || normalized.contains("connection timed out")
        || normalized.contains("broken pipe")
        || normalized.contains("no route to host")
        || normalized.contains("operation timed out")
}

#[cfg(test)]
mod tests {
    use super::{filtered_probe_stderr, is_ready_system_state};

    #[test]
    fn ready_states_allow_running_and_degraded() {
        assert!(is_ready_system_state("running"));
        assert!(is_ready_system_state("degraded"));
        assert!(!is_ready_system_state("starting"));
        assert!(!is_ready_system_state("maintenance"));
    }

    #[test]
    fn known_hosts_warning_is_filtered_from_probe_stderr() {
        let stderr = "Warning: Permanently added '[1.2.3.4]:22' (ED25519) to the list of known hosts.\nConnection refused";
        assert_eq!(filtered_probe_stderr(stderr), "Connection refused");
    }
}
