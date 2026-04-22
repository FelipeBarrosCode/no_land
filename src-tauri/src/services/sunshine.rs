use std::{collections::BTreeMap, time::Duration};

use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::{app_config::SunshineDefaults, remote_exec::RemoteExec};

const HEADLESS_EDID_2560X1440_60_BASE64: &str =
    "AP///////wAQrLCgUzIwMQ4aAQS1PCJ4Ok2VqFVOoSYPUFSlSwBxT4GAqcDRwAEBAQEBAQEBVl4AoKCgKVAwIDUAVVAhAAAaAAAA/wBGMExNWDc1MzIwMVMKAAAA/ABERUxMIFUyNzEzSE0KAAAA/QA4TB5TEQAKICAgICAgAIU=";

#[derive(Debug, Clone, Copy)]
pub struct DisplayProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl DisplayProfile {
    pub fn from_moonlight_prefs(width: u32, height: u32, fps: u32) -> Self {
        let width = width.clamp(640, 7680);
        let height = height.clamp(360, 4320);
        let fps = fps.clamp(24, 240);
        Self { width, height, fps }
    }

    pub fn virtual_hz(&self) -> u32 {
        self.fps * 2
    }

    pub fn fallback() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SunshineService {
    pub defaults: SunshineDefaults,
}

const SUNSHINE_PACKAGES: &[&str] = &[
    "sunshine",
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
];

impl SunshineService {
    pub fn render_config(&self, detected_capture: &str, detected_output: &str) -> String {
        let values = BTreeMap::from([
            ("port".to_string(), self.defaults.port.to_string()),
            ("origin_web_ui_allowed".to_string(), "all".to_string()),
            ("upnp".to_string(), "off".to_string()),
            ("encoder".to_string(), self.defaults.encoder.clone()),
            ("av1_mode".to_string(), self.defaults.av1_mode.to_string()),
            ("hevc_mode".to_string(), self.defaults.hevc_mode.to_string()),
            ("capture".to_string(), detected_capture.to_string()),
            (
                "nvenc_preset".to_string(),
                self.defaults.nvenc_preset.to_string(),
            ),
            (
                "fec_percentage".to_string(),
                self.defaults.fec_percentage.to_string(),
            ),
            ("output_name".to_string(), detected_output.to_string()),
            (
                "ping_timeout".to_string(),
                self.defaults.ping_timeout.to_string(),
            ),
        ]);

        values
            .into_iter()
            .map(|(key, value)| format!("{key} = {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn wait_for_dpkg_lock_with_message(&self, remote: &RemoteExec, max_wait_secs: u64) -> AppResult<bool> {
        let lock_script = format!(
            r#"#!/bin/bash
max_wait={}
check_count=0
echo "Waiting for package manager lock to be released..."
while sudo fuser /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock >/dev/null 2>&1; do
    check_count=$((check_count + 1))
    if [ $((check_count % 30)) -eq 0 ]; then
        elapsed=$((check_count))
        echo "Still waiting for package manager lock..." $elapsed "seconds elapsed"
    fi
    sleep 1
    max_wait=$((max_wait - 1))
    if [ $max_wait -le 0 ]; then
        echo "TIMEOUT: Package manager lock not released after {max_wait_secs} seconds"
        exit 1
    fi
done
echo "Package manager lock released after" $check_count "seconds"
exit 0"#,
            max_wait_secs
        );

        let remote = remote.clone();
        let result = tokio::task::spawn_blocking(move || {
            remote.ssh(&lock_script, Duration::from_secs(max_wait_secs + 60))
        })
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

        if result.status_code != 0 {
            info!(
                "dpkg lock wait returned {}: {}",
                result.status_code,
                result.stderr.trim()
            );
            return Ok(false);
        }

        info!("dpkg lock released successfully");
        Ok(true)
    }

    async fn check_sunshine_packages_needed(&self, remote: &RemoteExec) -> AppResult<Vec<String>> {
        let query = SUNSHINE_PACKAGES.join(" ");
        let check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!("dpkg-query -W -f='${{Package}}\\n' {}", query),
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if check.status_code != 0 {
            info!(
                "Package check returned {}, assuming all packages need installation",
                check.status_code
            );
            return Ok(SUNSHINE_PACKAGES.iter().map(|s| s.to_string()).collect());
        }

        let installed: std::collections::HashSet<_> = check
            .stdout
            .lines()
            .map(|l| l.trim().to_lowercase())
            .collect();

        let missing: Vec<String> = SUNSHINE_PACKAGES
            .iter()
            .filter(|p| !installed.contains(&p.to_lowercase()))
            .map(|s| s.to_string())
            .collect();

        if missing.is_empty() {
            info!("All Sunshine packages already installed, skipping apt-get");
        } else {
            info!("Missing Sunshine packages: {}", missing.join(", "));
        }

        Ok(missing)
    }

    pub async fn install_and_configure(
        &self,
        remote: &RemoteExec,
        target_user: &str,
        display: DisplayProfile,
    ) -> AppResult<()> {
        let target_home = self.resolve_user_home(remote, target_user).await?;
        let _target_uid = self.resolve_user_uid(remote, target_user).await?;
        let _target_gid = self.resolve_user_gid(remote, target_user).await?;
        let packages_needed = self.check_sunshine_packages_needed(remote).await?;

        if packages_needed.is_empty() {
            info!("All Sunshine packages already installed, skipping apt-get");
        } else {
            info!(
                "Missing Sunshine packages: {} (need to install)",
                packages_needed.join(", ")
            );

            let lock_acquired = self.wait_for_dpkg_lock_with_message(remote, 600).await?;
            if !lock_acquired {
                return Err(AppError::Provisioning(
                    "Package manager is locked by another process (likely unattended-upgrades). \
                    Waiting timed out after 10 minutes. Please try again in a few minutes when \
                    system updates have finished. Alternatively, you can SSH into the instance and \
                    run: sudo systemctl stop unattended-upgrades && sudo dpkg --configure -a".to_string(),
                ));
            }

            info!(
                "Lock acquired, installing {} missing Sunshine packages",
                packages_needed.len()
            );
            let install = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh(
                        "sudo apt-get -o DPkg::Lock::Timeout=600 update && sudo apt-get -o DPkg::Lock::Timeout=600 install -y sunshine pipewire pipewire-pulse wireplumber",
                        Duration::from_secs(600),
                    )
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };

            if install.status_code != 0 {
                return Err(AppError::Provisioning(format!(
                    "Failed to install Sunshine: {}",
                    install.stderr
                )));
            }

            info!(
                "Successfully installed {} Sunshine packages",
                packages_needed.len()
            );

            // Immediately mask/disable any auto-started sunshine service to prevent
            // port occupation before our provisioning reaches the cleanup step.
            let mask = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh(
                        "sudo systemctl stop sunshine 2>/dev/null || true; sudo systemctl disable sunshine 2>/dev/null || true; sudo systemctl mask sunshine 2>/dev/null || true; sudo pkill -9 -f sunshine 2>/dev/null || true",
                        Duration::from_secs(30),
                    )
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };

            if mask.status_code != 0 {
                warn!(
                    "Post-install sunshine mask had issues (continuing): stdout: {} | stderr: {}",
                    mask.stdout.trim(),
                    mask.stderr.trim()
                );
            }
        }

        self.setup_headless_display(remote, target_user, display).await?;

        // Verify Xorg is running before proceeding
        let xorg_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "pgrep -x Xorg >/dev/null 2>&1 && (command -v timeout >/dev/null 2>&1 && timeout 8s bash -lc 'DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --listmonitors' || DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --listmonitors) || (echo 'Xorg process not found' && exit 1)",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if xorg_check.status_code != 0 {
            let xorg_log_tail = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh(
                        "tail -120 /var/log/Xorg.0.log 2>/dev/null || echo 'Xorg log unavailable'",
                        Duration::from_secs(20),
                    )
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };
            let systemd_status = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh(
                        "systemctl status noland-xorg --no-pager 2>/dev/null || echo 'systemd status unavailable'",
                        Duration::from_secs(15),
                    )
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };
            let journalctl_tail = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh(
                        "journalctl -u noland-xorg --no-pager -n 50 2>/dev/null || echo 'journalctl unavailable'",
                        Duration::from_secs(15),
                    )
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };

            return Err(AppError::Provisioning(format!(
                "Xorg is not running after virtual display setup. Cannot continue. Output: {} | Error: {} | Xorg log tail: {} | systemd status: {} | journalctl: {}",
                xorg_check.stdout.trim(),
                xorg_check.stderr.trim(),
                xorg_log_tail.stdout.trim(),
                systemd_status.stdout.trim(),
                journalctl_tail.stdout.trim()
            )));
        }

        info!("Xorg verified running: {}", xorg_check.stdout.trim());

        self.setup_realtime_permissions(remote).await?;
        self.setup_virtual_input_permissions(remote, target_user).await?;
        self.setup_pipewire_config(remote, target_user).await?;

        let detected_capture = self.detect_capture_backend(remote).await?;
        info!("Detected capture backend: {}", detected_capture);

        let detected_output = self.detect_output_name(remote).await?;
        info!("Detected output name: {}", detected_output);

        // 1. Aggressive cleanup FIRST: kill ALL sunshine processes and free ALL ports
        let cleanup = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "sudo systemctl stop sunshine 2>/dev/null || true; sudo systemctl disable sunshine 2>/dev/null || true; sudo systemctl mask sunshine 2>/dev/null || true; sudo rm -f /etc/systemd/system/sunshine.service 2>/dev/null || true; sudo systemctl daemon-reload 2>/dev/null || true; sudo pkill -9 -f sunshine 2>/dev/null || true; rm -f /tmp/sunshine-start-*.log 2>/dev/null || true; for port in 47984 47989 47990 47991 48010; do sudo fuser -k ${{port}}/tcp 2>/dev/null || true; done; for port in 47998 47999 48000 48002; do sudo fuser -k ${{port}}/udp 2>/dev/null || true; done; sleep 3",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if cleanup.status_code != 0 {
            warn!(
                "Sunshine cleanup had issues (continuing): stdout: {} | stderr: {}",
                cleanup.stdout.trim(),
                cleanup.stderr.trim()
            );
        }

        // 2. Write config using printf (reliable) and verify
        let config = self.render_config(&detected_capture, &detected_output);
        let config_lines: Vec<String> = config.lines().map(|l| l.to_string()).collect();
        let printf_args = config_lines.join("' '");
        let write_config_command = format!(
            "mkdir -p {home}/.config/sunshine && printf '%s\n' '{printf_args}' > {home}/.config/sunshine/sunshine.conf && chown {user}:$(id -gn {user}) {home}/.config/sunshine/sunshine.conf && chmod 644 {home}/.config/sunshine/sunshine.conf && grep -q 'port =' {home}/.config/sunshine/sunshine.conf && echo 'CONFIG_OK' || (echo 'CONFIG_WRITE_FAILED' && exit 1)",
            home = target_home,
            user = target_user,
            printf_args = printf_args
        );

        let write_config = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&write_config_command, Duration::from_secs(90))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if write_config.status_code != 0 || !write_config.stdout.contains("CONFIG_OK") {
            return Err(AppError::Provisioning(format!(
                "Failed to write Sunshine config: stdout: {} | stderr: {}",
                write_config.stdout.trim(),
                write_config.stderr.trim()
            )));
        }

        // 3. Setup display access: copy Xauthority to user home with correct group
        let display_access = {
            let remote = remote.clone();
            let target_user = target_user.to_string();
            let target_home = target_home.to_string();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!(
                        "SHARED_XAUTH=\"/etc/X11/.Xauthority-noland\"; if [ -f \"$SHARED_XAUTH\" ]; then cp \"$SHARED_XAUTH\" \"{target_home}/.Xauthority\" && chown {target_user}:$(id -gn {target_user}) \"{target_home}/.Xauthority\" && chmod 666 \"{target_home}/.Xauthority\" && echo 'XAUTH_OK'; else echo 'XAUTH_MISSING' && exit 1; fi"
                    ),
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if display_access.status_code != 0 || !display_access.stdout.contains("XAUTH_OK") {
            return Err(AppError::Provisioning(format!(
                "Failed to setup display access: stdout: {} | stderr: {}",
                display_access.stdout.trim(),
                display_access.stderr.trim()
            )));
        }

        // 4. Open ALL Moonlight ports (TCP + UDP)
        let firewall = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "sudo ufw allow 47984/tcp 2>/dev/null || true && sudo ufw allow 47989/tcp 2>/dev/null || true && sudo ufw allow 47990/tcp 2>/dev/null || true && sudo ufw allow 47991/tcp 2>/dev/null || true && sudo ufw allow 47998/udp 2>/dev/null || true && sudo ufw allow 47999/udp 2>/dev/null || true && sudo ufw allow 48000/udp 2>/dev/null || true && sudo ufw allow 48002/udp 2>/dev/null || true && sudo ufw allow 48010/tcp 2>/dev/null || true && sudo iptables -I INPUT -p tcp --dport 47984 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p tcp --dport 47989 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p tcp --dport 47990 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p tcp --dport 47991 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p udp --dport 47998 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p udp --dport 47999 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p udp --dport 48000 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p udp --dport 48002 -j ACCEPT 2>/dev/null || true && sudo iptables -I INPUT -p tcp --dport 48010 -j ACCEPT 2>/dev/null || true && echo 'FIREWALL_OK'",
                    Duration::from_secs(60),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if firewall.status_code != 0 {
            warn!(
                "Firewall setup had issues (continuing): stdout: {} | stderr: {}",
                firewall.stdout.trim(),
                firewall.stderr.trim()
            );
        }

        // 5. Start Sunshine — split into TWO SSH calls to avoid session hang
        // Call 1: setsid detaches sunshine into a new session so SSH returns immediately
        let start_cmd = format!(
            "setsid sudo -u {target_user} bash -lc 'cd {target_home} && nohup env DISPLAY=:0 XAUTHORITY={target_home}/.Xauthority sunshine > /tmp/sunshine-start-{target_user}.log 2>&1 < /dev/null &' > /dev/null 2>&1",
            target_user = target_user,
            target_home = target_home,
        );

        let start_sunshine = {
            let remote = remote.clone();
            let start_cmd = start_cmd.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&start_cmd, Duration::from_secs(15))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if start_sunshine.status_code != 0 {
            warn!(
                "Sunshine start command returned non-zero (continuing): stdout: {} | stderr: {}",
                start_sunshine.stdout.trim(),
                start_sunshine.stderr.trim()
            );
        }

        // Give Sunshine time to initialize before checking
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Call 2: verify process is alive and web UI responds
        let verify_cmd = format!(
            "pgrep -x sunshine >/dev/null 2>&1 && curl -k -s --connect-timeout 5 https://localhost:47991/pin >/dev/null 2>&1 && echo 'SUNSHINE_STARTED' || (cat /tmp/sunshine-start-{user}.log 2>/dev/null; echo 'SUNSHINE_FAILED')",
            user = target_user,
        );

        let verify = {
            let remote = remote.clone();
            let verify_cmd = verify_cmd.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&verify_cmd, Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if verify.status_code != 0 || !verify.stdout.contains("SUNSHINE_STARTED") {
            return Err(AppError::Provisioning(format!(
                "Failed to start Sunshine. Log: {} | stdout: {} | stderr: {}",
                verify.stdout.trim(),
                verify.stdout.trim(),
                verify.stderr.trim()
            )));
        }

        // 6. Health check: verify web UI responds
        let health_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "curl -k -s --connect-timeout 10 https://localhost:47991/pin >/dev/null 2>&1 && echo 'HEALTH_OK' || echo 'HEALTH_FAIL'",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if health_check.status_code != 0 || !health_check.stdout.contains("HEALTH_OK") {
            return Err(AppError::Provisioning(format!(
                "Sunshine health check failed (web UI not responding). stdout: {} | stderr: {}",
                health_check.stdout.trim(),
                health_check.stderr.trim()
            )));
        }

        info!(
            "Sunshine Web UI available at https://<wireguard-ip>:47991 (use HTTPS, accept self-signed cert)"
        );

        // 7. Bootstrap credentials
        let creds_command = if target_user == "root" {
            "sunshine --creds sunshine password".to_string()
        } else {
            format!("sudo -u {} sunshine --creds sunshine password", target_user)
        };

        let bootstrap_creds = {
            let remote = remote.clone();
            let creds_command = creds_command.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&creds_command, Duration::from_secs(45))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if bootstrap_creds.status_code != 0 {
            warn!(
                "Sunshine credentials bootstrap failed (continuing): stdout: {} | stderr: {}",
                bootstrap_creds.stdout.trim(),
                bootstrap_creds.stderr.trim()
            );
        }

        let apply_affinity = {
            let remote = remote.clone();
            let cpu_affinity = self.defaults.cpu_affinity.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!(
                        "for pid in $(pgrep -x sunshine); do taskset -pc {cpu_affinity} \"$pid\" || sudo taskset -pc {cpu_affinity} \"$pid\" || true; done"
                    ),
                    Duration::from_secs(60),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if apply_affinity.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to apply Sunshine CPU affinity: {}",
                apply_affinity.stderr
            )));
        }

        self.validate(remote, target_user, display).await
    }

    async fn setup_headless_display(&self, remote: &RemoteExec, target_user: &str, display: DisplayProfile) -> AppResult<()> {
        let target_home = self.resolve_user_home(remote, target_user).await?;
        let target_user_owned = target_user.to_string();
        let uid = {
            let remote = remote.clone();
            let tu = target_user_owned.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&format!("id -u {tu}"), Duration::from_secs(10))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        let _uid: u32 = uid.stdout.trim().parse().unwrap_or(1000);

        let real_display_count: usize = {
            let remote = remote.clone();
            let probe = tokio::task::spawn_blocking(move || {
                remote.ssh(
                    r#"nvidia-smi --query-gpu=name --format=csv,noheader >/dev/null 2>&1 || { echo 0; exit 0; }; if command -v timeout >/dev/null 2>&1; then timeout 8s bash -lc "DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr 2>/dev/null | grep -c '+'" || echo 0; else DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr 2>/dev/null | grep -c '+' || echo 0; fi"#,
                    Duration::from_secs(15),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))?;

            match probe {
                Ok(output) => output.stdout.trim().parse().unwrap_or(0),
                Err(AppError::Timeout(error)) => {
                    warn!(
                        "Display probe timed out (treating as headless): {}",
                        error
                    );
                    0
                }
                Err(error) => return Err(error),
            }
        };

        let is_headless = real_display_count == 0;

        if !is_headless {
            info!("Real display detected. Skipping virtual display setup, using existing Xorg.");
            let create_user_dirs = format!(
                "mkdir -p {}/.config/pipewire/pipewire.conf.d {}/.config/wireplumber {}/.config/systemd/user",
                target_home, target_home, target_home
            );
            let output = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || remote.ssh(&create_user_dirs, Duration::from_secs(30)))
                    .await
                    .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };
            if output.status_code != 0 {
                return Err(AppError::Provisioning(format!(
                    "Failed to create user config directories: {}",
                    output.stderr
                )));
            }
            return Ok(());
        }

        let width = display.width;
        let height = display.height;
        let target_fps = display.fps;
        let virtual_hz = display.virtual_hz();

        info!(
            "Setting up virtual display for NVFBC: {}x{} @ {}Hz (target {} FPS = {} Hz virtual display)",
            width, height, virtual_hz, target_fps, virtual_hz
        );

        let detected_gpu_output = self.detect_gpu_output(remote).await?;
        let gpu_output = if detected_gpu_output.trim().is_empty() {
            warn!("GPU output detection returned empty connector; falling back to DFP-0");
            "DFP-0".to_string()
        } else {
            detected_gpu_output
        };
        info!("Detected GPU output: {}", gpu_output);

        let xorg_config = format!(
            r#"Section "ServerLayout"
   Identifier "TwinLayout"
   Screen 0 "metaScreen" 0 0
EndSection

Section "Monitor"
   Identifier "Monitor0"
   Option "Enable" "true"
EndSection

Section "Device"
   Identifier "Card0"
   Driver "nvidia"
   VendorName "NVIDIA Corporation"
   Option "MetaModes" "{w}x{h}"
   Option "UseDisplayDevice" "DFP"
   Option "ConnectedMonitor" "DFP"
   Option "CustomEDID" "DFP-0:/etc/X11/edid.bin"
   Option "IgnoreEDIDChecksum" "DFP-0"
   Option "HardDPMS" "false"
   Option "ModeDebug" "True"
   Option "ModeValidation" "NoVirtualSizeCheck,NoMaxPClkCheck,NoHorizSyncCheck,NoVertRefreshCheck,AllowNonEdidModes"
   Option "AllowEmptyInitialConfiguration" "True"
EndSection

Section "Screen"
   Identifier "metaScreen"
   Device "Card0"
   Monitor "Monitor0"
   DefaultDepth 24
   SubSection "Display"
       Depth 24
   EndSubSection
EndSection
"#,
            w = width,
            h = height
        );

        let xorg_config_without_connected = format!(
            r#"Section "ServerLayout"
   Identifier "TwinLayout"
   Screen 0 "metaScreen" 0 0
EndSection

Section "Monitor"
   Identifier "Monitor0"
   Option "Enable" "true"
EndSection

Section "Device"
   Identifier "Card0"
   Driver "nvidia"
   VendorName "NVIDIA Corporation"
   Option "MetaModes" "{w}x{h}"
   Option "UseDisplayDevice" "DFP"
   Option "CustomEDID" "DFP-0:/etc/X11/edid.bin"
   Option "IgnoreEDIDChecksum" "DFP-0"
   Option "HardDPMS" "false"
   Option "ModeDebug" "True"
   Option "ModeValidation" "NoVirtualSizeCheck,NoMaxPClkCheck,NoHorizSyncCheck,NoVertRefreshCheck,AllowNonEdidModes"
   Option "AllowEmptyInitialConfiguration" "True"
EndSection

Section "Screen"
   Identifier "metaScreen"
   Device "Card0"
   Monitor "Monitor0"
   DefaultDepth 24
   SubSection "Display"
       Depth 24
   EndSubSection
EndSection
"#,
            w = width,
            h = height,
        );

        let shell_script = format!(
            r#"set -euo pipefail

VIRT_W={w}
VIRT_H={h}
GPU_OUTPUT="{output}"
TARGET_USER="{target_user}"
TARGET_UID="{uid}"
TARGET_GROUP="$(id -gn "$TARGET_USER" 2>/dev/null || echo "$TARGET_USER")"
SHARED_XAUTH="/etc/X11/.Xauthority-noland"

echo "=== Noland TwinView Virtual Display Setup ==="
echo "Resolution: ${{VIRT_W}}x${{VIRT_H}}"
echo "GPU Output: ${{GPU_OUTPUT}}"
echo "Target User: ${{TARGET_USER}}"

# 1. Set DRM permissions for NVIDIA capture
echo "Setting DRM permissions..."
sudo chmod 666 /dev/dri/card0 2>/dev/null || true
sudo chmod 666 /dev/dri/renderD128 2>/dev/null || true

# 2. Install NVIDIA xorg config (TwinView approach - no EDID needed!)
echo "Installing NVIDIA Xorg virtual display config + synthetic EDID..."
sudo mkdir -p /etc/X11
sudo bash -lc 'base64 -d > /etc/X11/edid.bin <<'"'"'EDIDEOF'"'"'
{headless_edid}
EDIDEOF'
sudo chmod 644 /etc/X11/edid.bin
sudo mkdir -p /etc/X11/xorg.conf.d
sudo tee /etc/X11/xorg.conf.d/30-nvidia-virtual.conf > /dev/null <<'XORGEOF'
{xorg_config}
XORGEOF
echo "Xorg config installed"

sudo tee /etc/X11/xorg.conf.d/31-nvidia-virtual-fallback.conf > /dev/null <<'XORGFALLBACKEOF'
{xorg_config_without_connected}
XORGFALLBACKEOF
echo "Xorg fallback config installed"

# 3. STOP DISPLAY MANAGERS (prevents auto-restart of Xorg)
echo "Stopping display managers..."
sudo systemctl stop gdm 2>/dev/null || true
sudo systemctl stop sddm 2>/dev/null || true
sudo systemctl stop lightdm 2>/dev/null || true
sudo systemctl mask gdm 2>/dev/null || true
sudo systemctl mask sddm 2>/dev/null || true
sudo systemctl mask lightdm 2>/dev/null || true
sleep 2

# 4. Stop any existing Xorg service and clean up stale Xauthority
echo "Stopping existing Xorg..."
sudo systemctl stop noland-xorg 2>/dev/null || true
sudo pkill -9 Xorg 2>/dev/null || true
rm -f /root/.Xauthority $SHARED_XAUTH 2>/dev/null || true
sleep 2

# 5. Create shared Xauthority file accessible by both root and user
echo "Creating shared Xauthority..."
rm -f $SHARED_XAUTH
touch $SHARED_XAUTH
chmod 666 $SHARED_XAUTH
# Add both cookie forms required by different Xorg builds
COOKIE_HEX=$(openssl rand -hex 16)
xauth -f $SHARED_XAUTH add :0 . $COOKIE_HEX
xauth -f $SHARED_XAUTH add $(hostname)/unix:0 . $COOKIE_HEX
echo "Xauthority entries:"
xauth -f $SHARED_XAUTH list || true

# 6. Install systemd service for Xorg (survives SSH disconnect)
echo "Installing noland-xorg systemd service..."
sudo tee /etc/systemd/system/noland-xorg.service > /dev/null <<'SYSTEMDEOF'
[Unit]
Description=Noland Virtual Xorg Display
After=systemd-user-sessions.service

[Service]
Type=simple
Environment="DISPLAY=:0"
Environment="XAUTHORITY=/etc/X11/.Xauthority-noland"
ExecStart=/usr/bin/Xorg :0 -config /etc/X11/xorg.conf.d/30-nvidia-virtual.conf -auth /etc/X11/.Xauthority-noland -logfile /var/log/Xorg.0.log -novtswitch -logverbose 7
Restart=on-failure
RestartSec=5
User=root

[Install]
WantedBy=multi-user.target
SYSTEMDEOF
sudo systemctl daemon-reload

# 6a. Start Xorg via systemd
echo "Starting Xorg with virtual display config..."
sudo systemctl start noland-xorg

# Retry check for up to 20 seconds (some VMs are slow to initialize the NVIDIA driver)
XORG_READY=false
for i in $(seq 1 20); do
    sleep 1
    if sudo systemctl is-active noland-xorg >/dev/null 2>&1; then
        echo "Xorg service active after $i seconds"
        XORG_READY=true
        break
    fi
done

if [ "$XORG_READY" != "true" ]; then
    echo "Xorg failed to start with ConnectedMonitor=${{GPU_OUTPUT}}. Checking log..."
    echo "--- systemd status ---"
    sudo systemctl status noland-xorg --no-pager 2>/dev/null || true
    echo "--- journalctl ---"
    sudo journalctl -u noland-xorg --no-pager -n 50 2>/dev/null || true
    echo "--- Xorg log ---"
    tail -80 /var/log/Xorg.0.log 2>/dev/null || true
    echo "Retrying Xorg without ConnectedMonitor..."
    sudo systemctl stop noland-xorg 2>/dev/null || true
    sudo pkill -9 Xorg 2>/dev/null || true
    sleep 2

    # Update service to use fallback config
    sudo tee /etc/systemd/system/noland-xorg.service > /dev/null <<'SYSTEMDEOF'
[Unit]
Description=Noland Virtual Xorg Display
After=systemd-user-sessions.service

[Service]
Type=simple
Environment="DISPLAY=:0"
Environment="XAUTHORITY=/etc/X11/.Xauthority-noland"
ExecStart=/usr/bin/Xorg :0 -config /etc/X11/xorg.conf.d/31-nvidia-virtual-fallback.conf -auth /etc/X11/.Xauthority-noland -logfile /var/log/Xorg.0.log -novtswitch -logverbose 7
Restart=on-failure
RestartSec=5
User=root

[Install]
WantedBy=multi-user.target
SYSTEMDEOF
    sudo systemctl daemon-reload
    sudo systemctl start noland-xorg

    FALLBACK_READY=false
    for i in $(seq 1 20); do
        sleep 1
        if sudo systemctl is-active noland-xorg >/dev/null 2>&1; then
            echo "Xorg fallback service active after $i seconds"
            FALLBACK_READY=true
            break
        fi
    done
    if [ "$FALLBACK_READY" != "true" ]; then
        echo "Xorg fallback start also failed. Checking log..."
        echo "--- systemd status ---"
        sudo systemctl status noland-xorg --no-pager 2>/dev/null || true
        echo "--- journalctl ---"
        sudo journalctl -u noland-xorg --no-pager -n 50 2>/dev/null || true
        echo "--- Xorg log ---"
        tail -100 /var/log/Xorg.0.log 2>/dev/null || true
        exit 1
    fi
fi

echo "Xorg is running: $(pgrep -a Xorg | head -1)"

# Re-sync Xauthority after Xorg startup (Xorg may have added/changed cookies)
echo "Re-syncing Xauthority after Xorg startup..."
sleep 2
if [ -f /root/.Xauthority ]; then
    xauth -f /root/.Xauthority list | while read entry; do
        if [ -n "$entry" ]; then
            xauth -f $SHARED_XAUTH add $entry 2>/dev/null || true
        fi
    done
fi
# Also allow local connections without authentication (fallback)
DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xhost +local: 2>/dev/null || true

# Unmask display managers to avoid permanent host changes
sudo systemctl unmask gdm 2>/dev/null || true
sudo systemctl unmask sddm 2>/dev/null || true
sudo systemctl unmask lightdm 2>/dev/null || true

# 7. Wait for display to be ready
echo "Waiting for display..."
for i in $(seq 1 30); do
    if DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xdpyinfo >/dev/null 2>&1; then
        echo "Display ready after $i seconds"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "WARNING: Display not ready after 30 seconds"
    fi
    sleep 1
done

# 7.1 Force target mode for Sunshine/Moonlight consistency
echo "Enforcing target display mode ${{VIRT_W}}x${{VIRT_H}} @ {vhz}Hz..."
ACTIVE_OUTPUT=$(DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --query | awk '/ connected/{{print $1; exit}}' || true)
if [ -n "$ACTIVE_OUTPUT" ]; then
    if ! DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --query | grep -q " ${{VIRT_W}}x${{VIRT_H}}"; then
        MODELINE=$(cvt -r "$VIRT_W" "$VIRT_H" {vhz} 2>/dev/null | sed -n '2p' || true)
        MODE_NAME=$(echo "$MODELINE" | awk '{{print $2}}' || true)
        if [ -n "$MODELINE" ] && [ -n "$MODE_NAME" ]; then
            DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --newmode ${{MODELINE#Modeline }} 2>/dev/null || true
            DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --addmode "$ACTIVE_OUTPUT" "$MODE_NAME" 2>/dev/null || true
            DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --output "$ACTIVE_OUTPUT" --mode "$MODE_NAME" 2>/dev/null || true
        fi
    else
        DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --output "$ACTIVE_OUTPUT" --mode "${{VIRT_W}}x${{VIRT_H}}" --rate {vhz} 2>/dev/null || true
    fi
fi

# 8. Set up Xauthority for user (symlink to shared file)
echo "Setting up Xauthority for user $TARGET_USER..."
sudo mkdir -p /run/user/$TARGET_UID
sudo cp $SHARED_XAUTH /run/user/$TARGET_UID/.Xauthority 2>/dev/null || true
sudo cp $SHARED_XAUTH /home/$TARGET_USER/.Xauthority 2>/dev/null || true
sudo chmod 666 /run/user/$TARGET_UID/.Xauthority 2>/dev/null || true
sudo chmod 666 /home/$TARGET_USER/.Xauthority 2>/dev/null || true
sudo chown $TARGET_USER:$TARGET_GROUP /run/user/$TARGET_UID/.Xauthority 2>/dev/null || true
sudo chown $TARGET_USER:$TARGET_GROUP /home/$TARGET_USER/.Xauthority 2>/dev/null || true

# 9. Verify
echo "=== Verification ==="
echo "Xorg: $(pgrep -a Xorg | head -1 || echo 'NOT RUNNING')"
echo "Xrandr monitors:"
DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr --listmonitors 2>/dev/null || echo "xrandr FAILED"
echo "Xrandr full output:"
DISPLAY=:0 XAUTHORITY=$SHARED_XAUTH xrandr 2>/dev/null | head -20 || echo "xrandr FAILED"
echo "=== Setup Complete ==="
"#,
            w = width,
            h = height,
            output = gpu_output,
            target_user = target_user,
            uid = _uid,
            xorg_config = xorg_config,
            xorg_config_without_connected = xorg_config_without_connected,
            headless_edid = HEADLESS_EDID_2560X1440_60_BASE64,
            vhz = virtual_hz,
        );

        let escaped_shell = shell_single_quote_escape(&shell_script);
        let install_cmd = format!(
            "sudo bash -lc '{}'",
            escaped_shell
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&install_cmd, Duration::from_secs(300)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup NVIDIA virtual display (exit {}): stdout: {} | stderr: {}",
                output.status_code,
                output.stdout.trim(),
                output.stderr.trim()
            )));
        }

        info!("NVIDIA TwinView virtual display setup output: {}", output.stdout.trim());

        let create_user_dirs = format!(
            "mkdir -p {}/.config/pipewire/pipewire.conf.d {}/.config/wireplumber {}/.config/systemd/user",
            target_home, target_home, target_home
        );
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&create_user_dirs, Duration::from_secs(30)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to create user config directories: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn detect_gpu_output(&self, remote: &RemoteExec) -> AppResult<String> {
        let query_dfp = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "nvidia-xconfig --query-gpu-info 2>/dev/null | awk '/DFP-[0-9]+/{print $1}' | tr -d ':' | head -1",
                    Duration::from_secs(15),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        let dfp_output = query_dfp.stdout.trim();
        if query_dfp.status_code == 0 && !dfp_output.is_empty() {
            info!("Detected NVIDIA DFP connector from nvidia-xconfig: {}", dfp_output);
            return Ok(dfp_output.to_string());
        }

        let gpu_info = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1",
                    Duration::from_secs(15),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if gpu_info.status_code == 0 && !gpu_info.stdout.trim().is_empty() {
            info!(
                "Detected GPU '{}', defaulting headless connector to DFP-0",
                gpu_info.stdout.trim()
            );
            return Ok("DFP-0".to_string());
        }

        warn!("Could not detect NVIDIA GPU connector, defaulting to DFP-0");
        Ok("DFP-0".to_string())
    }

    async fn resolve_user_home(&self, remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let lookup_command = format!("getent passwd {} | cut -d: -f6", target_user);
        let lookup = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&lookup_command, Duration::from_secs(15)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if lookup.status_code == 0 {
            let home = lookup.stdout.trim();
            if !home.is_empty() {
                return Ok(home.to_string());
            }
        }

        if target_user == "root" {
            Ok("/root".to_string())
        } else {
            Ok(format!("/home/{target_user}"))
        }
    }

    async fn resolve_user_uid(&self, remote: &RemoteExec, target_user: &str) -> AppResult<u32> {
        let uid_command = {
            let remote = remote.clone();
            let target_user = target_user.to_string();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&format!("id -u {target_user}"), Duration::from_secs(10))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        uid_command
            .stdout
            .trim()
            .parse::<u32>()
            .map_err(|error| AppError::Provisioning(format!("Failed to resolve UID for {target_user}: {error}")))
    }

    async fn resolve_user_gid(&self, remote: &RemoteExec, target_user: &str) -> AppResult<u32> {
        let gid_command = {
            let remote = remote.clone();
            let target_user = target_user.to_string();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&format!("id -g {target_user}"), Duration::from_secs(10))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        gid_command
            .stdout
            .trim()
            .parse::<u32>()
            .map_err(|error| AppError::Provisioning(format!("Failed to resolve GID for {target_user}: {error}")))
    }

    async fn setup_realtime_permissions(&self, remote: &RemoteExec) -> AppResult<()> {
        let limits_config = r#"# Realtime audio permissions for low-latency streaming
# Generated by Noland Connect

@audio - rtprio 99
@audio - priority -19
@audio - memlock unlimited
@audio - nice -19
"#;

        let escaped = shell_single_quote_escape(limits_config);
        let command = format!(
            "sudo bash -lc 'cat > /etc/security/limits.d/99-realtime-audio.conf <<\"EOF\"\n{}\nEOF'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(30)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to configure realtime permissions: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn setup_virtual_input_permissions(
        &self,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let command = format!(
            r#"set -euo pipefail
TARGET_USER="{target_user}"

sudo modprobe uinput || true
echo uinput | sudo tee /etc/modules-load.d/uinput.conf >/dev/null
sudo tee /etc/udev/rules.d/99-uinput.rules >/dev/null <<'EOF'
KERNEL==\"uinput\", MODE=\"0660\", GROUP=\"input\", OPTIONS+=\"static_node=uinput\"
EOF
sudo udevadm control --reload-rules
sudo udevadm trigger
sudo chgrp input /dev/uinput 2>/dev/null || true
sudo chmod 660 /dev/uinput 2>/dev/null || true
sudo usermod -aG input "$TARGET_USER" || true

if command -v setfacl >/dev/null 2>&1; then
  sudo setfacl -m u:$TARGET_USER:rw /dev/uinput 2>/dev/null || true
fi
"#,
            target_user = target_user
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to configure virtual input device permissions: {}",
                output.stderr.trim()
            )));
        }

        Ok(())
    }

    async fn setup_pipewire_config(&self, remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let target_home = self.resolve_user_home(remote, target_user).await?;
        let pipewire_lowlatency = r#"# Low-latency PipeWire configuration
# Generated by Noland Connect

context.properties = {
    default.clock.rate = 48000,
    default.clock.quantum = 256,
    default.clock.min-quantum = 128,
    default.clock.max-quantum = 1024,
}
"#;

        let escaped = shell_single_quote_escape(pipewire_lowlatency);
        let command = if target_user == "root" {
            format!(
                "mkdir -p {home}/.config/pipewire/pipewire.conf.d && bash -lc 'cat > {home}/.config/pipewire/pipewire.conf.d/10-low-latency.conf <<\"EOF\"\n{config}\nEOF'",
                home = target_home,
                config = escaped
            )
        } else {
            format!(
                "sudo -u {user} mkdir -p {home}/.config/pipewire/pipewire.conf.d && sudo -u {user} bash -lc 'cat > {home}/.config/pipewire/pipewire.conf.d/10-low-latency.conf <<\"EOF\"\n{config}\nEOF'",
                home = target_home,
                config = escaped,
                user = target_user
            )
        };

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to configure PipeWire (exit {}): stdout: {} | stderr: {}",
                output.status_code,
                output.stdout.trim(),
                output.stderr.trim()
            )));
        }

        Ok(())
    }

    async fn detect_capture_backend(&self, remote: &RemoteExec) -> AppResult<String> {
        // Check if Xorg is running first (required for NVFBC)
        let xorg_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("pgrep -x Xorg >/dev/null 2>&1 && DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --listmonitors 2>/dev/null && echo yes || echo no", Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        // Check if this is NVIDIA GPU (NVFBC only works on NVIDIA)
        let nvidia_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1", Duration::from_secs(15))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        let is_nvidia = nvidia_check.status_code == 0 && !nvidia_check.stdout.trim().is_empty();

        // NVFBC: Use immediately if NVIDIA GPU is present
        if is_nvidia {
            info!("NVIDIA GPU detected, using capture backend: nvfbc");
            return Ok("nvfbc".to_string());
        }

        // KMS: Requires DRM device
        let kms_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("ls -la /dev/dri/renderD* 2>/dev/null | head -2 || true", Duration::from_secs(15))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if kms_check.status_code == 0 && !kms_check.stdout.trim().is_empty() {
            info!("KMS available, using capture backend: kms");
            return Ok("kms".to_string());
        }

        // X11 fallback
        if xorg_check.status_code == 0 && xorg_check.stdout.trim() == "yes" {
            info!("X11 available, using capture backend: x11");
            return Ok("x11".to_string());
        }

        info!("No capture backend detected, falling back to: x11");
        Ok("x11".to_string())
    }

    async fn detect_output_name(&self, remote: &RemoteExec) -> AppResult<String> {
        // First try to get the primary output from xrandr --listmonitors
        let monitors_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --listmonitors 2>/dev/null | head -5 || echo ''",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        info!("Monitor list check: {}", monitors_check.stdout.trim());

        // Try to extract connector name from monitors
        let output_from_monitors = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    r#"DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --listmonitors 2>/dev/null | grep -oE '[A-Z]+-[0-9]+' | head -1 || echo "0""#,
                    Duration::from_secs(15),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output_from_monitors.status_code == 0 && !output_from_monitors.stdout.trim().is_empty() {
            let output = output_from_monitors.stdout.trim().to_string();
            if output != "0" && !output.is_empty() {
                info!("Detected output from monitors: {}", output);
                return Ok(output);
            }
        }

        // Try xrandr directly
        let output_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    r#"DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr 2>/dev/null | grep -E '^\s+[0-9]+x[0-9]+' | head -1 | awk '{print $1}' | grep -oE '[a-zA-Z]+[0-9]+' || echo "0""#,
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output_check.status_code == 0 && !output_check.stdout.trim().is_empty() {
            let output = output_check.stdout.trim().to_string();
            if !output.is_empty() && output != "0" {
                info!("Detected output: {}", output);
                return Ok(output);
            }
        }

        // Check NVIDIA outputs specifically
        let nvidia_output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    r#"DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland nvidia-settings -q Xinerama -t 2>/dev/null | head -1 || DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr 2>/dev/null | grep -iE 'dfp|hdmi|dp|virtual' | head -1 | awk '{print $1}' || echo "0""#,
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if nvidia_output.status_code == 0 && !nvidia_output.stdout.trim().is_empty() {
            let output = nvidia_output.stdout.trim().to_string();
            if !output.is_empty() && output != "0" {
                info!("NVIDIA output detected: {}", output);
                return Ok(output);
            }
        }

        info!("Could not detect output, using default: 0");
        Ok("0".to_string())
    }

    pub async fn validate(
        &self,
        remote: &RemoteExec,
        target_user: &str,
        display: DisplayProfile,
    ) -> AppResult<()> {
        let target_home = self.resolve_user_home(remote, target_user).await?;

        // Check Sunshine is running (pgrep, not systemd)
        let process_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "pgrep -x sunshine >/dev/null 2>&1 && echo 'RUNNING' || echo 'NOT_RUNNING'",
                    Duration::from_secs(10),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if process_check.status_code != 0 || !process_check.stdout.contains("RUNNING") {
            let ps_output = {
                let remote = remote.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh("ps aux | grep -i sunshine | grep -v grep || true", Duration::from_secs(10))
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };
            return Err(AppError::Provisioning(format!(
                "Sunshine validation failed (process not running). pgrep stdout: {} | stderr: {} | ps: {}",
                process_check.stdout.trim(),
                process_check.stderr.trim(),
                ps_output.stdout.trim()
            )));
        }

        // Check config file exists
        let config_check = {
            let remote = remote.clone();
            let target_home = target_home.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!("test -f {target_home}/.config/sunshine/sunshine.conf && grep -q 'port =' {target_home}/.config/sunshine/sunshine.conf && echo 'CONFIG_OK' || echo 'CONFIG_BAD'"),
                    Duration::from_secs(10),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if config_check.status_code != 0 || !config_check.stdout.contains("CONFIG_OK") {
            return Err(AppError::Provisioning(format!(
                "Sunshine config validation failed. stdout: {} | stderr: {}",
                config_check.stdout.trim(),
                config_check.stderr.trim()
            )));
        }

        // Check display
        let display_debug = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --listmonitors",
                    Duration::from_secs(40),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if display_debug.status_code != 0 || !display_debug.stdout.contains("Monitors:") {
            warn!(
                "Display probe did not return monitor list; continuing because Sunshine is running. stdout: {} | stderr: {}",
                display_debug.stdout.trim(),
                display_debug.stderr.trim()
            );
        } else {
            info!("Sunshine display check: {}", display_debug.stdout.trim());
        }

        // Check display mode
        let display_mode_check = {
            let remote = remote.clone();
            let expected = format!("{}x{}", display.width, display.height);
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!("DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --query | grep -q \"{expected}\" && echo ok || (DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --query && exit 1)"),
                    Duration::from_secs(40),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if display_mode_check.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Sunshine display mode validation failed. Expected {}x{}, but current mode did not match. Output: {} | stderr: {}",
                display.width,
                display.height,
                display_mode_check.stdout.trim(),
                display_mode_check.stderr.trim()
            )));
        }

        // Check web UI responds
        let web_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "curl -k -s --connect-timeout 5 https://localhost:47991/pin >/dev/null 2>&1 && echo 'WEB_OK' || echo 'WEB_FAIL'",
                    Duration::from_secs(15),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if web_check.status_code != 0 || !web_check.stdout.contains("WEB_OK") {
            return Err(AppError::Provisioning(format!(
                "Sunshine web UI validation failed (not responding on https://localhost:47991/pin). stdout: {} | stderr: {}",
                web_check.stdout.trim(),
                web_check.stderr.trim()
            )));
        }

        Ok(())
    }
}

fn shell_single_quote_escape(content: &str) -> String {
    content.replace('\'', "'\"'\"'")
}

fn strip_wrapping_quotes(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}
