use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::env;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::Serialize;
use tokio::fs;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::{
    app_config::WireGuardDefaults, app_context::AppContext, os_detection::OsDetection,
    remote_exec::RemoteExec,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireGuardProvisionResult {
    pub server_ip: String,
    pub client_ip: String,
    pub server_public_key: String,
    pub client_public_key: String,
    pub client_config_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireGuardProvisionMode {
    FreshProvision,
    ReinitializeExisting,
}

#[derive(Debug, Clone)]
pub struct WireGuardService {
    pub defaults: WireGuardDefaults,
}

#[derive(Debug, Clone)]
struct ExistingRemoteIdentity {
    server_private_key: String,
    client_public_key: String,
}

#[derive(Debug, Clone)]
struct ExistingLocalIdentity {
    client_private_key: String,
    server_public_key: String,
}

#[derive(Debug, Clone)]
struct ExpectedLocalTunnel {
    interface_private_key: String,
    interface_public_key: String,
    peer_public_key: String,
    allowed_ips: String,
    endpoint_host: String,
    endpoint_port: u16,
    server_ip: String,
    client_ip: String,
}

#[derive(Debug, Clone, Default)]
struct LocalWireGuardRuntimeState {
    interface_name: String,
    peer_public_key: String,
    allowed_ips: String,
    latest_handshake: String,
}

#[derive(Debug, Clone, Default)]
struct LocalWireGuardPeerState {
    interface_name: String,
    peer_public_key: String,
    allowed_ips: String,
    latest_handshake: String,
}

const REQUIRED_REMOTE_WIREGUARD_PACKAGES: &[&str] = &["wireguard-tools", "iproute2", "ufw"];
const APT_UPDATE_TIMEOUT_SECS: u64 = 180;
const APT_INSTALL_TIMEOUT_SECS: u64 = 300;
const PACKAGE_MANAGER_READY_WAIT_SECS: u64 = 180;
#[cfg(target_os = "macos")]
const MACOS_HELPER_SUCCESS_GRACE_SECS: u64 = 300;
const MACOS_LOCAL_CONF_PATH: &str = "/usr/local/etc/wireguard/nolandwg0.conf";
const MACOS_HOMEBREW_CONF_PATH: &str = "/opt/homebrew/etc/wireguard/nolandwg0.conf";
const LEGACY_LOCAL_CONFIG_NAME: &str = "nolandwg0.conf";
#[cfg(target_os = "macos")]
const MACOS_WIREGUARD_HELPER_LABEL: &str = "com.noland.wireguard.nolandwg0";
#[cfg(target_os = "macos")]
const MACOS_WIREGUARD_HELPER_SCRIPT_PATH: &str = "/usr/local/libexec/noland-wireguard-repair.sh";
#[cfg(target_os = "macos")]
const MACOS_WIREGUARD_HELPER_PLIST_PATH: &str =
    "/Library/LaunchDaemons/com.noland.wireguard.nolandwg0.plist";
#[cfg(target_os = "macos")]
const MACOS_HELPER_MONITOR_REPAIR_COOLDOWN_SECS: u64 = 600;
const MONITOR_REPAIR_FAILURE_STREAK_THRESHOLD: u32 = 5;
const MONITOR_CONFLICT_WARN_EVERY: u32 = 5;
#[cfg(target_os = "macos")]
static MACOS_HELPER_LAST_MONITOR_REPAIR: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
static MONITOR_MISMATCH_STREAK: OnceLock<Mutex<u32>> = OnceLock::new();
static MONITOR_REPAIR_FAILURE_STREAK: OnceLock<Mutex<u32>> = OnceLock::new();

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacosHelperGeneration {
    Legacy,
    WatchRequests,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacosHelperStatusKind {
    Healthy,
    Repaired,
    Error,
    Invalid,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct MacosHelperStatus {
    kind: MacosHelperStatusKind,
    timestamp: Option<u64>,
    message: String,
}

fn legacy_local_config_path(app_data_dir: &Path) -> PathBuf {
    wireguard_local_root_dir(app_data_dir).join(LEGACY_LOCAL_CONFIG_NAME)
}

#[cfg(target_os = "macos")]
fn wireguard_local_root_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("wireguard-local")
}

#[cfg(not(target_os = "macos"))]
fn wireguard_local_root_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("wireguard")
}

fn monitor_mismatch_streak() -> &'static Mutex<u32> {
    MONITOR_MISMATCH_STREAK.get_or_init(|| Mutex::new(0))
}

fn note_monitor_conflict_candidate(conflict_candidate: bool) -> Option<u32> {
    let Ok(mut streak) = monitor_mismatch_streak().lock() else {
        return None;
    };
    if conflict_candidate {
        *streak = streak.saturating_add(1);
        Some(*streak)
    } else {
        *streak = 0;
        None
    }
}

fn monitor_repair_failure_streak() -> &'static Mutex<u32> {
    MONITOR_REPAIR_FAILURE_STREAK.get_or_init(|| Mutex::new(0))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn gotatun_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => "",
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn gotatun_binary_names() -> Vec<String> {
    let mut names = vec!["gotatun".to_string()];
    let triple = gotatun_target_triple();
    if !triple.is_empty() {
        names.push(format!("gotatun-{triple}"));
    }
    names
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn gotatun_candidate_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let names = gotatun_binary_names();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for name in &names {
                candidates.push(exe_dir.join(name));
                candidates.push(exe_dir.join("binaries").join(name));
                candidates.push(exe_dir.join("..").join("binaries").join(name));
                candidates.push(exe_dir.join("..").join("Resources").join(name));
                candidates.push(
                    exe_dir
                        .join("..")
                        .join("Resources")
                        .join("binaries")
                        .join(name),
                );
            }
        }
    }

    if let Ok(cwd) = env::current_dir() {
        for name in &names {
            candidates.push(cwd.join(name));
            candidates.push(cwd.join("binaries").join(name));
            candidates.push(cwd.join("src-tauri").join("binaries").join(name));
        }
    }

    candidates
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        return metadata.permissions().mode() & 0o111 != 0;
    }

    #[allow(unreachable_code)]
    true
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn locate_gotatun_binary() -> Option<PathBuf> {
    let env_override = std::env::var("NOLAND_GOTATUN_BIN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = env_override.filter(|path| is_executable_file(path)) {
        return Some(path);
    }

    let os = OsDetection::new();
    if let Some(path) = os.resolve_command_path("gotatun") {
        return Some(PathBuf::from(path));
    }

    for candidate in gotatun_candidate_paths() {
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    None
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn resolve_gotatun_binary() -> AppResult<String> {
    if let Some(path) = locate_gotatun_binary() {
        return Ok(path.display().to_string());
    }

    let searched = gotatun_candidate_paths()
        .into_iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    Err(AppError::Command(
        format!(
            "GotaTun is required for Noland's managed userspace tunnel, but no executable was found. Install or build `gotatun`, place it in PATH or `src-tauri/binaries`, or set `NOLAND_GOTATUN_BIN` to its full path. Searched: {searched}"
        ),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn locate_gotatun_binary() -> Option<PathBuf> {
    None
}

fn note_monitor_repair_health(healthy: bool) -> u32 {
    let Ok(mut streak) = monitor_repair_failure_streak().lock() else {
        return 0;
    };

    if healthy {
        *streak = 0;
    } else {
        *streak = streak.saturating_add(1);
    }

    *streak
}

fn instance_local_config_path(app_data_dir: &Path, instance_id: u64) -> PathBuf {
    wireguard_local_root_dir(app_data_dir)
        .join(instance_id.to_string())
        .join(LEGACY_LOCAL_CONFIG_NAME)
}

fn wireguard_root_from_config_path(config_path: &Path) -> Option<PathBuf> {
    let parent = config_path.parent()?;
    if parent
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
    {
        return parent.parent().map(Path::to_path_buf);
    }
    Some(parent.to_path_buf())
}

fn resolve_active_wireguard_config_path(
    state: &crate::models::app_state::PersistedAppState,
) -> Option<PathBuf> {
    if let Some(instance_id) = state.instance.instance_id {
        if let Some(path) = state
            .provisioned_servers
            .iter()
            .find(|record| record.instance_id == instance_id)
            .map(|record| PathBuf::from(record.wireguard_config_path.clone()))
            .filter(|path| path.exists())
        {
            return Some(path);
        }

        let current = PathBuf::from(state.wireguard.config_path.clone());
        if current.exists() {
            return Some(current);
        }
    }

    state
        .provisioned_servers
        .iter()
        .filter_map(|record| {
            let path = PathBuf::from(record.wireguard_config_path.clone());
            if !path.exists() {
                return None;
            }
            let modified = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok();
            Some((path, modified))
        })
        .max_by_key(|(_, modified)| *modified)
        .map(|(path, _)| path)
        .or_else(|| {
            let current = PathBuf::from(state.wireguard.config_path.clone());
            if current.exists() {
                Some(current)
            } else {
                None
            }
        })
}

fn cleanup_stale_wireguard_artifacts(active_config_path: &Path) {
    let Some(root) = wireguard_root_from_config_path(active_config_path) else {
        return;
    };

    let active_instance_dir = active_config_path.parent().map(Path::to_path_buf);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_file() {
            let extension = path.extension().and_then(|value| value.to_str());
            if matches!(extension, Some("request") | Some("status")) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }

        if !file_type.is_dir() {
            continue;
        }

        if active_instance_dir
            .as_ref()
            .map(|active| active == &path)
            .unwrap_or(false)
        {
            continue;
        }

        let conf_path = path.join(LEGACY_LOCAL_CONFIG_NAME);
        if !conf_path.exists() {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }

        let request_path = path.join("nolandwg0.repair.request");
        let status_path = path.join("nolandwg0.repair.status");
        let _ = std::fs::remove_file(request_path);
        let _ = std::fs::remove_file(status_path);
    }
}

#[cfg(target_os = "macos")]
fn repair_stale_local_wireguard_root(app_data_dir: &Path) -> AppResult<bool> {
    let wireguard_root = wireguard_local_root_dir(app_data_dir);
    if !wireguard_root.exists() {
        return Ok(false);
    }

    let timestamp = current_unix_timestamp()?;
    let backup_root = app_data_dir.join(format!("wireguard.stale-root-{timestamp}"));
    std::fs::rename(&wireguard_root, &backup_root).map_err(|error| {
        AppError::Io(format!(
            "Failed moving stale WireGuard directory {} aside to {}: {error}",
            wireguard_root.display(),
            backup_root.display()
        ))
    })?;
    std::fs::create_dir_all(&wireguard_root).map_err(|error| {
        AppError::Io(format!(
            "Failed recreating WireGuard directory {} after repair: {error}",
            wireguard_root.display()
        ))
    })?;

    warn!(
        "Repaired stale local WireGuard directory by moving {} to {} and recreating the root",
        wireguard_root.display(),
        backup_root.display()
    );
    Ok(true)
}

#[cfg(target_os = "macos")]
fn read_macos_helper_status(config_path: &Path) -> Option<MacosHelperStatus> {
    let status_path = macos_helper_status_path(config_path);
    let content = std::fs::read_to_string(status_path).ok()?;
    let kind = match content
        .lines()
        .find_map(|line| line.strip_prefix("status="))
        .unwrap_or("")
    {
        "healthy" => MacosHelperStatusKind::Healthy,
        "repaired" => MacosHelperStatusKind::Repaired,
        "error" => MacosHelperStatusKind::Error,
        _ => MacosHelperStatusKind::Invalid,
    };
    let timestamp = content
        .lines()
        .find_map(|line| line.strip_prefix("timestamp="))
        .and_then(|value| value.trim().parse::<u64>().ok());
    let message = content
        .lines()
        .find_map(|line| line.strip_prefix("message="))
        .unwrap_or("")
        .to_string();

    Some(MacosHelperStatus {
        kind,
        timestamp,
        message,
    })
}

#[cfg(target_os = "macos")]
fn helper_status_recently_succeeded(status: &MacosHelperStatus) -> bool {
    matches!(
        status.kind,
        MacosHelperStatusKind::Healthy | MacosHelperStatusKind::Repaired
    ) && status
        .timestamp
        .zip(current_unix_timestamp().ok())
        .map(|(timestamp, now)| now.saturating_sub(timestamp) <= MACOS_HELPER_SUCCESS_GRACE_SECS)
        .unwrap_or(false)
}

impl WireGuardService {
    async fn wait_for_dpkg_lock_with_message(
        &self,
        remote: &RemoteExec,
        max_wait_secs: u64,
    ) -> AppResult<bool> {
        let surgical_script = format!(
            r#"#!/bin/bash
set -uo pipefail

LOCK_FILES="/var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock"
MAX_WAIT={max_wait_secs}

check_lock() {{
    for lock in $LOCK_FILES; do
        if sudo fuser "$lock" >/dev/null 2>&1; then
            return 1
        fi
    done
    return 0
}}

# Phase 1: Quick check (0-3 seconds)
if check_lock; then
    echo "LOCK_FREE"
    exit 0
fi

# Phase 2: Aggressive kill (unattended-upgrades often auto-restarts)
echo "LOCK_HELD: killing competing apt processes..."
sudo systemctl stop unattended-upgrades 2>/dev/null || true
sudo systemctl mask unattended-upgrades 2>/dev/null || true
sudo pkill -9 -f unattended-upgrades 2>/dev/null || true
sudo pkill -9 -f apt.systemd.daily 2>/dev/null || true
sudo pkill -9 -f "[a]pt-get" 2>/dev/null || true
sudo pkill -9 -f "[d]pkg" 2>/dev/null || true
sleep 2

# Phase 3: Fix broken dpkg state and remove stale locks
echo "Fixing dpkg state..."
sudo dpkg --configure -a 2>/dev/null || true
for lock in $LOCK_FILES; do
    if [ -f "$lock" ]; then
        sudo rm -f "$lock" 2>/dev/null || true
    fi
done

# Phase 4: Check again after cleanup
sleep 1
if check_lock; then
    echo "LOCK_FREE_AFTER_KILL"
    exit 0
fi

# Phase 5: Patient wait with timeout
echo "Still locked after cleanup, waiting up to ${{MAX_WAIT}}s..."
check_count=0
while ! check_lock; do
    check_count=$((check_count + 1))
    if [ $((check_count % 15)) -eq 0 ]; then
        echo "Still waiting for package manager lock... ${{check_count}}s elapsed"
    fi
    sleep 1
    if [ $check_count -ge $MAX_WAIT ]; then
        echo "TIMEOUT: Package manager lock not released after ${{MAX_WAIT}} seconds"
        # Unmask so future boots are not broken
        sudo systemctl unmask unattended-upgrades 2>/dev/null || true
        exit 1
    fi
done

# Unmask so future boots are not broken
sudo systemctl unmask unattended-upgrades 2>/dev/null || true
echo "LOCK_RELEASED_AFTER_WAIT ${{check_count}}"
exit 0"#
        );

        for attempt in 1..=3 {
            let remote = remote.clone();
            let surgical_script = surgical_script.clone();
            let result = tokio::task::spawn_blocking(move || {
                remote.ssh(&surgical_script, Duration::from_secs(max_wait_secs + 30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

            if result.status_code == 0 {
                let stdout = result.stdout.trim();
                if stdout.contains("LOCK_FREE") || stdout.contains("LOCK_RELEASED") {
                    info!("dpkg lock acquired: {}", stdout);
                    return Ok(true);
                }

                info!("dpkg lock check returned unexpected output: {}", stdout);
                return Ok(false);
            }

            let stderr = result.stderr.trim();
            let stdout = result.stdout.trim();
            let retryable_ssh_failure = result.status_code == 255
                || stderr.contains("Connection closed")
                || stderr.contains("Broken pipe")
                || stderr.contains("Operation timed out")
                || stderr.contains("kex_exchange_identification")
                || stdout.contains("LOCK_HELD");

            info!(
                "dpkg lock wait attempt {} returned {}: stdout={} stderr={}",
                attempt, result.status_code, stdout, stderr
            );

            if !retryable_ssh_failure || attempt == 3 {
                return Ok(false);
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        Ok(false)
    }

    async fn wait_for_package_manager_ready(&self, remote: &RemoteExec) -> AppResult<()> {
        if self
            .wait_for_dpkg_lock_with_message(remote, PACKAGE_MANAGER_READY_WAIT_SECS)
            .await?
        {
            return Ok(());
        }

        Err(AppError::Provisioning(format!(
            "Package manager is locked by another process (likely unattended-upgrades). Waiting timed out after {} seconds. Please try again in a few minutes when system updates have finished. Alternatively, you can SSH into the instance and run: sudo systemctl stop unattended-upgrades && sudo dpkg --configure -a",
            PACKAGE_MANAGER_READY_WAIT_SECS
        )))
    }

    pub async fn configure(
        &self,
        remote: &RemoteExec,
        local_app_data_dir: &Path,
        instance_id: u64,
        endpoint_host: &str,
        endpoint_port: u16,
        mode: WireGuardProvisionMode,
    ) -> AppResult<WireGuardProvisionResult> {
        ensure_local_wireguard_tools()?;

        let local_config_path = instance_local_config_path(local_app_data_dir, instance_id);
        let local_config_dir = local_config_path.parent().unwrap_or(local_app_data_dir);
        if let Err(error) = fs::create_dir_all(&local_config_dir).await {
            #[cfg(target_os = "macos")]
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && repair_stale_local_wireguard_root(local_app_data_dir)?
            {
                fs::create_dir_all(&local_config_dir).await?;
            } else {
                return Err(error.into());
            }

            #[cfg(not(target_os = "macos"))]
            return Err(error.into());
        }
        let legacy_config_path = legacy_local_config_path(local_app_data_dir);

        let existing_remote_identity = self.load_existing_remote_identity(remote).await?;
        let existing_local_identity = match load_existing_local_identity(&local_config_path).await?
        {
            Some(identity) => Some(identity),
            None if legacy_config_path != local_config_path => {
                load_existing_local_identity(&legacy_config_path).await?
            }
            None => None,
        };
        let generate_fresh_identity = || -> AppResult<(String, String, String, String)> {
            let (server_private, server_public) = generate_keypair()?;
            let (client_private, client_public) = generate_keypair()?;
            Ok((server_private, server_public, client_private, client_public))
        };

        let (server_private, server_public, client_private, client_public) = match (
            existing_remote_identity,
            existing_local_identity,
        ) {
            (Some(remote_identity), Some(local_identity)) => {
                let derived_server_public = derive_public_key(&remote_identity.server_private_key)?;
                let derived_client_public = derive_public_key(&local_identity.client_private_key)?;
                if derived_server_public != local_identity.server_public_key
                    || derived_client_public != remote_identity.client_public_key
                {
                    warn!(
                            "WireGuard identity mismatch detected (mode={mode:?}); regenerating tunnel identity"
                        );
                    generate_fresh_identity()?
                } else {
                    info!("Reusing existing WireGuard key material from prior provisioning");
                    (
                        remote_identity.server_private_key,
                        derived_server_public,
                        local_identity.client_private_key,
                        derived_client_public,
                    )
                }
            }
            (Some(_), None) => {
                warn!(
                        "Remote WireGuard identity exists but local client config {} is missing (mode={mode:?}); regenerating local and remote tunnel identity",
                        local_config_path.display()
                    );
                generate_fresh_identity()?
            }
            (None, Some(_)) => {
                warn!(
                        "Local WireGuard identity exists at {} but remote server identity is missing (mode={mode:?}); regenerating local and remote tunnel identity",
                        local_config_path.display()
                    );
                generate_fresh_identity()?
            }
            (None, None) => {
                info!("No existing WireGuard key material found; bootstrapping new keys");
                generate_fresh_identity()?
            }
        };

        self.cleanup_existing_wireguard(remote).await?;
        self.setup_cpu_governor(remote).await?;
        self.wait_for_package_manager_ready(remote).await?;
        let primary_interface = self.detect_primary_interface(remote).await?;
        let server_config = self.render_server_config(&server_private, &client_public);
        let server_tunnel_host = strip_cidr(&self.defaults.server_tunnel_ip);
        let client_config = self.render_client_config(
            &client_private,
            &server_public,
            endpoint_host,
            endpoint_port,
            &format!("{server_tunnel_host}/32"),
        );

        self.setup_queue_management_persistent(remote).await?;
        self.setup_network_tuning_persistent(remote).await?;

        let packages_needed = self.check_wireguard_packages_needed(remote).await?;

        let escaped_server_config = shell_single_quote_escape(&server_config);

        // Write config file first (doesn't need dpkg lock)
        let config_script = format!(
            "sudo mkdir -p /etc/wireguard && sudo bash -lc 'cat > /etc/wireguard/{}.conf <<\"EOF\"\n{}\nEOF'",
            self.defaults.server_interface_name,
            escaped_server_config
        );

        let remote_write_config = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&config_script, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if remote_write_config.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "WireGuard config write failed: {}",
                remote_write_config.stderr
            )));
        }

        if !packages_needed.is_empty() {
            info!(
                "Missing WireGuard packages on remote, attempting install: {}",
                packages_needed.join(", ")
            );
            self.install_wireguard_packages(remote, &packages_needed)
                .await?;
        }

        // Set up firewall rules only after ufw is guaranteed to be installed
        let allowed_client_ip = strip_cidr(&self.defaults.client_tunnel_ip);
        self.setup_firewall_rules(remote, &primary_interface, &allowed_client_ip)
            .await?;

        let bring_up = {
            let remote = remote.clone();
            let iface = self.defaults.server_interface_name.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &format!(
                        "sudo systemctl enable wg-quick@{iface} && (sudo wg-quick down {iface} 2>/dev/null || true) && sudo wg-quick up {iface} && ip a show {iface} && sudo wg show"
                    ),
                    Duration::from_secs(120),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if bring_up.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "WireGuard interface did not start: {}",
                bring_up.stderr
            )));
        }

        self.setup_wireguard_routing(remote, &primary_interface)
            .await?;

        self.apply_network_tuning(remote, &primary_interface)
            .await?;
        self.validate_network_tuning(remote, &primary_interface)
            .await?;

        fs::write(&local_config_path, client_config).await?;

        Ok(WireGuardProvisionResult {
            server_ip: strip_cidr(&self.defaults.server_tunnel_ip),
            client_ip: strip_cidr(&self.defaults.client_tunnel_ip),
            server_public_key: server_public,
            client_public_key: client_public,
            client_config_path: local_config_path,
        })
    }

    async fn load_existing_remote_identity(
        &self,
        remote: &RemoteExec,
    ) -> AppResult<Option<ExistingRemoteIdentity>> {
        let iface = self.defaults.server_interface_name.clone();
        let command = format!(
            "sudo test -f /etc/wireguard/{iface}.conf && sudo cat /etc/wireguard/{iface}.conf"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(30)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Ok(None);
        }

        let server_private_key =
            match parse_wireguard_config_value(&output.stdout, "Interface", "PrivateKey") {
                Some(value) => value,
                None => return Ok(None),
            };
        let client_public_key =
            match parse_wireguard_config_value(&output.stdout, "Peer", "PublicKey") {
                Some(value) => value,
                None => return Ok(None),
            };

        Ok(Some(ExistingRemoteIdentity {
            server_private_key,
            client_public_key,
        }))
    }

    async fn check_wireguard_packages_needed(&self, remote: &RemoteExec) -> AppResult<Vec<String>> {
        let query = REQUIRED_REMOTE_WIREGUARD_PACKAGES.join(" ");
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
            return Ok(REQUIRED_REMOTE_WIREGUARD_PACKAGES
                .iter()
                .map(|s| s.to_string())
                .collect());
        }

        let installed: std::collections::HashSet<_> = check
            .stdout
            .lines()
            .map(|l| l.trim().to_lowercase())
            .collect();

        let missing: Vec<String> = REQUIRED_REMOTE_WIREGUARD_PACKAGES
            .iter()
            .filter(|p| !installed.contains(&p.to_lowercase()))
            .map(|s| s.to_string())
            .collect();

        if missing.is_empty() {
            info!("All WireGuard packages already installed, skipping apt-get");
        } else {
            info!("Missing WireGuard packages: {}", missing.join(", "));
        }

        Ok(missing)
    }

    async fn install_wireguard_packages(
        &self,
        remote: &RemoteExec,
        packages_needed: &[String],
    ) -> AppResult<()> {
        // Permanently neuter unattended-upgrades so it can't re-acquire the lock
        let disable_auto_upgrades = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "sudo systemctl stop unattended-upgrades 2>/dev/null || true; sudo systemctl disable --now unattended-upgrades 2>/dev/null || true; sudo systemctl mask unattended-upgrades 2>/dev/null || true; sudo apt-get remove -y unattended-upgrades 2>/dev/null || true; sudo rm -f /etc/apt/apt.conf.d/20auto-upgrades /etc/apt/apt.conf.d/50unattended-upgrades 2>/dev/null || true; echo 'AUTO_UPGRADES_DISABLED'",
                    Duration::from_secs(30),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if disable_auto_upgrades.status_code != 0 {
            warn!(
                "Failed to disable auto-upgrades (continuing): stdout: {} | stderr: {}",
                disable_auto_upgrades.stdout.trim(),
                disable_auto_upgrades.stderr.trim()
            );
        } else {
            info!(
                "Auto-upgrades disabled: {}",
                disable_auto_upgrades.stdout.trim()
            );
        }

        // wait_for_dpkg_lock_with_message handles all cleanup internally (Option C)
        let lock_acquired = self.wait_for_dpkg_lock_with_message(remote, 120).await?;
        if !lock_acquired {
            return Err(AppError::Provisioning(
                "Package manager is locked by another process (likely unattended-upgrades). \
                Waiting timed out after 60 seconds. Please try again in a few minutes when \
                system updates have finished. Alternatively, you can SSH into the instance and \
                run: sudo systemctl stop unattended-upgrades && sudo dpkg --configure -a"
                    .to_string(),
            ));
        }

        let update = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "sudo DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=60 update",
                    Duration::from_secs(APT_UPDATE_TIMEOUT_SECS),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if update.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed apt-get update for WireGuard dependencies (exit {}): stdout: {} | stderr: {}",
                update.status_code,
                update.stdout.trim(),
                update.stderr.trim()
            )));
        }

        let install_command = format!(
            "sudo DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=60 -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 install -y {}",
            packages_needed.join(" ")
        );

        let install = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    &install_command,
                    Duration::from_secs(APT_INSTALL_TIMEOUT_SECS),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if install.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed installing WireGuard dependencies (exit {}): stdout: {} | stderr: {}",
                install.status_code,
                install.stdout.trim(),
                install.stderr.trim()
            )));
        }

        let remaining = self.check_wireguard_packages_needed(remote).await?;
        if !remaining.is_empty() {
            return Err(AppError::Provisioning(format!(
                "WireGuard package install completed but required packages are still missing: {}",
                remaining.join(", ")
            )));
        }

        Ok(())
    }

    async fn setup_cpu_governor(&self, remote: &RemoteExec) -> AppResult<()> {
        let cpu_governor_service = r#"[Unit]
Description=Set CPU governor to performance
After=multi-user.target
ConditionPathExistsGlob=/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

[Service]
Type=oneshot
ExecStart=/bin/bash -lc "for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > \"$cpu\" 2>/dev/null || true; done"

[Install]
WantedBy=multi-user.target
"#;

        let escaped = shell_single_quote_escape(cpu_governor_service);
        let command = format!(
            "sudo bash -lc 'cat > /etc/systemd/system/set-cpu-governor.service <<\"EOF\"\n{}\nEOF'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to create CPU governor service: {}",
                output.stderr
            )));
        }

        let enable = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("sudo systemctl daemon-reload && sudo systemctl enable set-cpu-governor && sudo systemctl start set-cpu-governor", Duration::from_secs(30))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if enable.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to enable CPU governor: {}",
                enable.stderr
            )));
        }

        Ok(())
    }

    async fn setup_firewall_rules(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
        allowed_client_ip: &str,
    ) -> AppResult<()> {
        let firewall_setup = format!(
            r#"#!/bin/bash
set -euo pipefail

# Enable UFW if needed but avoid destructive reset of existing policy
ufw --force enable >/dev/null 2>&1 || true

# Allow SSH
ufw status | grep -q "22/tcp" || ufw allow 22/tcp comment 'SSH'

# Allow WireGuard
ufw status | grep -q "{}/udp" || ufw allow {}/udp comment 'WireGuard'

# Ensure WireGuard response traffic can always exit
ufw status | grep -q "{}/udp (out)" || ufw allow out {}/udp comment 'WireGuard outbound'

# Allow forwarding between public NIC and WireGuard interface
ufw route allow in on {} out on {} comment 'WG ingress forward' >/dev/null 2>&1 || true
ufw route allow in on {} out on {} comment 'WG egress forward' >/dev/null 2>&1 || true

# Allow ICMP (ping) via iptables directly — UFW ICMP syntax is inconsistent across versions
iptables -C INPUT -p icmp --icmp-type echo-request -j ACCEPT 2>/dev/null || iptables -A INPUT -p icmp --icmp-type echo-request -j ACCEPT
iptables -C INPUT -p icmp --icmp-type echo-reply -j ACCEPT 2>/dev/null || iptables -A INPUT -p icmp --icmp-type echo-reply -j ACCEPT
iptables -C OUTPUT -p icmp --icmp-type echo-request -j ACCEPT 2>/dev/null || iptables -A OUTPUT -p icmp --icmp-type echo-request -j ACCEPT
iptables -C OUTPUT -p icmp --icmp-type echo-reply -j ACCEPT 2>/dev/null || iptables -A OUTPUT -p icmp --icmp-type echo-reply -j ACCEPT

# Remove legacy broad Sunshine allow rules (best effort)
ufw --force delete allow in on {} to any port 47984,47989,47990,47991,48010 proto tcp >/dev/null 2>&1 || true
ufw --force delete allow in on {} to any port 47998,47999,48000,48002 proto udp >/dev/null 2>&1 || true
for port in 47984 47989 47990 47991 48010; do
  ufw --force delete allow "$port/tcp" >/dev/null 2>&1 || true
done
for port in 47998 47999 48000 48002; do
  ufw --force delete allow "$port/udp" >/dev/null 2>&1 || true
done

# Sunshine ports restricted to the configured WireGuard client only
ufw status | grep -q "from {}/32 to any port 47984,47989,47990,47991,48010 proto tcp" || ufw allow in on {} from {}/32 to any port 47984,47989,47990,47991,48010 proto tcp comment 'Sunshine TCP over WireGuard (single client)'
ufw status | grep -q "from {}/32 to any port 47998,47999,48000,48002 proto udp" || ufw allow in on {} from {}/32 to any port 47998,47999,48000,48002 proto udp comment 'Sunshine UDP over WireGuard (single client)'

# Deny all other Sunshine access paths (best effort; source rule above stays higher priority)
ufw status | grep -q "deny in on {} to any port 47984,47989,47990,47991,48010 proto tcp" || ufw deny in on {} to any port 47984,47989,47990,47991,48010 proto tcp >/dev/null 2>&1 || true
ufw status | grep -q "deny in on {} to any port 47998,47999,48000,48002 proto udp" || ufw deny in on {} to any port 47998,47999,48000,48002 proto udp >/dev/null 2>&1 || true
"#,
            self.defaults.listen_port,
            self.defaults.listen_port,
            self.defaults.listen_port,
            self.defaults.listen_port,
            self.defaults.server_interface_name,
            primary_interface,
            primary_interface,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name,
            allowed_client_ip,
            self.defaults.server_interface_name,
            allowed_client_ip,
            allowed_client_ip,
            self.defaults.server_interface_name,
            allowed_client_ip,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name,
            self.defaults.server_interface_name
        );

        let escaped = shell_single_quote_escape(&firewall_setup);
        let command = format!(
            "sudo bash -lc 'cat > /tmp/setup-firewall.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/setup-firewall.sh\n/tmp/setup-firewall.sh'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(90)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup firewall: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn setup_network_tuning_persistent(&self, remote: &RemoteExec) -> AppResult<()> {
        let wg_iface = self.defaults.server_interface_name.clone();
        let sysctl_config = format!(
            "# Network tuning for low-latency streaming
net.core.rmem_max=134217728
net.core.wmem_max=134217728
net.ipv4.tcp_rmem=4096 87380 134217728
net.ipv4.tcp_wmem=4096 65536 134217728
net.core.netdev_max_backlog=5000
net.ipv4.tcp_fastopen=3
net.ipv4.tcp_timestamps=0
net.ipv4.tcp_sack=1
net.ipv4.ip_forward=1
net.ipv4.conf.all.rp_filter=0
net.ipv4.conf.default.rp_filter=0
net.ipv4.conf.{wg_iface}.rp_filter=0
"
        );

        let escaped = shell_single_quote_escape(&sysctl_config);
        let command = format!(
            "sudo bash -lc 'cat > /etc/sysctl.d/99-noland-network.conf <<\"EOF\"\n{}\nEOF\nsudo sysctl --system >/dev/null'",
            escaped
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup network tuning: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    fn render_server_config(&self, server_private: &str, client_public: &str) -> String {
        format!(
            "[Interface]\nAddress = {}\nListenPort = {}\nPrivateKey = {}\nMTU = {}\n\n[Peer]\nPublicKey = {}\nAllowedIPs = {}\n",
            self.defaults.server_tunnel_ip,
            self.defaults.listen_port,
            server_private,
            self.defaults.tunnel_mtu,
            client_public,
            self.defaults.client_tunnel_ip,
        )
    }

    async fn setup_wireguard_routing(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let nic = primary_interface.to_string();
        // Note: FORWARD rules are handled by ufw route allow in setup_firewall_rules.
        // Only NAT/MASQUERADE remains here because UFW cannot configure it.
        let command = format!(
            "sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null && sudo iptables -t nat -C POSTROUTING -o {nic} -j MASQUERADE 2>/dev/null || sudo iptables -t nat -A POSTROUTING -o {nic} -j MASQUERADE"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(90)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup WireGuard routing/NAT: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    fn render_client_config(
        &self,
        client_private: &str,
        server_public: &str,
        endpoint_host: &str,
        endpoint_port: u16,
        allowed_ips: &str,
    ) -> String {
        format!(
            "[Interface]\nAddress = {}\nPrivateKey = {}\nListenPort = {}\nMTU = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}:{}\nAllowedIPs = {}\nPersistentKeepalive = {}\n",
            self.defaults.client_tunnel_ip,
            client_private,
            self.defaults.client_listen_port,
            self.defaults.tunnel_mtu,
            server_public,
            endpoint_host,
            endpoint_port,
            allowed_ips,
            self.defaults.persistent_keepalive_secs,
        )
    }

    async fn cleanup_existing_wireguard(&self, remote: &RemoteExec) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let command = format!(
            "sudo bash -lc 'target=\"{iface}\"; for dir in /sys/class/net/wg*; do [ -e \"$dir\" ] || continue; dev=$(basename \"$dir\"); if [ \"$dev\" != \"$target\" ]; then systemctl stop \"wg-quick@$dev\" >/dev/null 2>&1 || true; systemctl disable \"wg-quick@$dev\" >/dev/null 2>&1 || true; wg-quick down \"$dev\" >/dev/null 2>&1 || true; ip link delete \"$dev\" >/dev/null 2>&1 || true; fi; done'"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed clearing existing WireGuard interfaces: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn detect_primary_interface(&self, remote: &RemoteExec) -> AppResult<String> {
        let route_get = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("ip route get 1.1.1.1", Duration::from_secs(20))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if route_get.status_code == 0 {
            if let Some(iface) = route_get.stdout.lines().find_map(parse_default_route_dev) {
                if !iface.is_empty() {
                    return Ok(iface);
                }
            }
        }

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("ip -o route show default", Duration::from_secs(40))
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to detect primary egress interface: {}",
                output.stderr.trim()
            )));
        }

        let iface = output
            .stdout
            .lines()
            .find_map(parse_default_route_dev)
            .ok_or_else(|| {
                AppError::Provisioning(
                    "Could not detect primary egress interface from default route".to_string(),
                )
            })?;

        Ok(iface)
    }

    async fn setup_queue_management_persistent(&self, remote: &RemoteExec) -> AppResult<()> {
        let script = format!(
            r#"#!/usr/bin/env bash
set -uo pipefail

EGRESS_IF="$(ip route get 1.1.1.1 | awk '{{for (i=1;i<=NF;i++) if ($i=="dev") {{print $(i+1); exit}}}}')"
[ -n "$EGRESS_IF" ] || {{ echo "No egress interface detected"; exit 1; }}

QOS_MODE="{qos_mode}"
QOS_BANDWIDTH_MBIT="{qos_bandwidth_mbit}"
QOS_DIFFSERV_PROFILE="{qos_diffserv_profile}"
DSCP_ENABLED="{dscp_enabled}"

detect_bandwidth_mbit() {{
  if [ "$QOS_BANDWIDTH_MBIT" -gt 0 ] 2>/dev/null; then
    printf '%s' "$QOS_BANDWIDTH_MBIT"
    return
  fi

  local speed
  speed="$(cat "/sys/class/net/$EGRESS_IF/speed" 2>/dev/null || true)"
  if [ -n "$speed" ] && [ "$speed" -gt 0 ] 2>/dev/null; then
    awk -v raw="$speed" 'BEGIN {{ capped=int(raw * 0.90); if (capped < 100) capped=100; if (capped > 5000) capped=5000; printf "%d", capped }}'
    return
  fi

  printf '900'
}}

apply_dscp_rules() {{
  [ "$DSCP_ENABLED" = "1" ] || return 0

  if command -v iptables >/dev/null 2>&1; then
    iptables -t mangle -D OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4 2>/dev/null || true
    iptables -t mangle -D OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true
    iptables -t mangle -D OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true
    iptables -t mangle -A OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4
    iptables -t mangle -A OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21
    iptables -t mangle -A OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21
    return 0
  fi

  if command -v nft >/dev/null 2>&1; then
    nft list table inet noland_qos >/dev/null 2>&1 || nft add table inet noland_qos
    nft 'list chain inet noland_qos output' >/dev/null 2>&1 || nft 'add chain inet noland_qos output {{ type route hook output priority mangle; policy accept; }}'
    nft flush chain inet noland_qos output
    nft add rule inet noland_qos output udp dport 47998-48010 ip dscp set cs4
    nft add rule inet noland_qos output tcp dport {{ 47989, 47990, 47984 }} ip dscp set af21
    nft add rule inet noland_qos output udp dport {{ 47989, 47990, 47984 }} ip dscp set af21
  fi
}}

RATE_MBIT="$(detect_bandwidth_mbit)"

if [ "$QOS_MODE" = "cake" ] && tc qdisc replace dev "$EGRESS_IF" root cake bandwidth "${{RATE_MBIT}}mbit" "$QOS_DIFFSERV_PROFILE" nat 2>/dev/null; then
  echo "Applied CAKE on $EGRESS_IF at ${{RATE_MBIT}}mbit with $QOS_DIFFSERV_PROFILE"
else
  tc qdisc replace dev "$EGRESS_IF" root fq_codel || true
  echo "Applied fq_codel fallback on $EGRESS_IF"
fi

apply_dscp_rules || true
tc -s qdisc show dev "$EGRESS_IF"
iptables -t mangle -S OUTPUT 2>/dev/null | grep -E '47998:48010|47989|47990|47984' || true
"#,
            qos_mode = self.defaults.qos_mode,
            qos_bandwidth_mbit = self.defaults.qos_bandwidth_mbit,
            qos_diffserv_profile = self.defaults.qos_diffserv_profile,
            dscp_enabled = if self.defaults.dscp_enabled { 1 } else { 0 }
        );

        let service = r#"[Unit]
Description=Apply Noland QoS to default-route interface
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/noland-apply-qdisc.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
"#;

        let rollback = r#"#!/usr/bin/env bash
set -euo pipefail

EGRESS_IF="$(ip route get 1.1.1.1 | awk '{for (i=1;i<=NF;i++) if ($i=="dev") {print $(i+1); exit}}')"
[ -n "$EGRESS_IF" ] || { echo "No egress interface detected"; exit 1; }

tc qdisc del dev "$EGRESS_IF" root 2>/dev/null || true
if command -v iptables >/dev/null 2>&1; then
  iptables -t mangle -D OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4 2>/dev/null || true
  iptables -t mangle -D OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true
  iptables -t mangle -D OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true
fi
if command -v nft >/dev/null 2>&1; then
  nft delete table inet noland_qos 2>/dev/null || true
fi
tc qdisc show dev "$EGRESS_IF"
"#;

        let command = format!(
            "sudo bash -lc 'cat > /usr/local/bin/noland-apply-qdisc.sh <<\"EOF\"\n{}\nEOF\nchmod +x /usr/local/bin/noland-apply-qdisc.sh\ncat > /usr/local/bin/noland-rollback-qdisc.sh <<\"EOF\"\n{}\nEOF\nchmod +x /usr/local/bin/noland-rollback-qdisc.sh\ncat > /etc/systemd/system/noland-qdisc.service <<\"EOF\"\n{}\nEOF\nsystemctl daemon-reload\nsystemctl enable --now noland-qdisc.service\n/usr/local/bin/noland-apply-qdisc.sh'",
            shell_single_quote_escape(&script),
            shell_single_quote_escape(rollback),
            shell_single_quote_escape(service)
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(120)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to setup persistent queue management: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn apply_network_tuning(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let nic = primary_interface.to_string();
        let qos_mode = self.defaults.qos_mode.clone();
        let qos_bandwidth_mbit = self.defaults.qos_bandwidth_mbit;
        let qos_diffserv_profile = self.defaults.qos_diffserv_profile.clone();
        let dscp_enabled = if self.defaults.dscp_enabled { 1 } else { 0 };

        let command = format!(
            "sudo bash -lc 'RATE_MBIT={qos_bandwidth_mbit}; if [ \"$RATE_MBIT\" -le 0 ] 2>/dev/null; then LINK_SPEED=$(cat /sys/class/net/{nic}/speed 2>/dev/null || true); if [ -n \"$LINK_SPEED\" ] && [ \"$LINK_SPEED\" -gt 0 ] 2>/dev/null; then RATE_MBIT=$(awk -v raw=\"$LINK_SPEED\" \"BEGIN {{ capped=int(raw * 0.90); if (capped < 100) capped=100; if (capped > 5000) capped=5000; printf \\\"%d\\\", capped }}\"); else RATE_MBIT=900; fi; fi; if [ \"{qos_mode}\" = \"cake\" ] && tc qdisc replace dev {nic} root cake bandwidth \"${{RATE_MBIT}}mbit\" {qos_diffserv_profile} nat 2>/dev/null; then echo qos=cake rate=${{RATE_MBIT}}mbit; else tc qdisc replace dev {nic} root fq_codel; echo qos=fq_codel; fi; if [ \"{dscp_enabled}\" = \"1\" ]; then if command -v iptables >/dev/null 2>&1; then iptables -t mangle -D OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4 2>/dev/null || true; iptables -t mangle -D OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true; iptables -t mangle -D OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true; iptables -t mangle -A OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4; iptables -t mangle -A OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21; iptables -t mangle -A OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21; fi; fi; (sudo ethtool -C {nic} rx-usecs 0 tx-usecs 0 || true); sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null; sudo sysctl -w net.ipv4.conf.all.rp_filter=0 >/dev/null; sudo sysctl -w net.ipv4.conf.default.rp_filter=0 >/dev/null; sudo sysctl -w net.ipv4.conf.{nic}.rp_filter=0 >/dev/null; sudo sysctl -w net.ipv4.conf.{iface}.rp_filter=0 >/dev/null; (sudo systemctl stop tailscaled 2>/dev/null || true)'"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(120)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed applying network pacing/tuning: {}",
                output.stderr
            )));
        }

        Ok(())
    }

    async fn validate_network_tuning(
        &self,
        remote: &RemoteExec,
        primary_interface: &str,
    ) -> AppResult<()> {
        let iface = self.defaults.server_interface_name.clone();
        let nic = primary_interface.to_string();
        let command = format!(
            "ip a show {iface} && ip route && tc -s qdisc show dev {iface} && tc -s qdisc show dev {nic} && (iptables -t mangle -S OUTPUT 2>/dev/null | grep -E '47998:48010|47989|47990|47984' || true) && if pgrep -x sunshine >/dev/null; then taskset -p $(pgrep -x sunshine | head -n 1); fi"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(120)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Network tuning verification failed: {}",
                output.stderr
            )));
        }

        Ok(())
    }
}

