use std::time::Duration;

use tokio::{sync::watch, time::sleep};
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

pub struct RebootHelperService;

impl RebootHelperService {
    pub async fn reboot_and_reinitialize_with_endpoint_updates(
        remote: &RemoteExec,
        target_user: &str,
        endpoint_updates: watch::Receiver<RemoteExec>,
    ) -> AppResult<String> {
        Self::reboot_and_reinitialize_internal(remote, target_user, Some(endpoint_updates)).await
    }

    async fn reboot_and_reinitialize_internal(
        remote: &RemoteExec,
        target_user: &str,
        mut endpoint_updates: Option<watch::Receiver<RemoteExec>>,
    ) -> AppResult<String> {
        info!(
            event = "instance_reboot_start",
            target_user = target_user,
            "Reboot helper started"
        );

        let old_boot_id = Self::preflight_reboot(remote, target_user).await?;
        let schedule_script =
            "set -euo pipefail; sync; nohup sh -c 'sleep 3; systemctl reboot' >/dev/null 2>&1 & echo REBOOT_SCHEDULED";
        let schedule_command = format!("sudo bash -lc {}", shell_quote(schedule_script));
        let output = Self::probe_ssh(remote, &schedule_command, Duration::from_secs(30)).await?;
        let stdout = output.stdout.trim();
        let stderr = output.stderr.trim();

        info!(
            event = "instance_reboot_output",
            target_user = target_user,
            old_boot_id = %old_boot_id,
            status_code = output.status_code,
            stdout = %stdout,
            stderr = %stderr,
            "Reboot helper scheduling output"
        );

        if output.status_code != 0
            || !output
                .stdout
                .lines()
                .any(|line| line.trim() == "REBOOT_SCHEDULED")
        {
            warn!(
                event = "instance_reboot_failure",
                target_user = target_user,
                status_code = output.status_code,
                "Reboot helper failed to schedule reboot"
            );
            return Err(AppError::Provisioning(format!(
                "Reboot helper failed to schedule reboot: stdout: {} | stderr: {}",
                stdout, stderr
            )));
        }

        Self::wait_for_reboot_disconnect(remote).await?;
        let active_remote =
            Self::wait_for_reboot_reconnect(remote, &old_boot_id, endpoint_updates.as_mut())
                .await?;
        Self::wait_for_system_ready(&active_remote).await?;
        Self::ensure_noland_xorg_after_reboot(&active_remote).await?;
        Self::ensure_display_mode_after_reboot(&active_remote).await?;
        let display_xauthority =
            Self::wait_for_user_display_ready(&active_remote, target_user).await?;
        Self::ensure_audio_ready_after_reboot(&active_remote, target_user).await?;
        Self::recover_sunshine_after_reboot(&active_remote, target_user, &display_xauthority)
            .await?;

        info!(
            event = "instance_reboot_success",
            target_user = target_user,
            "Reboot helper completed successfully"
        );

        Ok("Instance reboot completed and services are back online".to_string())
    }

