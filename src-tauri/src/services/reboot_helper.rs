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
            "sudo bash -lc 'set -euo pipefail; TARGET_USER=\"{target_user}\"; TARGET_UID=$(id -u \"$TARGET_USER\"); TARGET_HOME=$(getent passwd \"$TARGET_USER\" | cut -d: -f6); RUNTIME_DIR=\"/run/user/$TARGET_UID\"; USER_XAUTH=\"$TARGET_HOME/.Xauthority\"; if ! id \"$TARGET_USER\" >/dev/null 2>&1; then echo \"Target user not found: $TARGET_USER\" >&2; exit 2; fi; if [ -z \"$TARGET_HOME\" ]; then echo \"Could not resolve home for $TARGET_USER\" >&2; exit 2; fi; systemctl daemon-reload; systemctl restart docker || true; systemctl restart tailscaled || true; systemctl restart ssh || systemctl restart sshd || true; systemctl restart cron || true; systemctl restart NetworkManager || true; mkdir -p \"$RUNTIME_DIR\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$RUNTIME_DIR\"; chmod 700 \"$RUNTIME_DIR\"; mkdir -p \"$TARGET_HOME/.config/sunshine\"; if [ -f \"$TARGET_HOME/.config/sunshine/sunshine.conf\" ]; then if grep -q \"^output_name\" \"$TARGET_HOME/.config/sunshine/sunshine.conf\"; then sed -i \"s/^output_name.*/output_name = HDMI-0/\" \"$TARGET_HOME/.config/sunshine/sunshine.conf\"; else echo \"output_name = HDMI-0\" >> \"$TARGET_HOME/.config/sunshine/sunshine.conf\"; fi; fi; touch \"$USER_XAUTH\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$USER_XAUTH\"; chmod 600 \"$USER_XAUTH\"; rm -f \"$RUNTIME_DIR/.Xauthority\" 2>/dev/null || true; echo \"REBOOT_DISPLAY_PATH display=:0 xauthority=$USER_XAUTH output=HDMI-0\"; ls -l \"$USER_XAUTH\" || true; sudo -u \"$TARGET_USER\" DISPLAY=:0 XAUTHORITY=\"$USER_XAUTH\" xrandr --listmonitors 2>/dev/null || true; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user daemon-reload || true; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user restart pipewire pipewire-pulse wireplumber || true; systemctl restart sunshine || true; if [ -x /tmp/noland-post-provision.sh ]; then ENABLE_GITHUB_GAME_COMPAT=0 /tmp/noland-post-provision.sh \"$TARGET_USER\" || true; fi; systemctl --failed || true; sync; nohup sh -c \"sleep 3; reboot\" >/dev/null 2>&1 & echo REBOOT_SCHEDULED'"
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
        Self::ensure_audio_ready_after_reboot(remote, target_user).await?;
        Self::recover_sunshine_after_reboot(remote, target_user).await?;

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

    async fn recover_sunshine_after_reboot(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        Self::wait_for_user_display_ready(remote, target_user).await?;

        let target_home = Self::resolve_user_home(remote, target_user).await?;
        let restart_command = format!(
            "sudo bash -lc 'TARGET_USER=\"{target_user}\"; TARGET_HOME=\"{target_home}\"; USER_XAUTH=\"$TARGET_HOME/.Xauthority\"; systemctl restart sunshine; sleep 2; PROC_COUNT=$(pgrep -u \"$TARGET_USER\" -x sunshine 2>/dev/null | wc -l | tr -d \" \\\t\"); if [ \"$PROC_COUNT\" != \"1\" ]; then echo SUNSHINE_POST_REBOOT_FAIL; echo \"PROC_COUNT=$PROC_COUNT\"; systemctl status sunshine --no-pager 2>/dev/null || true; exit 1; fi; sudo -u \"$TARGET_USER\" env DISPLAY=:0 XAUTHORITY=\"$USER_XAUTH\" xrandr --listmonitors >/dev/null 2>&1 || {{ echo SUNSHINE_POST_REBOOT_FAIL; echo USER_DISPLAY_NOT_READY; systemctl status sunshine --no-pager 2>/dev/null || true; exit 1; }}; WEB_OK=0; for i in $(seq 1 20); do if curl -k -s --connect-timeout 5 https://localhost:47990/pin >/dev/null 2>&1; then WEB_OK=1; break; fi; sleep 1; done; if [ \"$WEB_OK\" != \"1\" ]; then echo SUNSHINE_POST_REBOOT_FAIL; echo WEB_UI_NOT_READY; systemctl status sunshine --no-pager 2>/dev/null || true; journalctl -u sunshine --no-pager -n 80 2>/dev/null || true; exit 1; fi; if journalctl -u sunshine --no-pager -n 80 2>/dev/null | grep -q \"Unable to open display\"; then echo SUNSHINE_POST_REBOOT_FAIL; echo OPEN_DISPLAY_ERROR_IN_JOURNAL; journalctl -u sunshine --no-pager -n 80 2>/dev/null || true; exit 1; fi; echo SUNSHINE_POST_REBOOT_OK'"
        );

        let output = Self::probe_ssh(remote, &restart_command, Duration::from_secs(40)).await?;
        if output.status_code != 0 || !output.stdout.contains("SUNSHINE_POST_REBOOT_OK") {
            return Err(AppError::Provisioning(format!(
                "Sunshine failed post-reboot recovery using DISPLAY=:0 and XAUTHORITY=/home/{}/.Xauthority. stdout: {} | stderr: {}",
                target_user,
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        info!(
            target_user = target_user,
            "Sunshine recovered after reboot with single display path"
        );
        Ok(())
    }

    async fn ensure_audio_ready_after_reboot(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let target_uid = Self::resolve_user_uid(remote, target_user).await?;
        let runtime_dir = format!("/run/user/{target_uid}");

        let check_command = format!(
            "sudo bash -lc 'TARGET_USER=\"{target_user}\"; RUNTIME_DIR=\"{runtime_dir}\"; mkdir -p \"$RUNTIME_DIR\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$RUNTIME_DIR\"; chmod 700 \"$RUNTIME_DIR\"; PW=$(sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user is-active pipewire 2>/dev/null || true); PWP=$(sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user is-active pipewire-pulse 2>/dev/null || true); WP=$(sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user is-active wireplumber 2>/dev/null || true); SINK_OK=0; if sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" pactl list short sinks 2>/dev/null | grep -q \"sunshine_audio\"; then SINK_OK=1; fi; if [ \"$PW\" = \"active\" ] && [ \"$PWP\" = \"active\" ] && [ \"$WP\" = \"active\" ] && [ \"$SINK_OK\" = \"1\" ]; then echo AUDIO_READY; else echo AUDIO_NOT_READY; echo \"pipewire=$PW\"; echo \"pipewire_pulse=$PWP\"; echo \"wireplumber=$WP\"; echo \"sunshine_audio_sink=$SINK_OK\"; fi'"
        );

        let first = Self::probe_ssh(remote, &check_command, Duration::from_secs(20)).await?;
        if first.status_code == 0 && first.stdout.contains("AUDIO_READY") {
            info!(
                target_user = target_user,
                "Post-reboot audio stack is ready"
            );
            return Ok(());
        }

        warn!(
            target_user = target_user,
            stdout = %first.stdout.trim(),
            stderr = %filtered_probe_stderr(&first.stderr),
            "Post-reboot audio stack not ready; attempting one recovery pass"
        );

        let repair_command = format!(
            "sudo bash -lc 'TARGET_USER=\"{target_user}\"; RUNTIME_DIR=\"{runtime_dir}\"; TARGET_HOME=$(getent passwd \"$TARGET_USER\" | cut -d: -f6); mkdir -p \"$RUNTIME_DIR\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$RUNTIME_DIR\"; chmod 700 \"$RUNTIME_DIR\"; sudo -u \"$TARGET_USER\" mkdir -p \"$TARGET_HOME/.config/pipewire/pipewire-pulse.conf.d\"; sudo -u \"$TARGET_USER\" bash -lc \"cat > '$TARGET_HOME/.config/pipewire/pipewire-pulse.conf.d/20-sunshine-audio.conf' <<'EOF'\npulse.cmd = [\n  {{\n    cmd = \\\"load-module\\\"\n    args = \\\"module-null-sink sink_name=sunshine_audio sink_properties=device.description=sunshine_audio\\\"\n    flags = [ \\\"nofail\\\" ]\n  }}\n]\nEOF\"; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user daemon-reload || true; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user restart pipewire pipewire-pulse wireplumber || true; sleep 3; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" pactl set-default-sink sunshine_audio 2>/dev/null || true'"
        );
        let _ = Self::probe_ssh(remote, &repair_command, Duration::from_secs(25)).await?;

        let second = Self::probe_ssh(remote, &check_command, Duration::from_secs(20)).await?;
        if second.status_code == 0 && second.stdout.contains("AUDIO_READY") {
            info!(
                target_user = target_user,
                "Post-reboot audio stack recovered successfully"
            );
            return Ok(());
        }

        let diag_command = format!(
            "sudo bash -lc 'TARGET_USER=\"{target_user}\"; RUNTIME_DIR=\"{runtime_dir}\"; echo --- user-audio-status ---; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" systemctl --user status pipewire pipewire-pulse wireplumber --no-pager 2>/dev/null || true; echo --- sinks ---; sudo -u \"$TARGET_USER\" XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" pactl list short sinks 2>/dev/null || true'"
        );
        let diag = Self::probe_ssh(remote, &diag_command, Duration::from_secs(20)).await?;

        Err(AppError::Provisioning(format!(
            "Post-reboot audio recovery failed: sunshine_audio sink missing or audio services inactive. check1: {} | check2: {} | diagnostics: {} | stderr: {}",
            first.stdout.trim(),
            second.stdout.trim(),
            diag.stdout.trim(),
            filtered_probe_stderr(&diag.stderr)
        )))
    }

    async fn wait_for_user_display_ready(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        const DISPLAY_ATTEMPTS: usize = 30;
        const DISPLAY_INTERVAL: Duration = Duration::from_secs(2);
        let target_home = Self::resolve_user_home(remote, target_user).await?;

        for attempt in 1..=DISPLAY_ATTEMPTS {
            let command = format!(
                "sudo bash -lc 'TARGET_USER=\"{target_user}\"; TARGET_HOME=\"{target_home}\"; USER_XAUTH=\"$TARGET_HOME/.Xauthority\"; test -f \"$USER_XAUTH\" || exit 1; sudo -u \"$TARGET_USER\" env DISPLAY=:0 XAUTHORITY=\"$USER_XAUTH\" xrandr --listmonitors >/dev/null 2>&1'"
            );

            match Self::probe_ssh(remote, &command, Duration::from_secs(10)).await {
                Ok(output) if output.status_code == 0 => {
                    info!(
                        attempt = attempt,
                        target_user = target_user,
                        "User display became ready after reboot"
                    );
                    return Ok(());
                }
                Ok(output) => {
                    warn!(
                        attempt = attempt,
                        target_user = target_user,
                        status_code = output.status_code,
                        stderr = %filtered_probe_stderr(&output.stderr),
                        "Waiting for user display path to become ready after reboot"
                    );
                }
                Err(error) => {
                    warn!(
                        attempt = attempt,
                        target_user = target_user,
                        error = %error,
                        "Waiting for user display path to become ready after reboot"
                    );
                }
            }
            sleep(DISPLAY_INTERVAL).await;
        }

        Err(AppError::Timeout(format!(
            "User display did not become ready after reboot using DISPLAY=:0 and XAUTHORITY=/home/{}/.Xauthority",
            target_user
        )))
    }

    async fn resolve_user_home(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let command = format!("getent passwd {} | cut -d: -f6", target_user);
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(10)).await?;
        if output.status_code == 0 {
            let home = output.stdout.trim();
            if !home.is_empty() {
                return Ok(home.to_string());
            }
        }
        Err(AppError::Provisioning(format!(
            "Could not resolve home directory for user {}",
            target_user
        )))
    }

    async fn resolve_user_uid(remote: &RemoteExec, target_user: &str) -> AppResult<u32> {
        let command = format!("id -u {}", target_user);
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(10)).await?;
        if output.status_code == 0 {
            return output.stdout.trim().parse::<u32>().map_err(|error| {
                AppError::Provisioning(format!(
                    "Failed to parse UID for {}: {}",
                    target_user, error
                ))
            });
        }
        Err(AppError::Provisioning(format!(
            "Could not resolve UID for user {}",
            target_user
        )))
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