pub fn setup_local_wireguard_client(config_path: &Path) -> AppResult<String> {
    if !config_path.exists() {
        return Err(AppError::NotFound(format!(
            "WireGuard client config not found at {}",
            config_path.display()
        )));
    }

    ensure_local_wireguard_tools()?;

    normalize_wireguard_client_allowed_ips(config_path)?;

    #[cfg(target_os = "macos")]
    {
        setup_local_wireguard_client_macos(config_path)
    }

    #[cfg(target_os = "linux")]
    {
        setup_local_wireguard_client_linux(config_path)
    }

    #[cfg(target_os = "windows")]
    {
        setup_local_wireguard_client_windows(config_path)
    }
}

pub fn reconnect_local_wireguard_client(config_path: &Path) -> AppResult<String> {
    if !config_path.exists() {
        return Err(AppError::NotFound(format!(
            "WireGuard client config not found at {}",
            config_path.display()
        )));
    }

    ensure_local_wireguard_tools()?;

    normalize_wireguard_client_allowed_ips(config_path)?;

    #[cfg(target_os = "macos")]
    {
        reconnect_local_wireguard_client_macos(config_path)
    }

    #[cfg(target_os = "linux")]
    {
        reconnect_local_wireguard_client_linux(config_path)
    }

    #[cfg(target_os = "windows")]
    {
        reconnect_local_wireguard_client_windows(config_path)
    }
}

