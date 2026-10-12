use std::time::Duration;

use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

/// Run once before launching independent provisioning lanes. Never invoke
/// destructive dpkg lock recovery while another lane may be installing.
pub async fn prepare_provisioning_packages(remote: &RemoteExec) -> AppResult<()> {
    if !wait_for_dpkg_lock(remote, 180).await? {
        return Err(AppError::Provisioning(
            "Remote package manager is not ready".into(),
        ));
    }
    let prebuilt =
        super::shared_storage::agent_runtime::locate_state_agent_source_dir().is_ok_and(|source| {
            ["x86_64", "aarch64"].iter().all(|arch| {
                source
                    .join("prebuilt")
                    .join(arch)
                    .join("SHA256SUMS")
                    .is_file()
            })
        });
    let script =
        include_str!("../../scripts/bootstrap_provisioning_packages.sh").replace("\r\n", "\n");
    let script = format!(
        "export NOLAND_PREBUILT_AGENTS={}\n{script}",
        u8::from(prebuilt)
    );
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh_with_stdin(
            "sudo bash -s",
            script.into_bytes(),
            Duration::from_secs(1200),
        )
    })
    .await
    .map_err(|error| AppError::Command(format!("package bootstrap join failure: {error}")))??;
    if output.status_code != 0 || !output.stdout.contains("NOLAND_PROVISION_PACKAGES_READY") {
        return Err(AppError::Provisioning(format!(
            "Combined provisioning package installation failed: {} {}",
            output.stdout, output.stderr
        )));
    }
    Ok(())
}

/// Quiesce Ubuntu's periodic APT jobs and acquire the dpkg locks for provisioning.
///
/// Fresh cloud images frequently start `apt-daily-upgrade` while provisioning is
/// connecting. Stopping only `unattended-upgrades.service` is insufficient because
/// the apt timers can immediately launch a new process. This helper disables the
/// complete periodic chain, gives an active transaction a grace period, terminates
/// only known apt/dpkg holders if it remains stuck, and repairs interrupted dpkg
/// state after every lock is actually free.
pub async fn wait_for_dpkg_lock(remote: &RemoteExec, max_wait_secs: u64) -> AppResult<bool> {
    let script = format!(
        r#"#!/bin/bash
set -uo pipefail

LOCK_FILES="/var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock"
APT_UNITS="apt-daily.timer apt-daily-upgrade.timer apt-daily.service apt-daily-upgrade.service unattended-upgrades.service"
MAX_WAIT={max_wait_secs}
GRACE_SECONDS=30
TERM_GRACE_SECONDS=15
started=$(date +%s)
term_sent_at=0

# During coordinated provisioning another lane may own APT. Never kill it.
# The initial recovery gate runs before this per-boot marker is created.
if [ -f /run/noland-provision-packages-ready ]; then
  while sudo fuser $LOCK_FILES >/dev/null 2>&1; do
    if [ $(( $(date +%s) - started )) -ge "$MAX_WAIT" ]; then exit 20; fi
    sleep 1
  done
  echo NOLAND_DPKG_READY
  exit 0
fi

sudo systemctl stop --no-block $APT_UNITS >/dev/null 2>&1 || true
sudo systemctl mask $APT_UNITS >/dev/null 2>&1 || true
sudo install -d -m 0755 /etc/apt/apt.conf.d
printf '%s\n' \
  'APT::Periodic::Enable "0";' \
  'APT::Periodic::Update-Package-Lists "0";' \
  'APT::Periodic::Unattended-Upgrade "0";' \
  | sudo tee /etc/apt/apt.conf.d/99noland-disable-periodic >/dev/null

lock_holders() {{
  for lock in $LOCK_FILES; do
    sudo fuser "$lock" 2>/dev/null || true
  done | tr ' ' '\n' | grep -E '^[0-9]+$' | sort -u
}}

is_package_process() {{
  pid="$1"
  cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)
  case "$cmd" in
    *apt.systemd.daily*|*unattended-upgrade*|*/apt-get*|*/apt\ *|*/dpkg*) return 0 ;;
    *) return 1 ;;
  esac
}}

while true; do
  holders=$(lock_holders)
  if [ -z "$holders" ]; then
    break
  fi

  now=$(date +%s)
  elapsed=$((now - started))
  if [ "$elapsed" -ge "$GRACE_SECONDS" ]; then
    for pid in $holders; do
      if is_package_process "$pid"; then
        if [ "$term_sent_at" -eq 0 ]; then
          echo "Stopping stuck package process PID $pid after ${{elapsed}}s grace"
          sudo kill -TERM "$pid" 2>/dev/null || true
        elif [ $((now - term_sent_at)) -ge "$TERM_GRACE_SECONDS" ]; then
          echo "Force-stopping package process PID $pid after TERM grace"
          sudo kill -KILL "$pid" 2>/dev/null || true
        fi
      else
        echo "Refusing to terminate unknown lock holder PID $pid" >&2
        exit 22
      fi
    done
    if [ "$term_sent_at" -eq 0 ]; then
      term_sent_at=$now
    fi
  fi

  if [ "$elapsed" -ge "$MAX_WAIT" ]; then
    echo "PACKAGE_LOCK_TIMEOUT holders=$holders" >&2
    for pid in $holders; do
      ps -p "$pid" -o pid=,ppid=,etime=,stat=,cmd= >&2 || true
    done
    exit 20
  fi
  sleep 2
done

if ! sudo timeout 300 dpkg --configure -a; then
  echo "DPKG_REPAIR_FAILED" >&2
  exit 21
fi

echo "NOLAND_DPKG_READY"
"#
    );

    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh(&script, Duration::from_secs(max_wait_secs + 330))
    })
    .await
    .map_err(|error| {
        AppError::Command(format!("package-manager recovery join failure: {error}"))
    })??;

    if output.status_code == 0 && output.stdout.contains("NOLAND_DPKG_READY") {
        info!(details = %output.stdout.trim(), "package manager is ready for provisioning");
        return Ok(true);
    }

    warn!(
        status = output.status_code,
        stdout = %output.stdout.trim(),
        stderr = %output.stderr.trim(),
        "package manager did not become ready"
    );
    Ok(false)
}