    async fn preflight_reboot(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
if ! id "$TARGET_USER" >/dev/null 2>&1; then
    echo "Target user not found: $TARGET_USER" >&2
    exit 2
fi
TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
if [ -z "$TARGET_HOME" ] || [ ! -d "$TARGET_HOME" ]; then
    echo "Could not resolve an existing home for $TARGET_USER" >&2
    exit 2
fi
command -v xauth >/dev/null 2>&1 || {{ echo "xauth is required for display preflight" >&2; exit 2; }}
command -v xrandr >/dev/null 2>&1 || {{ echo "xrandr is required for display preflight" >&2; exit 2; }}
if [ "$(systemctl show sunshine.service --property=LoadState --value 2>/dev/null)" != "loaded" ]; then
    echo "Required service is not loaded: sunshine.service" >&2
    exit 2
fi
loginctl enable-linger "$TARGET_USER" 2>/dev/null || true
systemctl enable sunshine.service >/dev/null
if [ "$(systemctl show noland-xorg.service --property=LoadState --value 2>/dev/null)" = "loaded" ]; then
    systemctl mask gdm sddm lightdm 2>/dev/null || true
    systemctl enable noland-xorg.service >/dev/null
    if systemctl cat noland-desktop.service >/dev/null 2>&1; then systemctl enable noland-desktop.service >/dev/null; fi
    if systemctl cat noland-display-mode.service >/dev/null 2>&1; then systemctl enable noland-display-mode.service >/dev/null; fi
    mkdir -p /etc/systemd/system/sunshine.service.d
    if [ ! -f /etc/systemd/system/sunshine.service.d/noland-display.conf ]; then
    cat > /etc/systemd/system/sunshine.service.d/noland-display.conf <<'EOF'
[Unit]
Requires=noland-xorg.service
After=noland-xorg.service network-online.target
Wants=network-online.target

[Service]
Environment=XAUTHORITY=/etc/X11/.Xauthority-noland
EOF
    fi
fi
systemctl daemon-reload
CANONICAL_XAUTH=/etc/X11/.Xauthority-noland
USER_XAUTH="$TARGET_HOME/.Xauthority"
DISPLAY_XAUTH=""
for candidate in "$CANONICAL_XAUTH" "$USER_XAUTH"; do
    if [ ! -s "$candidate" ] || ! sudo -u "$TARGET_USER" test -r "$candidate"; then
        continue
    fi
    XAUTH_ENTRIES=$(sudo -u "$TARGET_USER" xauth -f "$candidate" list 2>/dev/null || true)
    if [ -n "$XAUTH_ENTRIES" ]; then
        DISPLAY_XAUTH="$candidate"
        break
    fi
done
if [ -z "$DISPLAY_XAUTH" ]; then
    echo "No non-empty, readable Xauthority with entries was found at $CANONICAL_XAUTH or $USER_XAUTH" >&2
    exit 2
fi
echo "REBOOT_PREFLIGHT_OK user=$TARGET_USER home=$TARGET_HOME xauthority=$DISPLAY_XAUTH""#,
            target_user = shell_quote(target_user),
        );
        let command = format!("sudo bash -lc {}", shell_quote(&script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(30)).await?;

        if output.status_code != 0 || !output.stdout.contains("REBOOT_PREFLIGHT_OK") {
            return Err(AppError::Provisioning(format!(
                "Reboot preflight failed: stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        let boot_id = "fire-and-forget".to_string();

        info!(
            target_user = target_user,
            boot_id = %boot_id,
            preflight = %output.stdout.trim(),
            "Reboot preflight passed"
        );
        Ok(boot_id)
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

    async fn wait_for_reboot_reconnect(
        remote: &RemoteExec,
        _old_boot_id: &str,
        endpoint_updates: Option<&mut watch::Receiver<RemoteExec>>,
    ) -> AppResult<RemoteExec> {
        const RECONNECT_ATTEMPTS: usize = 36;
        const RECONNECT_INTERVAL: Duration = Duration::from_secs(10);
        const BOOT_ID_COMMAND: &str = "echo REBOOT_BOOT_ID=fire-and-forget";

        let endpoint_updates = endpoint_updates;
        for attempt in 1..=RECONNECT_ATTEMPTS {
            let active_remote = endpoint_updates
                .as_ref()
                .map(|updates| updates.borrow().clone())
                .unwrap_or_else(|| remote.clone());
            match Self::probe_ssh(&active_remote, BOOT_ID_COMMAND, Duration::from_secs(15)).await {
                Ok(probe) => {
                    if probe.status_code == 0 {
                        info!(
                            attempt = attempt,
                            ssh_host = %active_remote.ssh_host,
                            "Observed successful SSH connection after reboot trigger (fire-and-forget mode)"
                        );
                        return Ok(active_remote);
                    }
                    warn!(
                        attempt = attempt,
                        status_code = probe.status_code,
                        stderr = %filtered_probe_stderr(&probe.stderr),
                        "Waiting for SSH to become fully ready..."
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
            "Timed out waiting for the instance to reconnect via SSH after reboot (fire-and-forget mode).".to_string()
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

    async fn ensure_noland_xorg_after_reboot(remote: &RemoteExec) -> AppResult<()> {
        let script = r#"set -euo pipefail
if [ "$(systemctl show noland-xorg.service --property=LoadState --value 2>/dev/null)" != "loaded" ]; then
    echo NOLAND_XORG_NOT_INSTALLED
    exit 0
fi
systemctl daemon-reload
systemctl enable noland-xorg.service
systemctl start noland-xorg.service
for attempt in $(seq 1 20); do
    if systemctl is-enabled --quiet noland-xorg.service && systemctl is-active --quiet noland-xorg.service; then
        echo NOLAND_XORG_READY
        exit 0
    fi
    sleep 1
done
echo NOLAND_XORG_NOT_READY
systemctl status noland-xorg.service --no-pager 2>/dev/null || true
journalctl -u noland-xorg.service -b --no-pager -n 80 2>/dev/null || true
tail -80 /var/log/Xorg.0.log 2>/dev/null || true
exit 1"#;
        let command = format!("sudo bash -lc {}", shell_quote(script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(35)).await?;

        if output.stdout.contains("NOLAND_XORG_NOT_INSTALLED") {
            info!("Custom Noland Xorg is not installed; preserving the host display-manager path");
            return Ok(());
        }
        if output.status_code != 0 || !output.stdout.contains("NOLAND_XORG_READY") {
            return Err(AppError::Provisioning(format!(
                "noland-xorg could not be enabled and started after reboot. stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        info!("noland-xorg is enabled and active after reboot");
        Ok(())
    }

    async fn ensure_display_mode_after_reboot(remote: &RemoteExec) -> AppResult<()> {
        let script = r#"set -euo pipefail
if ! systemctl cat noland-display-mode.service >/dev/null 2>&1; then
    echo NOLAND_DISPLAY_MODE_NOT_INSTALLED
    exit 0
fi
systemctl enable noland-display-mode.service >/dev/null
systemctl restart noland-display-mode.service
if ! systemctl is-active --quiet noland-display-mode.service; then
    systemctl status noland-display-mode.service --no-pager 2>/dev/null || true
    journalctl -u noland-display-mode.service -b --no-pager -n 80 2>/dev/null || true
    exit 1
fi
echo NOLAND_DISPLAY_MODE_READY"#;
        let command = format!("sudo bash -lc {}", shell_quote(script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(50)).await?;
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "The selected display mode could not be restored after reboot. stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }
        info!("Persistent display mode restored after reboot");
        Ok(())
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
        display_xauthority: &str,
    ) -> AppResult<()> {
        let script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
DISPLAY_XAUTH={display_xauthority}
if [ ! -s "$DISPLAY_XAUTH" ]; then
    echo SUNSHINE_POST_REBOOT_FAIL
    echo "XAUTHORITY_MISSING_OR_EMPTY=$DISPLAY_XAUTH"
    exit 1
fi
if ! sudo -u "$TARGET_USER" env DISPLAY=:0 XAUTHORITY="$DISPLAY_XAUTH" xrandr --listmonitors >/dev/null 2>&1; then
    echo SUNSHINE_POST_REBOOT_FAIL
    echo "DISPLAY_NOT_READY xauthority=$DISPLAY_XAUTH"
    exit 1
fi
systemctl enable sunshine.service >/dev/null
systemctl restart sunshine.service
sleep 2
WEB_OK=0
RTSP_OK=0
for attempt in $(seq 1 30); do
    if curl -k -s --connect-timeout 5 https://localhost:47990/pin >/dev/null 2>&1 || curl -k -s --connect-timeout 5 https://127.0.0.1:47990/pin >/dev/null 2>&1; then
        WEB_OK=1
    fi
    if ss -ltn 2>/dev/null | grep -Eq ':48010[[:space:]]'; then
        RTSP_OK=1
    fi
    if [ "$WEB_OK" = "1" ] && [ "$RTSP_OK" = "1" ]; then
        break
    fi
    sleep 1
done
PROC_COUNT=$(pgrep -u "$TARGET_USER" -x sunshine 2>/dev/null | wc -l | tr -d '[:space:]' || true)
INVOCATION_ID=$(systemctl show sunshine.service --property=InvocationID --value 2>/dev/null || true)
if [ -n "$INVOCATION_ID" ]; then
    CURRENT_LOGS=$(journalctl "_SYSTEMD_INVOCATION_ID=$INVOCATION_ID" --no-pager -n 120 2>/dev/null || true)
else
    CURRENT_LOGS=$(journalctl -u sunshine.service -b --no-pager -n 120 2>/dev/null || true)
fi
if [ "$PROC_COUNT" != "1" ] || [ "$WEB_OK" != "1" ] || [ "$RTSP_OK" != "1" ] || ! systemctl is-active --quiet sunshine.service; then
    echo SUNSHINE_POST_REBOOT_FAIL
    echo "PROC_COUNT=$PROC_COUNT WEB_OK=$WEB_OK RTSP_OK=$RTSP_OK"
    systemctl status sunshine.service --no-pager 2>/dev/null || true
    printf '%s\n' "$CURRENT_LOGS"
    echo '--- listening ports ---'
    ss -ltnp 2>/dev/null | grep -E ':(47990|48010)[[:space:]]' || true
    exit 1
fi
if printf '%s\n' "$CURRENT_LOGS" | grep -Eqi 'Unable to open display|Failed to (open|create).*display|Could not open.*display'; then
    echo SUNSHINE_POST_REBOOT_FAIL
    echo OPEN_DISPLAY_ERROR_IN_CURRENT_INVOCATION
    printf '%s\n' "$CURRENT_LOGS"
    exit 1
fi
echo "SUNSHINE_POST_REBOOT_OK web=47990 rtsp=48010 xauthority=$DISPLAY_XAUTH""#,
            target_user = shell_quote(target_user),
            display_xauthority = shell_quote(display_xauthority),
        );
        let restart_command = format!("sudo bash -lc {}", shell_quote(&script));
        let output = Self::probe_ssh(remote, &restart_command, Duration::from_secs(90)).await?;
        if output.status_code != 0 || !output.stdout.contains("SUNSHINE_POST_REBOOT_OK") {
            return Err(AppError::Provisioning(format!(
                "Sunshine failed post-reboot recovery using DISPLAY=:0 and XAUTHORITY={}. stdout: {} | stderr: {}",
                display_xauthority,
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        info!(
            target_user = target_user,
            display_xauthority = display_xauthority,
            "Sunshine recovered after reboot; web and RTSP listeners are ready"
        );
        Ok(())
    }

    async fn ensure_audio_ready_after_reboot(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let target_uid = Self::resolve_user_uid(remote, target_user).await?;
        let runtime_dir = format!("/run/user/{target_uid}");
        let bus_path = format!("{runtime_dir}/bus");

        let check_command = format!(
            "sudo bash -lc 'TARGET_USER=\"{target_user}\"; RUNTIME_DIR=\"{runtime_dir}\"; BUS_PATH=\"{bus_path}\"; mkdir -p \"$RUNTIME_DIR\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$RUNTIME_DIR\"; chmod 700 \"$RUNTIME_DIR\"; run_user() {{ sudo -u \"$TARGET_USER\" env XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" DBUS_SESSION_BUS_ADDRESS=unix:path=\"$BUS_PATH\" \"$@\"; }}; PW=$(run_user systemctl --user is-active pipewire 2>/dev/null || true); PWP=$(run_user systemctl --user is-active pipewire-pulse 2>/dev/null || true); WP=$(run_user systemctl --user is-active wireplumber 2>/dev/null || true); SINK_OK=0; if run_user pactl list short sinks 2>/dev/null | grep -Eq \"^[0-9]+[[:space:]]+sunshine_audio([[:space:]]|$)\"; then SINK_OK=1; fi; if [ -S \"$BUS_PATH\" ]; then echo \"session_bus=1\"; else echo \"session_bus=0\"; fi; if [ \"$PW\" = \"active\" ] && [ \"$PWP\" = \"active\" ] && [ \"$WP\" = \"active\" ] && [ \"$SINK_OK\" = \"1\" ]; then echo AUDIO_READY; else echo AUDIO_NOT_READY; echo \"pipewire=$PW\"; echo \"pipewire_pulse=$PWP\"; echo \"wireplumber=$WP\"; echo \"sunshine_audio_sink=$SINK_OK\"; fi'"
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

        let repair_script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
RUNTIME_DIR={runtime_dir}
BUS_PATH={bus_path}
TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
mkdir -p "$RUNTIME_DIR"
chown "$TARGET_USER:$(id -gn "$TARGET_USER")" "$RUNTIME_DIR"
chmod 700 "$RUNTIME_DIR"
run_user() {{
    sudo -u "$TARGET_USER" env \
        HOME="$TARGET_HOME" \
        XDG_RUNTIME_DIR="$RUNTIME_DIR" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=$BUS_PATH" \
        "$@"
}}
run_user mkdir -p "$TARGET_HOME/.config/pipewire/pipewire.conf.d"
cat > "$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf" <<'EOF'
context.objects = [
    {{
        factory = adapter
        args = {{
            factory.name = support.null-audio-sink
            node.name = sunshine_audio
            node.description = "Noland Audio"
            media.class = "Audio/Sink"
            audio.position = [ FL FR ]
            monitor.channel-volumes = true
            monitor.passthrough = true
            adapter.auto-port-config = {{
                mode = dsp
                monitor = true
                position = preserve
            }}
        }}
    }}
]
EOF
chown "$TARGET_USER:$(id -gn "$TARGET_USER")" "$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf"
rm -f "$TARGET_HOME/.config/pipewire/pipewire-pulse.conf.d/20-sunshine-audio.conf"
run_user systemctl --user start pipewire.service pipewire-pulse.service wireplumber.service || true
sleep 2
if ! run_user pactl list short sinks 2>/dev/null | grep -Eq '^[0-9]+[[:space:]]+sunshine_audio([[:space:]]|$)'; then
    run_user pactl load-module module-null-sink \
        sink_name=sunshine_audio \
        sink_properties=device.description=Noland-Audio \
        rate=48000 channels=2 >/dev/null
fi
run_user pactl set-default-sink sunshine_audio
"#,
            target_user = shell_quote(target_user),
            runtime_dir = shell_quote(&runtime_dir),
            bus_path = shell_quote(&bus_path),
        );
        let repair_command = format!("sudo bash -lc {}", shell_quote(&repair_script));
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
            "sudo bash -lc 'TARGET_USER=\"{target_user}\"; RUNTIME_DIR=\"{runtime_dir}\"; BUS_PATH=\"{bus_path}\"; TARGET_HOME=$(getent passwd \"$TARGET_USER\" | cut -d: -f6); run_user() {{ sudo -u \"$TARGET_USER\" env XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" DBUS_SESSION_BUS_ADDRESS=unix:path=\"$BUS_PATH\" \"$@\"; }}; echo --- session-bus ---; test -S \"$BUS_PATH\" && echo BUS_OK || echo BUS_MISSING; echo --- user-audio-status ---; run_user systemctl --user status pipewire pipewire-pulse wireplumber --no-pager 2>/dev/null || true; echo --- pactl-info ---; run_user pactl info 2>/dev/null || true; echo --- sinks ---; run_user pactl list short sinks 2>/dev/null || true; echo --- sources ---; run_user pactl list short sources 2>/dev/null || true; echo --- sunshine-audio-dropin ---; if [ -f \"$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf\" ]; then run_user cat \"$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf\"; else echo MISSING; fi'"
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

    async fn wait_for_user_display_ready(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<String> {
        const DISPLAY_ATTEMPTS: usize = 30;
        const DISPLAY_INTERVAL: Duration = Duration::from_secs(2);
        let target_home = Self::resolve_user_home(remote, target_user).await?;
        let script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
USER_XAUTH={user_xauth}
for candidate in /etc/X11/.Xauthority-noland "$USER_XAUTH"; do
    if [ "$candidate" = "/etc/X11/.Xauthority-noland" ]; then
        chmod 0644 "$candidate" 2>/dev/null || true
    fi
    if [ ! -s "$candidate" ] || ! sudo -u "$TARGET_USER" test -r "$candidate"; then
        continue
    fi
    if command -v timeout >/dev/null 2>&1; then
        DISPLAY_COMMAND=(timeout -s 9 5s xrandr --listmonitors)
    else
        DISPLAY_COMMAND=(xrandr --listmonitors)
    fi
    if sudo -u "$TARGET_USER" env DISPLAY=:0 XAUTHORITY="$candidate" "${{DISPLAY_COMMAND[@]}}" >/dev/null 2>&1; then
        echo "DISPLAY_XAUTHORITY=$candidate"
        exit 0
    fi
done
exit 1"#,
            target_user = shell_quote(target_user),
            user_xauth = shell_quote(&format!("{target_home}/.Xauthority")),
        );
        let command = format!("sudo bash -lc {}", shell_quote(&script));

        for attempt in 1..=DISPLAY_ATTEMPTS {
            match Self::probe_ssh(remote, &command, Duration::from_secs(30)).await {
                Ok(output) if output.status_code == 0 => {
                    if let Some(xauthority) = parse_display_xauthority(&output.stdout) {
                        info!(
                            attempt = attempt,
                            target_user = target_user,
                            xauthority = xauthority,
                            "User display became ready after reboot"
                        );
                        return Ok(xauthority);
                    }
                    warn!(
                        attempt = attempt,
                        target_user = target_user,
                        "Display probe succeeded without reporting its Xauthority path"
                    );
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
            "User display did not become ready after reboot using DISPLAY=:0 with /etc/X11/.Xauthority-noland or {target_home}/.Xauthority"
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

#[cfg(test)]
const BOOT_ID_PREFIX: &str = "REBOOT_BOOT_ID=";
const DISPLAY_XAUTHORITY_PREFIX: &str = "DISPLAY_XAUTHORITY=";

#[cfg(test)]
fn normalize_boot_id(value: &str) -> Option<String> {
    let value = value.trim();
    let parsed = uuid::Uuid::parse_str(value).ok()?;
    (value.len() == 36).then(|| parsed.hyphenated().to_string())
}

#[cfg(test)]
fn parse_marked_boot_id(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        line.trim()
            .strip_prefix(BOOT_ID_PREFIX)
            .and_then(normalize_boot_id)
    })
}

#[cfg(test)]
fn boot_id_changed(old_boot_id: &str, new_boot_id: &str) -> bool {
    match (
        normalize_boot_id(old_boot_id),
        normalize_boot_id(new_boot_id),
    ) {
        (Some(old_boot_id), Some(new_boot_id)) => old_boot_id != new_boot_id,
        _ => false,
    }
}

fn parse_display_xauthority(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let path = line.trim().strip_prefix(DISPLAY_XAUTHORITY_PREFIX)?.trim();
        (!path.is_empty()).then(|| path.to_string())
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
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
    use super::{
        boot_id_changed, filtered_probe_stderr, is_ready_system_state, parse_marked_boot_id,
    };

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

    #[test]
    fn parses_and_normalizes_marked_boot_id() {
        let output = "preflight ok\nREBOOT_BOOT_ID=01234567-89AB-CDEF-0123-456789ABCDEF\n";
        assert_eq!(
            parse_marked_boot_id(output).as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
    }

    #[test]
    fn rejects_missing_or_malformed_boot_id() {
        assert_eq!(parse_marked_boot_id("reboot-online\n"), None);
        assert_eq!(parse_marked_boot_id("REBOOT_BOOT_ID=not-a-uuid\n"), None);
        assert_eq!(
            parse_marked_boot_id("REBOOT_BOOT_ID=0123456789ab-cdef-0123-456789abcdef\n"),
            None
        );
    }

    #[test]
    fn boot_id_comparison_requires_two_valid_different_ids() {
        let old = "01234567-89ab-cdef-0123-456789abcdef";
        let new = "fedcba98-7654-3210-fedc-ba9876543210";

        assert!(!boot_id_changed(old, old));
        assert!(boot_id_changed(old, new));
        assert!(!boot_id_changed(old, "invalid"));
    }
}