pub fn remove_local_wireguard_config(config_path: &Path) -> AppResult<()> {
    let Some(parent) = config_path.parent() else {
        return Ok(());
    };

    let parent_name = parent
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let is_instance_specific_dir = parent_name.chars().all(|c| c.is_ascii_digit());
    if is_instance_specific_dir {
        if parent.exists() {
            std::fs::remove_dir_all(parent).map_err(|error| {
                AppError::Command(format!(
                    "Failed removing WireGuard config directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        return Ok(());
    }

    let repair_request_path = config_path.with_extension("repair.request");
    let repair_status_path = config_path.with_extension("repair.status");

    for path in [
        repair_request_path.as_path(),
        repair_status_path.as_path(),
        config_path,
    ] {
        if !path.exists() {
            continue;
        }

        std::fs::remove_file(path).map_err(|error| {
            AppError::Command(format!(
                "Failed removing WireGuard artifact {}: {error}",
                path.display()
            ))
        })?;
    }

    Ok(())
}

pub fn read_local_wireguard_show_output() -> AppResult<String> {
    let os = OsDetection::new();
    if !os.command_exists("wg") {
        return Ok(String::new());
    }

    let wg_program = os
        .resolve_command_path("wg")
        .unwrap_or_else(|| "wg".to_string());
    let mut command = Command::new(wg_program);
    os.with_augmented_path(&mut command);

    let output = command
        .arg("show")
        .output()
        .map_err(|error| AppError::Command(format!("Failed to run wg show: {error}")))?;

    if !output.status.success() {
        return Ok(String::new());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn normalize_wireguard_state_from_disk(
    state: &mut crate::models::app_state::PersistedAppState,
    app_data_dir: &Path,
) -> AppResult<bool> {
    let mut changed = false;
    if let Some(instance_id) = state.instance.instance_id {
        let legacy_path = legacy_local_config_path(app_data_dir);
        let instance_path = instance_local_config_path(app_data_dir, instance_id);
        if legacy_path.exists() && !instance_path.exists() {
            if let Some(parent) = instance_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    AppError::Command(format!(
                        "Failed creating WireGuard config directory {}: {error}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::copy(&legacy_path, &instance_path).map_err(|error| {
                AppError::Command(format!(
                    "Failed migrating legacy WireGuard config {} -> {}: {error}",
                    legacy_path.display(),
                    instance_path.display()
                ))
            })?;
            changed = true;
        }
    }
    let current_instance_id = state.instance.instance_id;
    let persisted_config_path = current_instance_id
        .map(|instance_id| instance_local_config_path(app_data_dir, instance_id))
        .filter(|path| path.exists())
        .or_else(|| {
            state
                .provisioned_servers
                .iter()
                .find(|record| Some(record.instance_id) == current_instance_id)
                .and_then(|record| {
                    let path = PathBuf::from(record.wireguard_config_path.clone());
                    if path.exists() {
                        Some(path)
                    } else {
                        None
                    }
                })
        })
        .unwrap_or_else(|| legacy_local_config_path(app_data_dir));

    if let Some(record) = state
        .instance
        .instance_id
        .and_then(|instance_id| {
            state
                .provisioned_servers
                .iter()
                .find(|record| record.instance_id == instance_id)
        })
        .cloned()
    {
        if state.instance.ssh_host.trim().is_empty() && !record.ssh_host.trim().is_empty() {
            state.instance.ssh_host = record.ssh_host;
            changed = true;
        }
        if state.instance.ssh_port == 0 && record.ssh_port != 0 {
            state.instance.ssh_port = record.ssh_port;
            changed = true;
        }
        if state.wireguard.server_ip.trim().is_empty()
            && !record.wireguard_server_ip.trim().is_empty()
        {
            state.wireguard.server_ip = record.wireguard_server_ip;
            changed = true;
        }
        if state.wireguard.client_ip.trim().is_empty()
            && !record.wireguard_client_ip.trim().is_empty()
        {
            state.wireguard.client_ip = record.wireguard_client_ip;
            changed = true;
        }
        if state.wireguard.server_public_key.trim().is_empty()
            && !record.wireguard_server_public_key.trim().is_empty()
        {
            state.wireguard.server_public_key = record.wireguard_server_public_key;
            changed = true;
        }
        if state.wireguard.client_public_key.trim().is_empty()
            && !record.wireguard_client_public_key.trim().is_empty()
        {
            state.wireguard.client_public_key = record.wireguard_client_public_key;
            changed = true;
        }
        if state.wireguard.config_path.trim().is_empty()
            && !record.wireguard_config_path.trim().is_empty()
        {
            state.wireguard.config_path = record.wireguard_config_path;
            changed = true;
        }
        if state.moonlight.host_address.trim().is_empty()
            && !record.moonlight_host_address.trim().is_empty()
        {
            state.moonlight.host_address = record.moonlight_host_address;
            changed = true;
        }
    }

    if persisted_config_path.exists() {
        let persisted_path_string = persisted_config_path.display().to_string();
        if state.wireguard.config_path != persisted_path_string {
            state.wireguard.config_path = persisted_path_string.clone();
            changed = true;
        }

        let expected = load_expected_local_tunnel(&persisted_config_path)?;
        changed |= apply_expected_tunnel_to_state(state, &expected);

        for record in &mut state.provisioned_servers {
            let should_sync_record = current_instance_id
                .map(|instance_id| record.instance_id == instance_id)
                .unwrap_or(false);
            if !should_sync_record {
                continue;
            }

            if record.wireguard_config_path != persisted_path_string {
                record.wireguard_config_path = persisted_path_string.clone();
                changed = true;
            }
            if record.wireguard_server_ip.trim().is_empty() {
                record.wireguard_server_ip = expected.server_ip.clone();
                changed = true;
            }
            if record.wireguard_client_ip.trim().is_empty() {
                record.wireguard_client_ip = expected.client_ip.clone();
                changed = true;
            }
            if record.wireguard_server_public_key.trim().is_empty() {
                record.wireguard_server_public_key = expected.peer_public_key.clone();
                changed = true;
            }
            if record.wireguard_client_public_key.trim().is_empty() {
                record.wireguard_client_public_key = expected.interface_public_key.clone();
                changed = true;
            }
            if record.moonlight_host_address.trim().is_empty()
                && !expected.server_ip.trim().is_empty()
            {
                record.moonlight_host_address = expected.server_ip.clone();
                changed = true;
            }
        }
    }

    if state.moonlight.host_address.trim().is_empty()
        && !state.wireguard.server_ip.trim().is_empty()
    {
        state.moonlight.host_address = state.wireguard.server_ip.clone();
        changed = true;
    }

    Ok(changed)
}

pub async fn maintain_persisted_local_tunnel(context: &AppContext) -> AppResult<()> {
    if context.wireguard_mutation_in_progress() {
        return Ok(());
    }

    let snapshot = context.load_state().await;
    if snapshot.instance.instance_id.is_none() || snapshot.wireguard.server_ip.trim().is_empty() {
        return Ok(());
    }

    let Some(config_path) = resolve_active_wireguard_config_path(&snapshot) else {
        return Ok(());
    };

    cleanup_stale_wireguard_artifacts(&config_path);

    #[cfg(target_os = "macos")]
    if !Path::new(MACOS_WIREGUARD_HELPER_PLIST_PATH).exists() {
        return Ok(());
    }

    let expected = load_expected_local_tunnel(&config_path)?;
    let runtime_before = collect_local_wireguard_runtime_state(Some(&expected.peer_public_key))?;
    let handshake_ok = has_recent_handshake(&runtime_before.latest_handshake);
    let config_mismatch = !local_tunnel_runtime_matches_expected(&runtime_before, &expected);
    let ping_ok = can_ping_tunnel_host(&expected.server_ip);
    let runtime_empty = runtime_before.interface_name.trim().is_empty()
        && runtime_before.peer_public_key.trim().is_empty()
        && runtime_before.allowed_ips.trim().is_empty()
        && runtime_before.latest_handshake.trim().is_empty();

    #[cfg(target_os = "macos")]
    let helper_status = read_macos_helper_status(&config_path);
    #[cfg(target_os = "macos")]
    if matches!(
        macos_helper_generation(),
        Some(MacosHelperGeneration::WatchRequests)
    ) {
        if helper_status
            .as_ref()
            .is_some_and(helper_status_recently_succeeded)
        {
            let _ = note_monitor_conflict_candidate(false);
            clear_macos_monitor_repair_cooldown();
            let _ = context
                .update_state(|state| {
                    state.wireguard.config_path = config_path.display().to_string();
                    apply_expected_tunnel_to_state(state, &expected);
                })
                .await;
            return Ok(());
        }
    }

    let tunnel_healthy = !config_mismatch
        && (handshake_ok || (ping_ok && !runtime_before.interface_name.trim().is_empty()));

    if tunnel_healthy {
        let _ = note_monitor_repair_health(true);
        let _ = note_monitor_conflict_candidate(false);
        #[cfg(target_os = "macos")]
        clear_macos_monitor_repair_cooldown();

        let _ = context
            .update_state(|state| {
                state.wireguard.config_path = config_path.display().to_string();
                apply_expected_tunnel_to_state(state, &expected);
                if !runtime_before.interface_name.trim().is_empty() {
                    state.wireguard.last_runtime_interface = runtime_before.interface_name.clone();
                }
            })
            .await;

        return Ok(());
    }

    let connectivity_missing = !tunnel_healthy;
    let hard_mismatch = config_mismatch;
    #[cfg(target_os = "macos")]
    let helper_generation = macos_helper_generation();
    #[cfg(target_os = "macos")]
    let needs_repair = match helper_generation {
        Some(MacosHelperGeneration::WatchRequests) => config_mismatch || connectivity_missing,
        Some(MacosHelperGeneration::Legacy) => false,
        None => config_mismatch || connectivity_missing,
    };
    #[cfg(not(target_os = "macos"))]
    let needs_repair = config_mismatch || connectivity_missing;

    #[cfg(target_os = "macos")]
    if matches!(helper_generation, Some(MacosHelperGeneration::Legacy)) && config_mismatch {
        if can_attempt_macos_monitor_repair(false) {
            warn!(
                "Legacy Noland WireGuard helper detected. Noland no longer auto-manages local WireGuard; open the WireGuard app and manage the tunnel manually (peer_match={}, allowed_ips_match={}, handshake_missing={}, ping_ok={})",
                runtime_before.peer_public_key == expected.peer_public_key,
                runtime_before.allowed_ips == expected.allowed_ips,
                !handshake_ok,
                ping_ok
            );
        }
        return Ok(());
    }

    if needs_repair {
        let failure_streak = note_monitor_repair_health(false);
        if !hard_mismatch && failure_streak < MONITOR_REPAIR_FAILURE_STREAK_THRESHOLD {
            if failure_streak == 1 || failure_streak % MONITOR_CONFLICT_WARN_EVERY == 0 {
                warn!(
                    "WireGuard health monitor observed an unhealthy check. Noland no longer auto-repairs the local tunnel; open the WireGuard app and manage the tunnel manually if needed (streak={}/{}, peer_match={}, allowed_ips_match={}, handshake_missing={}, ping_ok={})",
                    failure_streak,
                    MONITOR_REPAIR_FAILURE_STREAK_THRESHOLD,
                    runtime_before.peer_public_key == expected.peer_public_key,
                    runtime_before.allowed_ips == expected.allowed_ips,
                    !handshake_ok,
                    ping_ok
                );
            }
            return Ok(());
        }

        let conflict_candidate = config_mismatch && !runtime_empty && !handshake_ok && ping_ok;
        let conflict_streak = note_monitor_conflict_candidate(conflict_candidate);
        if runtime_empty {
            let _ = note_monitor_conflict_candidate(false);
        }
        if let Some(streak) = conflict_streak {
            if streak >= MONITOR_CONFLICT_WARN_EVERY && streak % MONITOR_CONFLICT_WARN_EVERY == 0 {
                warn!(
                    "WireGuard health monitor repeatedly sees reachability to the tunnel host but the active local tunnel does not match the saved Noland identity. This usually means another tunnel/controller owns the interface (for example WireGuard.app). streak={} expected_peer={} active_peer={} expected_allowed_ips={} active_allowed_ips={}",
                    streak,
                    expected.peer_public_key,
                    runtime_before.peer_public_key,
                    expected.allowed_ips,
                    runtime_before.allowed_ips
                );
            }
        }

        if runtime_empty {
            #[cfg(target_os = "macos")]
            if let Some(status) = helper_status.as_ref() {
                match status.kind {
                    MacosHelperStatusKind::Error => warn!(
                        "WireGuard health monitor cannot see local WireGuard runtime from app context and the installed helper last reported an error. Noland will not auto-repair; open the WireGuard app and manage the tunnel manually: {}",
                        if status.message.trim().is_empty() {
                            "WireGuard helper did not report a specific failure."
                        } else {
                            status.message.as_str()
                        }
                    ),
                    MacosHelperStatusKind::Invalid => warn!(
                        "WireGuard health monitor cannot see local WireGuard runtime from app context, and the helper status file is invalid or incomplete. Noland will not auto-repair; open the WireGuard app and manage the tunnel manually."
                    ),
                    _ => {}
                }
            }
            #[cfg(not(target_os = "macos"))]
            warn!(
                "WireGuard health monitor cannot see any local WireGuard runtime yet. Noland will not auto-repair; open the WireGuard app and manage the tunnel manually."
            );
        }

        warn!(
            "WireGuard health monitor detected stale local tunnel state, but Noland no longer auto-manages local WireGuard. Open the WireGuard app and manage the tunnel manually (peer_match={}, allowed_ips_match={}, handshake_missing={}, ping_ok={})",
            runtime_before.peer_public_key == expected.peer_public_key,
            runtime_before.allowed_ips == expected.allowed_ips,
            !handshake_ok,
            ping_ok
        );
        return Ok(());
    }

    let _ = context
        .update_state(|state| {
            state.wireguard.config_path = config_path.display().to_string();
            apply_expected_tunnel_to_state(state, &expected);
            if !runtime_before.interface_name.trim().is_empty() {
                state.wireguard.last_runtime_interface = runtime_before.interface_name.clone();
            }
        })
        .await;

    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_local_wireguard_tools() -> AppResult<()> {
    let os = OsDetection::new();
    if os.command_exists("wg") && os.command_exists("wg-quick") {
        let _ = resolve_gotatun_binary()?;
        return Ok(());
    }

    Err(AppError::Command(
        "WireGuard tools are missing (wg/wg-quick). Please install wireguard-tools and gotatun manually, then retry."
            .to_string(),
    ))
}

#[cfg(target_os = "linux")]
fn ensure_local_wireguard_tools() -> AppResult<()> {
    let os = OsDetection::new();
    if os.command_exists("wg") && os.command_exists("wg-quick") {
        let _ = resolve_gotatun_binary()?;
        return Ok(());
    }

    Err(AppError::Command(
        "WireGuard tools are missing (wg/wg-quick). Please install wireguard-tools and gotatun manually, then retry."
            .to_string(),
    ))
}

#[cfg(target_os = "windows")]
fn ensure_local_wireguard_tools() -> AppResult<()> {
    let wg_status = Command::new("where").arg("wg").status();
    let wireguard_exe_status = Command::new("where").arg("wireguard.exe").status();

    let has_wg = wg_status.map(|status| status.success()).unwrap_or(false);
    let has_wireguard_exe = wireguard_exe_status
        .map(|status| status.success())
        .unwrap_or(false);

    if has_wg && has_wireguard_exe {
        return Ok(());
    }

    Err(AppError::Command(
        "WireGuard tools are not installed on Windows. Please install WireGuard from https://wireguard.com/install and retry."
            .to_string(),
    ))
}

fn normalize_wireguard_client_allowed_ips(config_path: &Path) -> AppResult<()> {
    const SCOPED_ALLOWED_IPS: &str = "10.77.0.1/32";

    let original = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading WireGuard client config {}: {error}",
            config_path.display()
        ))
    })?;

    let mut in_peer_section = false;
    let mut in_interface_section = false;
    let mut replaced = false;
    let mut normalized_lines = Vec::with_capacity(original.lines().count() + 2);

    for line in original.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_peer_section = trimmed.eq_ignore_ascii_case("[Peer]");
            in_interface_section = trimmed.eq_ignore_ascii_case("[Interface]");
        }

        if in_interface_section && lower.starts_with("dns") {
            continue;
        }

        if in_peer_section && lower.starts_with("allowedips") {
            normalized_lines.push(format!("AllowedIPs = {SCOPED_ALLOWED_IPS}"));
            replaced = true;
        } else {
            normalized_lines.push(line.to_string());
        }
    }

    if !replaced {
        return Err(AppError::InvalidInput(format!(
            "WireGuard client config {} is missing AllowedIPs in [Peer] section",
            config_path.display()
        )));
    }

    let mut normalized = normalized_lines.join("\n");
    if original.ends_with('\n') {
        normalized.push('\n');
    }

    if normalized != original {
        std::fs::write(config_path, normalized).map_err(|error| {
            AppError::Command(format!(
                "Failed writing normalized WireGuard client config {}: {error}",
                config_path.display()
            ))
        })?;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn setup_local_wireguard_client_macos(config_path: &Path) -> AppResult<String> {
    enforce_single_control_plane_macos(config_path)?;

    let expected = load_expected_local_tunnel(config_path)?;
    if local_tunnel_runtime_is_healthy(&expected) {
        return Ok(
            "WireGuard client tunnel already active on this Mac with the saved Noland tunnel identity"
                .to_string(),
        );
    }

    if matches!(
        macos_helper_generation(),
        Some(MacosHelperGeneration::WatchRequests)
    ) {
        match request_macos_helper_repair(config_path, "setup-local-wireguard-client") {
            Ok(()) => {
                if local_tunnel_runtime_is_healthy(&expected) {
                    return Ok(
                        "WireGuard client tunnel repaired on this Mac using the installed Noland helper"
                            .to_string(),
                    );
                }
            }
            Err(error) => {
                warn!("WireGuard helper setup failed; falling back to hard reconnect: {error}");
            }
        }
    }

    hard_reconnect_local_wireguard_client_macos(config_path, true)
}

#[cfg(target_os = "macos")]
fn reconnect_local_wireguard_client_macos(config_path: &Path) -> AppResult<String> {
    enforce_single_control_plane_macos(config_path)?;

    let expected = load_expected_local_tunnel(config_path)?;
    if local_tunnel_runtime_is_healthy(&expected) {
        return Ok(
            "WireGuard client tunnel already healthy on this Mac with the saved Noland tunnel identity"
                .to_string(),
        );
    }

    if matches!(
        macos_helper_generation(),
        Some(MacosHelperGeneration::WatchRequests)
    ) {
        match request_macos_helper_repair(config_path, "reconnect-local-wireguard-client") {
            Ok(()) => {
                if local_tunnel_runtime_is_healthy(&expected) {
                    return Ok(
                        "WireGuard client tunnel reconnected on this Mac using the installed Noland helper"
                            .to_string(),
                    );
                }
            }
            Err(error) => {
                warn!("WireGuard helper reconnect failed; falling back to hard reconnect: {error}");
            }
        }
    }

    hard_reconnect_local_wireguard_client_macos(config_path, false)
}

#[cfg(target_os = "macos")]
fn hard_reconnect_local_wireguard_client_macos(
    config_path: &Path,
    is_setup: bool,
) -> AppResult<String> {
    let expected = load_expected_local_tunnel(config_path)?;
    let gotatun_bin = resolve_gotatun_binary()?;

    let path = config_path.display().to_string().replace('"', "\\\"");
    let request_path = macos_helper_request_path(config_path)
        .display()
        .to_string()
        .replace('"', "\\\"");
    let status_path = macos_helper_status_path(config_path)
        .display()
        .to_string()
        .replace('"', "\\\"");
    let expected_peer = expected.peer_public_key.replace('"', "\\\"");
    let expected_allowed_ips = expected.allowed_ips.replace('"', "\\\"");
    let expected_server_ip = expected.server_ip.replace('"', "\\\"");
    let expected_endpoint_host = expected.endpoint_host.replace('"', "\\\"");
    let expected_endpoint_port = expected.endpoint_port;
    let gotatun_bin = gotatun_bin.replace('"', "\\\"");
    let repair_script = format!(
        r#"#!/bin/sh
set -eu

export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

SOURCE_CONF_PATH="{path}"
LOCAL_CONF_PATH="{MACOS_LOCAL_CONF_PATH}"
HOMEBREW_CONF_PATH="{MACOS_HOMEBREW_CONF_PATH}"
REQUEST_PATH="{request_path}"
STATUS_PATH="{status_path}"
EXPECTED_PEER="{expected_peer}"
EXPECTED_ALLOWED_IPS="{expected_allowed_ips}"
EXPECTED_SERVER_IP="{expected_server_ip}"
EXPECTED_ENDPOINT_HOST="{expected_endpoint_host}"
EXPECTED_ENDPOINT_PORT="{expected_endpoint_port}"
APP_OWNER=$(stat -f '%Su' "$SOURCE_CONF_PATH" 2>/dev/null || true)
APP_GROUP=$(stat -f '%Sg' "$SOURCE_CONF_PATH" 2>/dev/null || true)
GOTATUN_BIN="{gotatun_bin}"
export WG_QUICK_USERSPACE_IMPLEMENTATION="$GOTATUN_BIN"
export WG_SUDO=1

fix_app_data_ownership() {{
  target_path="$1"
  [ -n "$APP_OWNER" ] || return 0
  [ -n "$APP_GROUP" ] || return 0
  [ -e "$target_path" ] || return 0
  chown "$APP_OWNER:$APP_GROUP" "$target_path" 2>/dev/null || true
}}

write_status() {{
  status="$1"
  message="$2"
  mkdir -p "$(dirname "$STATUS_PATH")"
  fix_app_data_ownership "$(dirname "$STATUS_PATH")"
  printf 'status=%s\ntimestamp=%s\nmessage=%s\n' "$status" "$(date +%s)" "$message" > "$STATUS_PATH"
  chmod 644 "$STATUS_PATH" 2>/dev/null || true
  fix_app_data_ownership "$STATUS_PATH"
}}

compact_output() {{
  printf '%s' "$1" | tr '\n' ' ' | tr '\r' ' '
}}

has_recent_handshake() {{
  latest_handshake="$1"
  [ -n "$latest_handshake" ] && ! printf '%s' "$latest_handshake" | grep -qi 'never'
}}

source_config_matches_expected() {{
  source_path="$1"
  [ -f "$source_path" ] || return 1

  source_peer=$(grep -A 12 '^\[Peer\]' "$source_path" | grep '^PublicKey[[:space:]]*=' | head -n 1 | cut -d= -f2- | tr -d ' \r')
  source_allowed=$(grep -A 12 '^\[Peer\]' "$source_path" | grep '^AllowedIPs[[:space:]]*=' | head -n 1 | cut -d= -f2- | tr -d ' \r')
  source_endpoint=$(grep -A 12 '^\[Peer\]' "$source_path" | grep '^Endpoint[[:space:]]*=' | head -n 1 | cut -d= -f2- | tr -d ' \r')

  [ "$source_peer" = "$EXPECTED_PEER" ] || return 1
  [ "$source_allowed" = "$EXPECTED_ALLOWED_IPS" ] || return 1

  expected_endpoint="$EXPECTED_ENDPOINT_HOST:$EXPECTED_ENDPOINT_PORT"
  [ -n "$source_endpoint" ] || return 1
  [ "$source_endpoint" = "$expected_endpoint" ] || return 1

  return 0
}}

tunnel_matches_expected() {{
  wg_output="$1"
  [ -n "$wg_output" ] \
    && printf '%s\n' "$wg_output" | grep -F "peer: $EXPECTED_PEER" >/dev/null 2>&1 \
    && printf '%s\n' "$wg_output" | grep -F "allowed ips: $EXPECTED_ALLOWED_IPS" >/dev/null 2>&1
}}

tunnel_freshness_status() {{
  wg_output="$1"

  if ! tunnel_matches_expected "$wg_output"; then
    printf 'peer_or_allowed_ips_mismatch'
    return
  fi

  latest_handshake=$(printf '%s\n' "$wg_output" | sed -n 's/^  latest handshake: //p' | head -n 1)
  if has_recent_handshake "$latest_handshake"; then
    printf 'ok'
    return
  fi

  if [ -z "$EXPECTED_SERVER_IP" ] || ping -c 1 -t 3 "$EXPECTED_SERVER_IP" >/dev/null 2>&1; then
    printf 'ok'
  else
    printf 'handshake_missing_and_ping_failed'
  fi
}}

mkdir -p "$(dirname "$REQUEST_PATH")"
fix_app_data_ownership "$(dirname "$REQUEST_PATH")"
if [ ! -f "$SOURCE_CONF_PATH" ]; then
  write_status "error" "Missing source WireGuard config at $SOURCE_CONF_PATH"
  exit 1
fi

if ! source_config_matches_expected "$SOURCE_CONF_PATH"; then
  write_status "error" "Source WireGuard config does not match expected peer/allowedIPs/endpoint; refusing to patch helper config"
  exit 1
fi

install -m 600 "$SOURCE_CONF_PATH" "$LOCAL_CONF_PATH"
install -m 600 "$SOURCE_CONF_PATH" "$HOMEBREW_CONF_PATH"

CONF_PATH="$LOCAL_CONF_PATH"
if [ ! -f "$CONF_PATH" ] && [ -f "$HOMEBREW_CONF_PATH" ]; then
  CONF_PATH="$HOMEBREW_CONF_PATH"
fi

if [ ! -f "$CONF_PATH" ]; then
  write_status "error" "Missing saved WireGuard config at $LOCAL_CONF_PATH and $HOMEBREW_CONF_PATH"
  exit 1
fi

CURRENT_SHOW=$(wg show 2>/dev/null || true)
CURRENT_STATUS=$(tunnel_freshness_status "$CURRENT_SHOW")
if [ "$CURRENT_STATUS" = "ok" ]; then
  rm -f "$REQUEST_PATH" 2>/dev/null || true
  write_status "healthy" "WireGuard tunnel already healthy"
  exit 0
fi

wg-quick down "$LOCAL_CONF_PATH" >/dev/null 2>&1 || true
wg-quick down "$HOMEBREW_CONF_PATH" >/dev/null 2>&1 || true
sleep 1
if ! UP_OUTPUT=$(wg-quick up "$CONF_PATH" 2>&1); then
  SUMMARY=$(compact_output "$UP_OUTPUT")
  rm -f "$REQUEST_PATH" 2>/dev/null || true
  write_status "error" "wg-quick up failed: $SUMMARY"
  printf '%s\n' "$UP_OUTPUT"
  exit 1
fi
SHOW_OUTPUT=$(wg show 2>/dev/null || true)
rm -f "$REQUEST_PATH" 2>/dev/null || true

ATTEMPTS=10
while [ "$ATTEMPTS" -gt 0 ]; do
  CURRENT_STATUS=$(tunnel_freshness_status "$SHOW_OUTPUT")
  if [ "$CURRENT_STATUS" = "ok" ]; then
    write_status "repaired" "WireGuard tunnel applied successfully"
    printf '%s\n' "$SHOW_OUTPUT"
    exit 0
  fi

  ATTEMPTS=$((ATTEMPTS - 1))
  [ "$ATTEMPTS" -gt 0 ] || break
  sleep 1
  SHOW_OUTPUT=$(wg show 2>/dev/null || true)
done

SUMMARY=$(compact_output "$SHOW_OUTPUT")
write_status "error" "WireGuard helper freshness check failed (${{CURRENT_STATUS:-unknown}}) after reconnect; wg show: $SUMMARY"
printf '%s\n' "$SHOW_OUTPUT"
exit 1

"#
    );
    let launchd_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>{MACOS_WIREGUARD_HELPER_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{MACOS_WIREGUARD_HELPER_SCRIPT_PATH}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>NetworkState</key>
    <true/>
  </dict>
  <key>WatchPaths</key>
  <array>
    <string>{path}</string>
    <string>{request_path}</string>
  </array>
  <key>StartInterval</key>
  <integer>60</integer>
  <key>StandardOutPath</key>
  <string>/var/log/noland-wireguard.log</string>
  <key>StandardErrorPath</key>
  <string>/var/log/noland-wireguard.log</string>
</dict>
</plist>
"#
    );
    let shell_script = format!(
        "set -euo pipefail; cd /; export PATH=\"/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH\"; if ! command -v wg-quick >/dev/null 2>&1; then echo 'wg-quick not found. Install wireguard-tools first.' >&2; exit 1; fi; if [ ! -x \"{gotatun_bin}\" ] && ! command -v \"{gotatun_bin}\" >/dev/null 2>&1; then echo 'gotatun not found. Install gotatun first or set NOLAND_GOTATUN_BIN.' >&2; exit 1; fi; mkdir -p /usr/local/etc/wireguard /opt/homebrew/etc/wireguard /usr/local/libexec; install -m 600 \"{path}\" {MACOS_LOCAL_CONF_PATH}; install -m 600 \"{path}\" {MACOS_HOMEBREW_CONF_PATH}; rm -f \"{request_path}\" \"{status_path}\"; cat > {MACOS_WIREGUARD_HELPER_SCRIPT_PATH} <<'EOF'\n{repair_script}\nEOF\nchmod 755 {MACOS_WIREGUARD_HELPER_SCRIPT_PATH}; cat > {MACOS_WIREGUARD_HELPER_PLIST_PATH} <<'EOF'\n{launchd_plist}\nEOF\nchown root:wheel {MACOS_WIREGUARD_HELPER_PLIST_PATH}; chmod 644 {MACOS_WIREGUARD_HELPER_PLIST_PATH}; launchctl bootout system/{MACOS_WIREGUARD_HELPER_LABEL} >/dev/null 2>&1 || true; {MACOS_WIREGUARD_HELPER_SCRIPT_PATH}; launchctl bootstrap system {MACOS_WIREGUARD_HELPER_PLIST_PATH}; wg show"
    );
    let applescript = format!(
        "do shell script \"{}\" with administrator privileges",
        shell_script.replace('\\', "\\\\").replace('"', "\\\"")
    );

    let output = Command::new("osascript")
        .current_dir("/")
        .arg("-e")
        .arg(applescript)
        .output()
        .map_err(|error| AppError::Command(format!("Failed to run osascript: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "Failed to reconnect local WireGuard client (exit {}): stdout: {} | stderr: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    if is_setup {
        Ok("Managed GotaTun tunnel configured and activated on this Mac".to_string())
    } else {
        Ok("Managed GotaTun tunnel reconnected on this Mac".to_string())
    }
}

#[cfg(target_os = "macos")]
fn macos_helper_request_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("repair.request")
}

#[cfg(target_os = "macos")]
fn macos_helper_status_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("repair.status")
}

#[cfg(target_os = "macos")]
fn macos_helper_is_installed() -> bool {
    Path::new(MACOS_WIREGUARD_HELPER_PLIST_PATH).exists()
        && Path::new(MACOS_WIREGUARD_HELPER_SCRIPT_PATH).exists()
}

#[cfg(target_os = "macos")]
fn macos_helper_generation() -> Option<MacosHelperGeneration> {
    if !macos_helper_is_installed() {
        return None;
    }

    let Ok(plist) = std::fs::read_to_string(MACOS_WIREGUARD_HELPER_PLIST_PATH) else {
        return Some(MacosHelperGeneration::Legacy);
    };

    if plist.contains("WatchPaths")
        && plist.contains("repair.request")
        && plist.contains("KeepAlive")
        && plist.contains("NetworkState")
    {
        Some(MacosHelperGeneration::WatchRequests)
    } else {
        Some(MacosHelperGeneration::Legacy)
    }
}

#[cfg(target_os = "macos")]
fn macos_helper_monitor_repair_state() -> &'static Mutex<Option<Instant>> {
    MACOS_HELPER_LAST_MONITOR_REPAIR.get_or_init(|| Mutex::new(None))
}

#[cfg(target_os = "macos")]
fn can_attempt_macos_monitor_repair(force_bypass_cooldown: bool) -> bool {
    if force_bypass_cooldown {
        let mut state = macos_helper_monitor_repair_state()
            .lock()
            .expect("macos helper monitor repair mutex poisoned");
        *state = Some(Instant::now());
        return true;
    }

    let mut state = macos_helper_monitor_repair_state()
        .lock()
        .expect("macos helper monitor repair mutex poisoned");
    let now = Instant::now();

    if let Some(last_attempt) = *state {
        if now.duration_since(last_attempt)
            < Duration::from_secs(MACOS_HELPER_MONITOR_REPAIR_COOLDOWN_SECS)
        {
            return false;
        }
    }

    *state = Some(now);
    true
}

#[cfg(target_os = "macos")]
fn clear_macos_monitor_repair_cooldown() {
    if let Ok(mut state) = macos_helper_monitor_repair_state().lock() {
        *state = None;
    }
}

#[cfg(target_os = "macos")]
fn request_macos_helper_repair(config_path: &Path, reason: &str) -> AppResult<()> {
    if !matches!(
        macos_helper_generation(),
        Some(MacosHelperGeneration::WatchRequests)
    ) {
        return Err(AppError::Command(
            format!(
                "Legacy Noland WireGuard helper detected; run the explicit {} flow to upgrade the helper before using request-based repairs.",
                if reason.contains("setup") { "setup" } else { "reconnect" }
            )
        ));
    }

    let request_path = macos_helper_request_path(config_path);
    let status_path = macos_helper_status_path(config_path);
    if let Some(parent) = request_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AppError::Command(format!(
                "Failed creating Noland WireGuard helper request directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let _ = std::fs::remove_file(&status_path);

    std::fs::write(
        &request_path,
        format!("reason={reason}\ntimestamp={}\n", current_unix_timestamp()?),
    )
    .map_err(|error| {
        AppError::Command(format!(
            "Failed writing Noland WireGuard helper request {}: {error}",
            request_path.display()
        ))
    })?;

    let _ = std::fs::write(
        config_path,
        std::fs::read(config_path).map_err(|error| {
            AppError::Command(format!(
                "Failed reading WireGuard config {} before helper repair request: {error}",
                config_path.display()
            ))
        })?,
    );

    wait_for_macos_helper_result(config_path)
}

#[cfg(target_os = "macos")]
fn wait_for_macos_helper_result(config_path: &Path) -> AppResult<()> {
    let expected = load_expected_local_tunnel(config_path)?;
    let start = std::time::Instant::now();
    let timeout = if matches!(
        macos_helper_generation(),
        Some(MacosHelperGeneration::WatchRequests)
    ) {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(75)
    };

    while start.elapsed() < timeout {
        if local_tunnel_runtime_is_healthy(&expected) {
            return Ok(());
        }

        if let Some(status) = read_macos_helper_status(config_path) {
            if matches!(
                status.kind,
                MacosHelperStatusKind::Healthy | MacosHelperStatusKind::Repaired
            ) {
                return Ok(());
            }
            if matches!(status.kind, MacosHelperStatusKind::Error) {
                return Err(AppError::Command(if status.message.trim().is_empty() {
                    "WireGuard helper did not report a specific failure.".to_string()
                } else {
                    status.message
                }));
            }
        }

        std::thread::sleep(Duration::from_millis(500));
    }

    Err(AppError::Command(
        format!(
            "Timed out waiting for the installed Noland WireGuard helper to repair the local tunnel after {} seconds.",
            timeout.as_secs()
        ),
    ))
}

#[cfg(target_os = "macos")]
fn current_unix_timestamp() -> AppResult<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| {
            AppError::State(format!(
                "Clock failure while writing helper request: {error}"
            ))
        })
}

#[cfg(target_os = "macos")]
fn enforce_single_control_plane_macos(config_path: &Path) -> AppResult<()> {
    if Path::new("/Applications/WireGuard.app").exists() {
        warn!(
            "WireGuard.app is installed. Avoid reusing GUI tunnel names with CLI-managed tunnels to prevent tunnel ownership conflicts."
        );
    }

    let expected_peer = parse_wireguard_config_value(
        &std::fs::read_to_string(config_path).map_err(|error| {
            AppError::Command(format!(
                "Failed reading WireGuard client config {}: {error}",
                config_path.display()
            ))
        })?,
        "Peer",
        "PublicKey",
    )
    .ok_or_else(|| {
        AppError::InvalidInput(format!(
            "WireGuard client config {} is missing [Peer] PublicKey",
            config_path.display()
        ))
    })?;

    let output = resolved_command_output("wg", &["show"])?;

    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut active_peers: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("peer:") {
            active_peers.push(value.trim().to_string());
        }
    }

    if active_peers.is_empty() {
        return Ok(());
    }

    if active_peers.iter().any(|peer| peer == &expected_peer) {
        return Ok(());
    }

    warn!(
        "Another WireGuard tunnel appears active and may conflict with Noland-managed tunnel. Active peer(s): {}",
        active_peers.join(", ")
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn setup_local_wireguard_client_linux(config_path: &Path) -> AppResult<String> {
    const LOCAL_TUNNEL_NAME: &str = "nolandwg0";
    let expected = load_expected_local_tunnel(config_path)?;
    let gotatun_bin = resolve_gotatun_binary()?;

    let destination = "/etc/wireguard/nolandwg0.conf";
    let interface_exists = Command::new("sudo")
        .args(["wg", "show", LOCAL_TUNNEL_NAME])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if interface_exists && local_tunnel_runtime_is_healthy(&expected) {
        return Ok(
            "WireGuard client tunnel already active on this Linux machine (no reapply performed)"
                .to_string(),
        );
    }

    let copy = Command::new("sudo")
        .args([
            "install",
            "-m",
            "600",
            config_path.to_string_lossy().as_ref(),
            destination,
        ])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to copy WireGuard config: {error}")))?;

    if !copy.status.success() {
        return Err(AppError::Command(format!(
            "Failed to copy WireGuard config to /etc/wireguard with sudo (exit {}). Approve the sudo prompt and retry. stderr: {}",
            copy.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&copy.stderr).trim()
        )));
    }

    let gotatun_env = format!("WG_QUICK_USERSPACE_IMPLEMENTATION={}", gotatun_bin);

    let up = Command::new("sudo")
        .args([
            "env",
            gotatun_env.as_str(),
            "WG_SUDO=1",
            "wg-quick",
            "up",
            destination,
        ])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to start local WireGuard: {error}")))?;

    if !up.status.success() {
        return Err(AppError::Command(format!(
            "Failed to start local WireGuard with sudo (exit {}). Approve the sudo prompt and retry. stderr: {}",
            up.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&up.stderr).trim()
        )));
    }

    wait_for_local_tunnel_health(&expected, Duration::from_secs(20), Duration::from_secs(1))?;

    Ok("Managed GotaTun tunnel configured and activated on this Linux machine".to_string())
}

#[cfg(target_os = "linux")]
fn reconnect_local_wireguard_client_linux(config_path: &Path) -> AppResult<String> {
    let expected = load_expected_local_tunnel(config_path)?;
    let gotatun_bin = resolve_gotatun_binary()?;
    let destination = "/etc/wireguard/nolandwg0.conf";

    let copy = Command::new("sudo")
        .args([
            "install",
            "-m",
            "600",
            config_path.to_string_lossy().as_ref(),
            destination,
        ])
        .output()
        .map_err(|error| AppError::Command(format!("Failed to copy WireGuard config: {error}")))?;

    if !copy.status.success() {
        return Err(AppError::Command(format!(
            "Failed to copy WireGuard config to /etc/wireguard with sudo (exit {}). Approve the sudo prompt and retry. stderr: {}",
            copy.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&copy.stderr).trim()
        )));
    }

    let gotatun_env = format!("WG_QUICK_USERSPACE_IMPLEMENTATION={}", gotatun_bin);

    let _ = Command::new("sudo")
        .args([
            "env",
            gotatun_env.as_str(),
            "WG_SUDO=1",
            "wg-quick",
            "down",
            destination,
        ])
        .status();

    let up = Command::new("sudo")
        .args([
            "env",
            gotatun_env.as_str(),
            "WG_SUDO=1",
            "wg-quick",
            "up",
            destination,
        ])
        .output()
        .map_err(|error| {
            AppError::Command(format!("Failed to reconnect local WireGuard: {error}"))
        })?;

    if !up.status.success() {
        return Err(AppError::Command(format!(
            "Failed to reconnect local WireGuard with sudo (exit {}). Approve the sudo prompt and retry. stderr: {}",
            up.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&up.stderr).trim()
        )));
    }

    wait_for_local_tunnel_health(&expected, Duration::from_secs(20), Duration::from_secs(1))?;

    Ok("Managed GotaTun tunnel reconnected on this Linux machine".to_string())
}

#[cfg(target_os = "windows")]
fn setup_local_wireguard_client_windows(config_path: &Path) -> AppResult<String> {
    const LOCAL_TUNNEL_NAME: &str = "nolandwg0";
    let expected = load_expected_local_tunnel(config_path)?;
    let config = config_path.display().to_string();
    let already_active = match resolved_command_status("wg", &["show", LOCAL_TUNNEL_NAME]) {
        Ok(status) => status.success(),
        Err(_) => false,
    };

    if already_active && local_tunnel_runtime_is_healthy(&expected) {
        return Ok(
            "WireGuard client tunnel already active on this Windows machine (no reapply performed)"
                .to_string(),
        );
    }

    let output = resolved_command_output("wireguard.exe", &["/installtunnelservice", &config])?;

    if !output.status.success() {
        return Err(AppError::Command(format_wireguard_windows_failure(
            "setup", &output,
        )));
    }

    wait_for_local_tunnel_health(&expected, Duration::from_secs(25), Duration::from_secs(1))?;

    Ok("WireGuard client tunnel installed as Windows service".to_string())
}

#[cfg(target_os = "windows")]
fn reconnect_local_wireguard_client_windows(config_path: &Path) -> AppResult<String> {
    let expected = load_expected_local_tunnel(config_path)?;
    let config = config_path.display().to_string();

    let _ = resolved_command_status("wireguard.exe", &["/uninstalltunnelservice", "nolandwg0"]);

    let output = resolved_command_output("wireguard.exe", &["/installtunnelservice", &config])?;

    if !output.status.success() {
        return Err(AppError::Command(format_wireguard_windows_failure(
            "reconnect",
            &output,
        )));
    }

    wait_for_local_tunnel_health(&expected, Duration::from_secs(25), Duration::from_secs(1))?;

    Ok("WireGuard client tunnel reconnected on this Windows machine".to_string())
}

fn parse_default_route_dev(line: &str) -> Option<String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    let dev_index = parts.iter().position(|part| *part == "dev")?;
    let iface = parts.get(dev_index + 1)?;
    Some((*iface).to_string())
}

