use std::{collections::BTreeMap, time::Duration};

use serde::Serialize;
use tracing::info;

use crate::errors::{AppError, AppResult};

use super::remote_exec::{ExecOutput, RemoteExec};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NvidiaDiagnostics {
    pub commands: BTreeMap<String, ExecOutput>,
}

#[derive(Debug, Clone)]
pub struct NvidiaHeadlessService;

const HEADLESS_PACKAGES: &[&str] = &[
    "x11-xserver-utils",
    "alsa-utils",
];

const APT_UPDATE_TIMEOUT_SECS: u64 = 900;
const APT_INSTALL_TIMEOUT_SECS: u64 = 1800;

impl NvidiaHeadlessService {
    pub async fn setup_and_validate(&self, remote: &RemoteExec) -> AppResult<()> {
        let packages_needed = self.check_packages_needed(remote).await?;

        if packages_needed.is_empty() {
            info!("All headless packages already installed, skipping apt-get");
        } else {
            info!(
                "Missing headless packages: {} (need to install)",
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
                "Lock acquired, installing {} missing headless packages",
                packages_needed.len()
            );

            let mut update_success = false;
            let mut last_update_output: Option<ExecOutput> = None;
            for attempt in 1..=3 {
                info!(
                    "Running apt-get update for NVIDIA headless dependencies (attempt {}/3)",
                    attempt
                );
                let update = {
                    let remote = remote.clone();
                    tokio::task::spawn_blocking(move || {
                        remote.ssh(
                            "sudo apt-get -o DPkg::Lock::Timeout=600 update",
                            Duration::from_secs(APT_UPDATE_TIMEOUT_SECS),
                        )
                    })
                    .await
                    .map_err(|error| AppError::Command(format!("join failure: {error}")))??
                };

                if update.status_code == 0 {
                    update_success = true;
                    break;
                }

                let lock_error = update.stderr.contains("Could not get lock")
                    || update.stderr.contains("Unable to lock directory")
                    || update.stdout.contains("Could not get lock")
                    || update.stdout.contains("Unable to lock directory");

                last_update_output = Some(update.clone());

                if lock_error && attempt < 3 {
                    info!(
                        "apt-get update hit lock contention on attempt {}. Waiting for lock release before retry...",
                        attempt
                    );
                    let lock_acquired_retry = self.wait_for_dpkg_lock_with_message(remote, 600).await?;
                    if !lock_acquired_retry {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }

                break;
            }

            if !update_success {
                let update = last_update_output.unwrap_or_else(|| ExecOutput {
                    command: "sudo apt-get -o DPkg::Lock::Timeout=600 update".to_string(),
                    status_code: -1,
                    stdout: String::new(),
                    stderr: "apt-get update did not run".to_string(),
                    duration_ms: 0,
                });
                return Err(AppError::Provisioning(format!(
                    "Failed apt-get update for NVIDIA headless dependencies (exit {}): stdout: {} | stderr: {}",
                    update.status_code,
                    update.stdout.trim(),
                    update.stderr.trim()
                )));
            }

            let install_script = format!(
                r#"sudo bash -lc 'set -euo pipefail
APT_COMMON_OPTS="-o DPkg::Lock::Timeout=600 -o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30"

echo "[nvidia-headless] Starting apt --fix-broken install"
sudo DEBIAN_FRONTEND=noninteractive apt-get $APT_COMMON_OPTS -y --fix-broken install &
FIX_PID=$!
while kill -0 "$FIX_PID" 2>/dev/null; do
  echo "[nvidia-headless] fix-broken still running... $(date -u +%H:%M:%S)"
  sleep 20
done
wait "$FIX_PID"

echo "[nvidia-headless] Held packages (if any):"
sudo apt-mark showhold || true

echo "[nvidia-headless] Installing headless packages: {packages}"
INSTALLED=0
for attempt in 1 2 3; do
  echo "[nvidia-headless] install attempt $attempt/3"
  sudo DEBIAN_FRONTEND=noninteractive apt-get $APT_COMMON_OPTS install -y --fix-missing {packages} &
  INSTALL_PID=$!
  while kill -0 "$INSTALL_PID" 2>/dev/null; do
    echo "[nvidia-headless] package install still running... $(date -u +%H:%M:%S)"
    sleep 20
  done

  if wait "$INSTALL_PID"; then
    INSTALLED=1
    echo "[nvidia-headless] install attempt $attempt succeeded"
    break
  fi

  echo "[nvidia-headless] install attempt $attempt failed"
  if [ "$attempt" -lt 3 ]; then
    echo "[nvidia-headless] refreshing package indexes before retry"
    sudo apt-get $APT_COMMON_OPTS update || true
    sleep 15
  fi
done

if [ "$INSTALLED" -ne 1 ]; then
  echo "[nvidia-headless] package install failed after 3 attempts"
  exit 100
fi

echo "[nvidia-headless] Headless package install complete"'"#,
                packages = packages_needed.join(" ")
            );

            let install = {
                let remote = remote.clone();
                let install_script = install_script.clone();
                tokio::task::spawn_blocking(move || {
                    remote.ssh(&install_script, Duration::from_secs(APT_INSTALL_TIMEOUT_SECS))
                })
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
            };

            if install.status_code != 0 {
                return Err(AppError::Provisioning(format!(
                    "Failed installing NVIDIA headless dependencies (exit {}): stdout: {} | stderr: {}",
                    install.status_code,
                    install.stdout.trim(),
                    install.stderr.trim()
                )));
            }

            info!(
                "Successfully installed {} headless packages",
                packages_needed.len()
            );
        }

        let nvidia_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh("nvidia-smi", Duration::from_secs(30)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if nvidia_check.status_code != 0 || !nvidia_check.stdout.contains("NVIDIA-SMI") {
            return Err(AppError::Provisioning(
                "CUDA device not detected. Please pick a compatible NVIDIA host.".to_string(),
            ));
        }

        info!("GPU detected: {}", nvidia_check.stdout.lines().next().unwrap_or("unknown"));

        let gpu_info = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv,noheader", Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if gpu_info.status_code == 0 {
            info!("GPU Info: {}", gpu_info.stdout.trim().replace('\n', " | "));
        }

        self.enable_persistence_mode(remote).await?;

        let encoder_check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "nvidia-smi --query-gpu=encoder.stats.averageFps --format=csv,noheader",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if encoder_check.status_code != 0 {
            return Err(AppError::Provisioning(
                "NVENC path unavailable. Sunshine cannot stream without a usable encoder."
                    .to_string(),
            ));
        }

        let encoder_supported = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("nvidia-smi --query-gpu=encoder.supported --format=csv,noheader", Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if encoder_supported.status_code == 0 {
            info!("NVENC supported: {}", encoder_supported.stdout.trim());
        }

        Ok(())
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

    async fn check_packages_needed(&self, remote: &RemoteExec) -> AppResult<Vec<String>> {
        let query = HEADLESS_PACKAGES.join(" ");
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
            return Ok(HEADLESS_PACKAGES.iter().map(|s| s.to_string()).collect());
        }

        let installed: std::collections::HashSet<_> = check
            .stdout
            .lines()
            .map(|l| l.trim().to_lowercase())
            .collect();

        let missing: Vec<String> = HEADLESS_PACKAGES
            .iter()
            .filter(|p| !installed.contains(&p.to_lowercase()))
            .map(|s| s.to_string())
            .collect();

        if missing.is_empty() {
            info!("All headless packages already installed, skipping apt-get");
        } else {
            info!("Missing packages: {}", missing.join(", "));
        }

        Ok(missing)
    }

    async fn enable_persistence_mode(&self, remote: &RemoteExec) -> AppResult<()> {
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("sudo nvidia-smi -pm ENABLED", Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            info!("Warning: Could not enable persistence mode: {}", output.stderr);
        } else {
            info!("NVIDIA persistence mode enabled");
        }

        Ok(())
    }

    pub async fn collect_diagnostics(&self, remote: &RemoteExec) -> AppResult<NvidiaDiagnostics> {
        let command_list = [
            "nvidia-smi",
            "nvidia-smi -q",
            "xrandr",
            "systemctl status sunshine --no-pager",
            "journalctl -u sunshine --no-pager -n 200",
            "pactl list short sinks",
            "ip a",
            "ip route",
            "wg show",
            "tc qdisc show dev wg0",
            "ulimit -r",
            "cat /proc/cmdline | grep -i nvidia",
        ];

        let mut map = BTreeMap::new();
        for command in command_list {
            let remote_clone = remote.clone();
            let output = tokio::task::spawn_blocking(move || {
                remote_clone.ssh(command, Duration::from_secs(40))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??;
            map.insert(command.to_string(), output);
        }

        Ok(NvidiaDiagnostics { commands: map })
    }
}