fn generate_keypair() -> AppResult<(String, String)> {
    let private_output = resolved_command_output("wg", &["genkey"])?;
    if !private_output.status.success() {
        return Err(AppError::Command(format!(
            "wg genkey failed: {}",
            String::from_utf8_lossy(&private_output.stderr)
        )));
    }
    let private = String::from_utf8_lossy(&private_output.stdout).to_string();

    let mut child = resolved_command("wg")?
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn wg pubkey: {error}")))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(private.as_bytes()).map_err(|error| {
            AppError::Command(format!("Failed writing to wg pubkey stdin: {error}"))
        })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| AppError::Command(format!("Failed reading wg pubkey output: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "wg pubkey failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let public = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if public.is_empty() {
        return Err(AppError::Command(
            "wg pubkey returned an empty key".to_string(),
        ));
    }

    Ok((private.trim().to_string(), public))
}

fn derive_public_key(private_key: &str) -> AppResult<String> {
    let mut child = resolved_command("wg")?
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn wg pubkey: {error}")))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(private_key.as_bytes()).map_err(|error| {
            AppError::Command(format!("Failed writing to wg pubkey stdin: {error}"))
        })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| AppError::Command(format!("Failed reading wg pubkey output: {error}")))?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "wg pubkey failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolved_command(tool: &str) -> AppResult<Command> {
    let os = OsDetection::new();
    let program = os.resolve_command_path(tool).ok_or_else(|| {
        AppError::Command(format!(
            "`{tool}` is not available in PATH. {}",
            os.install_hint_for_tool(tool)
        ))
    })?;
    let mut command = Command::new(program);
    os.with_augmented_path(&mut command);
    Ok(command)
}

fn resolved_command_output(tool: &str, args: &[&str]) -> AppResult<std::process::Output> {
    resolved_command(tool)?
        .args(args)
        .output()
        .map_err(|error| AppError::Command(format!("Failed to run {tool}: {error}")))
}

#[cfg(target_os = "windows")]
fn resolved_command_status(tool: &str, args: &[&str]) -> AppResult<std::process::ExitStatus> {
    resolved_command(tool)?
        .args(args)
        .status()
        .map_err(|error| AppError::Command(format!("Failed to run {tool}: {error}")))
}

async fn load_existing_local_identity(
    config_path: &Path,
) -> AppResult<Option<ExistingLocalIdentity>> {
    let content = match fs::read_to_string(config_path).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::Command(format!(
                "Failed reading local WireGuard config {}: {error}",
                config_path.display()
            )))
        }
    };

    let client_private_key = match parse_wireguard_config_value(&content, "Interface", "PrivateKey")
    {
        Some(value) => value,
        None => return Ok(None),
    };
    let server_public_key = match parse_wireguard_config_value(&content, "Peer", "PublicKey") {
        Some(value) => value,
        None => return Ok(None),
    };

    Ok(Some(ExistingLocalIdentity {
        client_private_key,
        server_public_key,
    }))
}

fn load_expected_local_tunnel(config_path: &Path) -> AppResult<ExpectedLocalTunnel> {
    let content = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading WireGuard client config {}: {error}",
            config_path.display()
        ))
    })?;

    let interface_private_key = parse_wireguard_config_value(&content, "Interface", "PrivateKey")
        .ok_or_else(|| {
        AppError::InvalidInput(format!(
            "WireGuard client config {} is missing [Interface] PrivateKey",
            config_path.display()
        ))
    })?;
    let interface_public_key = derive_public_key(&interface_private_key)?;
    let peer_public_key =
        parse_wireguard_config_value(&content, "Peer", "PublicKey").ok_or_else(|| {
            AppError::InvalidInput(format!(
                "WireGuard client config {} is missing [Peer] PublicKey",
                config_path.display()
            ))
        })?;
    let allowed_ips =
        parse_wireguard_config_value(&content, "Peer", "AllowedIPs").ok_or_else(|| {
            AppError::InvalidInput(format!(
                "WireGuard client config {} is missing [Peer] AllowedIPs",
                config_path.display()
            ))
        })?;
    let endpoint = parse_wireguard_config_value(&content, "Peer", "Endpoint").unwrap_or_default();
    let (endpoint_host, endpoint_port) = parse_wireguard_endpoint(&endpoint);
    let client_ip = parse_wireguard_config_value(&content, "Interface", "Address")
        .map(|value| strip_cidr(value.split(',').next().unwrap_or(&value).trim()))
        .unwrap_or_default();
    let server_ip = strip_cidr(allowed_ips.split(',').next().unwrap_or(&allowed_ips).trim());

    Ok(ExpectedLocalTunnel {
        interface_private_key,
        interface_public_key,
        peer_public_key,
        allowed_ips,
        endpoint_host,
        endpoint_port,
        server_ip,
        client_ip,
    })
}

fn collect_local_wireguard_runtime_state(
    expected_peer_public_key: Option<&str>,
) -> AppResult<LocalWireGuardRuntimeState> {
    let output = read_local_wireguard_show_output()?;
    if output.trim().is_empty() {
        return Ok(LocalWireGuardRuntimeState::default());
    }

    let mut peers = Vec::new();
    let mut current_interface_name = String::new();
    let mut current_interface_public_key = String::new();
    let mut current_peer: Option<LocalWireGuardPeerState> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("interface:") {
            if let Some(peer) = current_peer.take() {
                peers.push(peer);
            }
            current_interface_name = value.trim().to_string();
            current_interface_public_key.clear();
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("public key:") {
            if let Some(peer) = current_peer.as_mut() {
                if peer.peer_public_key.is_empty() {
                    peer.peer_public_key = value.trim().to_string();
                }
            } else if current_interface_public_key.is_empty() {
                current_interface_public_key = value.trim().to_string();
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("peer:") {
            if let Some(peer) = current_peer.take() {
                peers.push(peer);
            }
            current_peer = Some(LocalWireGuardPeerState {
                interface_name: current_interface_name.clone(),
                peer_public_key: value.trim().to_string(),
                allowed_ips: String::new(),
                latest_handshake: String::new(),
            });
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("allowed ips:") {
            if let Some(peer) = current_peer.as_mut() {
                if peer.allowed_ips.is_empty() {
                    peer.allowed_ips = value.trim().to_string();
                }
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("latest handshake:") {
            if let Some(peer) = current_peer.as_mut() {
                if peer.latest_handshake.is_empty() {
                    peer.latest_handshake = value.trim().to_string();
                }
            }
        }
    }

    if let Some(peer) = current_peer.take() {
        peers.push(peer);
    }

    let selected = expected_peer_public_key
        .and_then(|expected_peer| {
            peers
                .iter()
                .find(|peer| peer.peer_public_key == expected_peer)
        })
        .or_else(|| peers.first());

    Ok(selected
        .map(|peer| LocalWireGuardRuntimeState {
            interface_name: peer.interface_name.clone(),
            peer_public_key: peer.peer_public_key.clone(),
            allowed_ips: peer.allowed_ips.clone(),
            latest_handshake: peer.latest_handshake.clone(),
        })
        .unwrap_or_default())
}

fn local_tunnel_runtime_matches_expected(
    runtime: &LocalWireGuardRuntimeState,
    expected: &ExpectedLocalTunnel,
) -> bool {
    !runtime.interface_name.trim().is_empty()
        && runtime.peer_public_key == expected.peer_public_key
        && normalize_allowed_ips(&runtime.allowed_ips)
            == normalize_allowed_ips(&expected.allowed_ips)
}

fn local_tunnel_runtime_is_healthy(expected: &ExpectedLocalTunnel) -> bool {
    let Ok(runtime) = collect_local_wireguard_runtime_state(Some(&expected.peer_public_key)) else {
        return false;
    };

    if !local_tunnel_runtime_matches_expected(&runtime, expected) {
        return false;
    }

    if has_recent_handshake(&runtime.latest_handshake) {
        return true;
    }

    can_ping_tunnel_host(&expected.server_ip)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn wait_for_local_tunnel_health(
    expected: &ExpectedLocalTunnel,
    timeout: Duration,
    poll_interval: Duration,
) -> AppResult<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if local_tunnel_runtime_is_healthy(expected) {
            return Ok(());
        }
        std::thread::sleep(poll_interval);
    }

    let runtime = collect_local_wireguard_runtime_state(Some(&expected.peer_public_key))?;
    Err(AppError::Command(format!(
        "WireGuard tunnel did not become healthy after apply. expected_peer_match={} expected_allowed_ips_match={} latest_handshake='{}'",
        runtime.peer_public_key == expected.peer_public_key,
        normalize_allowed_ips(&runtime.allowed_ips) == normalize_allowed_ips(&expected.allowed_ips),
        runtime.latest_handshake
    )))
}

fn has_recent_handshake(latest_handshake: &str) -> bool {
    !latest_handshake.trim().is_empty() && !latest_handshake.to_ascii_lowercase().contains("never")
}

fn can_ping_tunnel_host(server_ip: &str) -> bool {
    if server_ip.trim().is_empty() {
        return false;
    }

    let args = OsDetection::new().ping_args(server_ip);
    Command::new("ping")
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn normalize_allowed_ips(value: &str) -> String {
    let mut parts = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts.sort_unstable();
    parts.join(",")
}

fn parse_wireguard_config_value(content: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    let target_section = format!("[{}]", section);
    let key_lower = key.to_ascii_lowercase();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed.eq_ignore_ascii_case(&target_section);
            continue;
        }

        if !in_section {
            continue;
        }

        let Some((raw_key, raw_value)) = trimmed.split_once('=') else {
            continue;
        };

        if raw_key.trim().to_ascii_lowercase() == key_lower {
            return Some(raw_value.trim().to_string());
        }
    }

    None
}

fn parse_wireguard_endpoint(endpoint: &str) -> (String, u16) {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return (String::new(), 0);
    }

    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((host, port)) = rest.split_once("]:") {
            return (host.to_string(), port.parse::<u16>().unwrap_or(0));
        }
    }

    if let Some((host, port)) = trimmed.rsplit_once(':') {
        return (host.to_string(), port.parse::<u16>().unwrap_or(0));
    }

    (trimmed.to_string(), 0)
}

fn apply_expected_tunnel_to_state(
    state: &mut crate::models::app_state::PersistedAppState,
    expected: &ExpectedLocalTunnel,
) -> bool {
    let mut changed = false;

    if state.wireguard.server_ip != expected.server_ip {
        state.wireguard.server_ip = expected.server_ip.clone();
        changed = true;
    }
    if state.wireguard.client_ip != expected.client_ip {
        state.wireguard.client_ip = expected.client_ip.clone();
        changed = true;
    }
    if state.wireguard.server_public_key != expected.peer_public_key {
        state.wireguard.server_public_key = expected.peer_public_key.clone();
        changed = true;
    }
    if state.wireguard.client_public_key != expected.interface_public_key {
        state.wireguard.client_public_key = expected.interface_public_key.clone();
        changed = true;
    }

    let client_private_fingerprint = wireguard_key_fingerprint(&expected.interface_private_key);
    if state.wireguard.client_private_key_fingerprint != client_private_fingerprint {
        state.wireguard.client_private_key_fingerprint = client_private_fingerprint;
        changed = true;
    }

    let client_public_fingerprint = wireguard_key_fingerprint(&expected.interface_public_key);
    if state.wireguard.client_public_key_fingerprint != client_public_fingerprint {
        state.wireguard.client_public_key_fingerprint = client_public_fingerprint;
        changed = true;
    }

    let server_public_fingerprint = wireguard_key_fingerprint(&expected.peer_public_key);
    if state.wireguard.server_public_key_fingerprint != server_public_fingerprint {
        state.wireguard.server_public_key_fingerprint = server_public_fingerprint;
        changed = true;
    }

    if state.wireguard.endpoint_host != expected.endpoint_host {
        state.wireguard.endpoint_host = expected.endpoint_host.clone();
        changed = true;
    }
    if state.wireguard.endpoint_port != expected.endpoint_port {
        state.wireguard.endpoint_port = expected.endpoint_port;
        changed = true;
    }

    if state.moonlight.host_address != expected.server_ip {
        state.moonlight.host_address = expected.server_ip.clone();
        changed = true;
    }

    changed
}

fn wireguard_key_fingerprint(key: &str) -> String {
    let raw = BASE64_STANDARD.decode(key.trim()).unwrap_or_default();
    if raw.is_empty() {
        return String::new();
    }

    raw.iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn strip_cidr(ip: &str) -> String {
    ip.split('/').next().unwrap_or(ip).to_string()
}

fn shell_single_quote_escape(content: &str) -> String {
    content.replace('\'', "'\"'\"'")
}

#[cfg(target_os = "windows")]
fn format_wireguard_windows_failure(action: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let combined = if stderr.is_empty() {
        stdout.clone()
    } else if stdout.is_empty() {
        stderr.clone()
    } else {
        format!("{stderr} | {stdout}")
    };

    let lower = combined.to_ascii_lowercase();
    if lower.contains("access is denied")
        || lower.contains("elevation")
        || lower.contains("administrator")
    {
        return format!(
            "WireGuard {action} failed due to missing administrator privileges. Run Noland Connect as Administrator (or approve UAC), then retry. Details: {}",
            combined
        );
    }

    format!(
        "Failed to {action} local WireGuard client (exit {}): {}",
        output.status.code().unwrap_or(-1),
        combined
    )
}
