use std::{
    io::ErrorKind,
    net::{IpAddr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{info, warn};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};

use crate::{
    errors::{AppError, AppResult},
    utils::managed_binaries::{
        bundled_binary_candidate_paths, bundled_binary_names, locate_bundled_binary,
    },
};

use super::{app_config::WireGuardDefaults, app_context::AppContext, remote_exec::RemoteExec};

#[cfg(target_os = "linux")]
use super::os_detection::OsDetection;

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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GotatunRuntimeStatus {
    engine: String,
    active: bool,
    pid: u32,
    interface_name: String,
    config_path: String,
    peer_public_key: String,
    allowed_ips: Vec<String>,
    endpoint: String,
    latest_handshake_age_secs: Option<u64>,
    rx_bytes: u64,
    tx_bytes: u64,
    updated_at_unix: u64,
    error: Option<String>,
}

const REQUIRED_REMOTE_WIREGUARD_PACKAGES: &[&str] = &["wireguard-tools", "iproute2", "ufw"];
const APT_UPDATE_TIMEOUT_SECS: u64 = 180;
const APT_INSTALL_TIMEOUT_SECS: u64 = 300;
const PACKAGE_MANAGER_READY_WAIT_SECS: u64 = 180;
const LEGACY_LOCAL_CONFIG_NAME: &str = "nolandwg0.conf";
const MONITOR_REPAIR_FAILURE_STREAK_THRESHOLD: u32 = 5;
const GOTATUN_RUNTIME_DIR_NAME: &str = "gotatun-runtime";
const GOTATUN_STATUS_FILE_NAME: &str = "status.json";
const GOTATUN_STOP_REQUEST_FILE_NAME: &str = "stop.request";
const GOTATUN_HELPER_READY_TIMEOUT_SECS: u64 = 30;
const GOTATUN_HELPER_STOP_TIMEOUT_SECS: u64 = 15;
static MONITOR_REPAIR_FAILURE_STREAK: OnceLock<Mutex<u32>> = OnceLock::new();
static ACTIVE_GOTATUN_STATUS_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

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

fn monitor_repair_failure_streak() -> &'static Mutex<u32> {
    MONITOR_REPAIR_FAILURE_STREAK.get_or_init(|| Mutex::new(0))
}

fn bundled_tool_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => "",
    }
}

fn managed_tool_spec(tool: &str) -> Option<(&'static str, &'static str, bool)> {
    match tool {
        "noland-net-helper" | "noland-net-helper.exe" => Some((
            "noland-net-helper",
            "NOLAND_NET_HELPER_BIN",
            cfg!(target_os = "windows"),
        )),
        _ => None,
    }
}

fn managed_tool_binary_names(tool: &str) -> Vec<String> {
    let Some((stem, _env_var, uses_exe_suffix)) = managed_tool_spec(tool) else {
        return vec![tool.to_string()];
    };

    bundled_binary_names(stem, uses_exe_suffix, bundled_tool_target_triple())
}

fn bundled_tool_candidate_paths(tool: &str) -> Vec<PathBuf> {
    bundled_binary_candidate_paths(
        &managed_tool_binary_names(tool),
        std::env::current_exe().ok().as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
}

fn locate_managed_tool_binary(tool: &str) -> Option<PathBuf> {
    let (lookup_name, env_var, uses_exe_suffix) = managed_tool_spec(tool)?;
    locate_bundled_binary(
        lookup_name,
        env_var,
        uses_exe_suffix,
        bundled_tool_target_triple(),
    )
}

fn resolve_managed_tool_binary(tool: &str) -> AppResult<String> {
    let Some((lookup_name, env_var, _uses_exe_suffix)) = managed_tool_spec(tool) else {
        return Err(AppError::Command(format!(
            "No managed tool resolver exists for `{tool}`"
        )));
    };

    if let Some(path) = locate_managed_tool_binary(tool) {
        return Ok(path.display().to_string());
    }

    let searched = bundled_tool_candidate_paths(lookup_name)
        .into_iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let message = if cfg!(debug_assertions) {
        format!(
            "Required managed tool `{lookup_name}` was not found. Place it in `src-tauri/binaries` or set {env_var} to its full path. Searched: {searched}"
        )
    } else {
        format!(
            "This Noland Connect installation is incomplete: the bundled `{lookup_name}` component is missing or not executable. Reinstall the app or report the package; do not install WireGuard manually."
        )
    };
    Err(AppError::Command(message))
}

pub(crate) fn locate_noland_net_helper_binary() -> Option<PathBuf> {
    locate_managed_tool_binary("noland-net-helper")
}

fn resolve_noland_net_helper_binary() -> AppResult<String> {
    resolve_managed_tool_binary("noland-net-helper")
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

fn gotatun_runtime_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(GOTATUN_RUNTIME_DIR_NAME)
}

fn gotatun_status_path(config_path: &Path) -> PathBuf {
    gotatun_runtime_dir(config_path).join(GOTATUN_STATUS_FILE_NAME)
}

fn gotatun_stop_request_path(config_path: &Path) -> PathBuf {
    gotatun_runtime_dir(config_path).join(GOTATUN_STOP_REQUEST_FILE_NAME)
}

fn active_gotatun_status_path() -> &'static Mutex<Option<PathBuf>> {
    ACTIVE_GOTATUN_STATUS_PATH.get_or_init(|| Mutex::new(None))
}

fn remember_active_gotatun_config(config_path: &Path) {
    if let Ok(mut path) = active_gotatun_status_path().lock() {
        *path = Some(gotatun_status_path(config_path));
    }
}

fn load_gotatun_runtime_status(path: &Path) -> Option<GotatunRuntimeStatus> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn load_active_gotatun_runtime_status() -> Option<GotatunRuntimeStatus> {
    let path = active_gotatun_status_path().lock().ok()?.clone()?;
    load_gotatun_runtime_status(&path)
}

fn gotatun_runtime_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(target_os = "windows")]
pub(crate) fn locate_wintun_library() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("NOLAND_WINTUN_DLL") {
        let path = PathBuf::from(value.trim());
        if path.is_file() {
            return Some(path);
        }
    }

    let target = bundled_tool_target_triple();
    let names = vec!["wintun.dll".to_string(), format!("wintun-{target}.dll")];
    bundled_binary_candidate_paths(
        &names,
        std::env::current_exe().ok().as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
    .into_iter()
    .find(|path| path.is_file())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn locate_wintun_library() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "windows")]
fn resolve_wintun_library() -> AppResult<PathBuf> {
    locate_wintun_library().ok_or_else(|| {
        AppError::Command(
            "This Noland Connect installation is incomplete: the bundled Wintun adapter library is missing. Reinstall the app or report the package; do not install WireGuard manually."
                .to_string(),
        )
    })
}

fn launch_managed_gotatun_helper(config_path: &Path) -> AppResult<()> {
    let helper = resolve_noland_net_helper_binary()?;
    let runtime_dir = gotatun_runtime_dir(config_path);
    std::fs::create_dir_all(&runtime_dir).map_err(|error| {
        AppError::Command(format!(
            "Failed creating managed GotaTun runtime directory {}: {error}",
            runtime_dir.display()
        ))
    })?;
    let _ = std::fs::remove_file(gotatun_stop_request_path(config_path));

    #[cfg(target_os = "linux")]
    {
        let broker = if OsDetection::new().command_exists("pkexec") {
            "pkexec"
        } else if OsDetection::new().command_exists("sudo") {
            "sudo"
        } else {
            return Err(AppError::Command(
                "Noland needs the desktop privilege broker (`pkexec`) to create its managed tunnel, but no supported privilege broker is available."
                    .to_string(),
            ));
        };
        Command::new(broker)
            .arg(&helper)
            .args(["run", "--config"])
            .arg(config_path)
            .arg("--state-dir")
            .arg(&runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                AppError::Command(format!(
                    "Failed launching the bundled Noland GotaTun helper through {broker}: {error}"
                ))
            })?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        fn shell_quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\"'\"'"))
        }
        let command = [
            shell_quote(&helper),
            "run".to_string(),
            "--config".to_string(),
            shell_quote(&config_path.display().to_string()),
            "--state-dir".to_string(),
            shell_quote(&runtime_dir.display().to_string()),
        ]
        .join(" ");
        let applescript = format!(
            "do shell script \"{}\" with administrator privileges",
            command.replace('\\', "\\\\").replace('"', "\\\"")
        );
        Command::new("osascript")
            .args(["-e", &applescript])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                AppError::Command(format!(
                    "Failed launching the bundled Noland GotaTun helper with administrator privileges: {error}"
                ))
            })?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        fn powershell_quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "''"))
        }
        let wintun = resolve_wintun_library()?;
        let argument_list = [
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--state-dir".to_string(),
            runtime_dir.display().to_string(),
            "--wintun".to_string(),
            wintun.display().to_string(),
        ]
        .into_iter()
        .map(|value| powershell_quote(&value))
        .collect::<Vec<_>>()
        .join(",");
        let script = format!(
            "Start-Process -FilePath {} -Verb RunAs -ArgumentList @({argument_list})",
            powershell_quote(&helper)
        );
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                AppError::Command(format!(
                    "Failed launching the bundled Noland GotaTun helper with Windows elevation: {error}"
                ))
            })?;
        return Ok(());
    }
}

fn wait_for_managed_gotatun_start(config_path: &Path, launched_at: u64) -> AppResult<()> {
    let status_path = gotatun_status_path(config_path);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(GOTATUN_HELPER_READY_TIMEOUT_SECS) {
        if let Some(status) = load_gotatun_runtime_status(&status_path) {
            if status.updated_at_unix >= launched_at
                && status.config_path == config_path.display().to_string()
            {
                if status.active {
                    return Ok(());
                }
                if let Some(error) = status.error.filter(|value| !value.trim().is_empty()) {
                    return Err(AppError::Command(format!(
                        "The bundled Noland GotaTun helper could not start the tunnel: {error}"
                    )));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    Err(AppError::Timeout(format!(
        "The bundled Noland GotaTun helper did not become ready within {} seconds. Approve the operating-system elevation prompt and retry.",
        GOTATUN_HELPER_READY_TIMEOUT_SECS
    )))
}

fn request_managed_gotatun_stop(config_path: &Path) -> AppResult<()> {
    let runtime_dir = gotatun_runtime_dir(config_path);
    if !runtime_dir.exists() {
        return Ok(());
    }
    std::fs::write(gotatun_stop_request_path(config_path), b"stop\n").map_err(|error| {
        AppError::Command(format!(
            "Failed requesting managed GotaTun tunnel shutdown: {error}"
        ))
    })?;

    let started = Instant::now();
    let status_path = gotatun_status_path(config_path);
    while started.elapsed() < Duration::from_secs(GOTATUN_HELPER_STOP_TIMEOUT_SECS) {
        if load_gotatun_runtime_status(&status_path).is_none_or(|status| {
            !status.active
                || gotatun_runtime_unix_timestamp().saturating_sub(status.updated_at_unix) > 5
        }) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    Err(AppError::Timeout(format!(
        "Managed GotaTun tunnel did not stop within {} seconds",
        GOTATUN_HELPER_STOP_TIMEOUT_SECS
    )))
}

fn setup_managed_gotatun_tunnel(config_path: &Path) -> AppResult<String> {
    remember_active_gotatun_config(config_path);
    let expected = load_expected_local_tunnel(config_path)?;
    if let Some(status) = load_gotatun_runtime_status(&gotatun_status_path(config_path)) {
        if status.active
            && status.peer_public_key == expected.peer_public_key
            && normalize_allowed_ips(&status.allowed_ips.join(","))
                == normalize_allowed_ips(&expected.allowed_ips)
            && can_ping_tunnel_host(&expected.server_ip)
        {
            return Ok(
                "Embedded GotaTun tunnel is already active with the saved Noland identity"
                    .to_string(),
            );
        }
        if status.active {
            request_managed_gotatun_stop(config_path)?;
        }
    }

    let launched_at = gotatun_runtime_unix_timestamp();
    launch_managed_gotatun_helper(config_path)?;
    wait_for_managed_gotatun_start(config_path, launched_at)?;
    wait_for_local_tunnel_health(&expected, Duration::from_secs(25), Duration::from_secs(1))?;
    Ok("Embedded GotaTun tunnel configured and activated by Noland".to_string())
}

fn reconnect_managed_gotatun_tunnel(config_path: &Path) -> AppResult<String> {
    remember_active_gotatun_config(config_path);
    request_managed_gotatun_stop(config_path)?;
    let launched_at = gotatun_runtime_unix_timestamp();
    launch_managed_gotatun_helper(config_path)?;
    wait_for_managed_gotatun_start(config_path, launched_at)?;
    let expected = load_expected_local_tunnel(config_path)?;
    wait_for_local_tunnel_health(&expected, Duration::from_secs(25), Duration::from_secs(1))?;
    Ok("Embedded GotaTun tunnel reconnected by Noland".to_string())
}

fn teardown_managed_gotatun_tunnel(config_path: &Path) -> AppResult<String> {
    remember_active_gotatun_config(config_path);
    request_managed_gotatun_stop(config_path)?;
    Ok("Embedded GotaTun tunnel stopped by Noland".to_string())
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

    let timestamp = gotatun_runtime_unix_timestamp();
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
            "sudo bash -lc 'RATE_MBIT={qos_bandwidth_mbit}; if [ \"$RATE_MBIT\" -le 0 ] 2>/dev/null; then LINK_SPEED=$(cat /sys/class/net/{nic}/speed 2>/dev/null || true); if [ -n \"$LINK_SPEED\" ] && [ \"$LINK_SPEED\" -gt 0 ] 2>/dev/null; then RATE_MBIT=$(awk -v raw=\"$LINK_SPEED\" \"BEGIN {{ capped=int(raw * 0.90); if (capped < 100) capped=100; if (capped > 5000) capped=5000; printf \\\"%d\\\", capped }}\"); else RATE_MBIT=900; fi; fi; if [ \"{qos_mode}\" = \"cake\" ] && tc qdisc replace dev {nic} root cake bandwidth \"${{RATE_MBIT}}mbit\" {qos_diffserv_profile} nat 2>/dev/null; then echo qos=cake rate=${{RATE_MBIT}}mbit; else tc qdisc replace dev {nic} root fq_codel; echo qos=fq_codel; fi; if [ \"{dscp_enabled}\" = \"1\" ]; then if command -v iptables >/dev/null 2>&1; then iptables -t mangle -D OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4 2>/dev/null || true; iptables -t mangle -D OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true; iptables -t mangle -D OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21 2>/dev/null || true; iptables -t mangle -A OUTPUT -p udp --dport 47998:48010 -j DSCP --set-dscp-class CS4; iptables -t mangle -A OUTPUT -p tcp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21; iptables -t mangle -A OUTPUT -p udp -m multiport --dports 47989,47990,47984 -j DSCP --set-dscp-class AF21; fi; fi; (sudo ethtool -C {nic} rx-usecs 0 tx-usecs 0 || true); sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null; sudo sysctl -w net.ipv4.conf.all.rp_filter=0 >/dev/null; sudo sysctl -w net.ipv4.conf.default.rp_filter=0 >/dev/null; sudo sysctl -w net.ipv4.conf.{nic}.rp_filter=0 >/dev/null; sudo sysctl -w net.ipv4.conf.{iface}.rp_filter=0 >/dev/null'"
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

    setup_local_wireguard_client_inner(config_path)
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

    reconnect_local_wireguard_client_inner(config_path)
}

pub fn teardown_local_wireguard_client(config_path: &Path) -> AppResult<String> {
    teardown_managed_gotatun_tunnel(config_path)
}

fn setup_local_wireguard_client_inner(config_path: &Path) -> AppResult<String> {
    setup_managed_gotatun_tunnel(config_path)
}

fn reconnect_local_wireguard_client_inner(config_path: &Path) -> AppResult<String> {
    reconnect_managed_gotatun_tunnel(config_path)
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
    let Some(status) = load_active_gotatun_runtime_status() else {
        return Ok(String::new());
    };
    if !status.active {
        return Ok(String::new());
    }

    let latest_handshake = status
        .latest_handshake_age_secs
        .map(|seconds| format!("{seconds} seconds ago"))
        .unwrap_or_else(|| "never".to_string());
    Ok(format!(
        "interface: {}\n  public key: managed-by-{}\n  peer: {}\n    endpoint: {}\n    allowed ips: {}\n    latest handshake: {}\n    transfer: {} B received, {} B sent\n    runtime pid: {}\n",
        status.interface_name,
        status.engine,
        status.peer_public_key,
        status.endpoint,
        status.allowed_ips.join(", "),
        latest_handshake,
        status.rx_bytes,
        status.tx_bytes,
        status.pid,
    ))
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
        remember_active_gotatun_config(&persisted_config_path);
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
    remember_active_gotatun_config(&config_path);
    cleanup_stale_wireguard_artifacts(&config_path);

    let expected = load_expected_local_tunnel(&config_path)?;
    let status = load_gotatun_runtime_status(&gotatun_status_path(&config_path));
    let status_fresh = status.as_ref().is_some_and(|runtime| {
        gotatun_runtime_unix_timestamp().saturating_sub(runtime.updated_at_unix) <= 5
    });
    let runtime = collect_local_wireguard_runtime_state(Some(&expected.peer_public_key))?;
    let runtime_matches = local_tunnel_runtime_matches_expected(&runtime, &expected);
    let handshake_ok = status
        .as_ref()
        .and_then(|runtime| runtime.latest_handshake_age_secs)
        .is_some_and(|age| age <= 180);
    let ping_ok = can_ping_tunnel_host(&expected.server_ip);
    let process_active = status.as_ref().is_some_and(|runtime| runtime.active);
    let tunnel_healthy =
        status_fresh && process_active && runtime_matches && (handshake_ok || ping_ok);

    if tunnel_healthy {
        let _ = note_monitor_repair_health(true);
        let _ = context
            .update_state(|state| {
                state.wireguard.config_path = config_path.display().to_string();
                apply_expected_tunnel_to_state(state, &expected);
                state.wireguard.last_runtime_interface = runtime.interface_name.clone();
            })
            .await;
        return Ok(());
    }

    let hard_failure = !status_fresh || !process_active || !runtime_matches;
    let failure_streak = note_monitor_repair_health(false);
    if !hard_failure && failure_streak < MONITOR_REPAIR_FAILURE_STREAK_THRESHOLD {
        if failure_streak == 1 {
            warn!(
                "Embedded GotaTun health check is temporarily unhealthy; waiting before repair (handshake_recent={}, ping_ok={})",
                handshake_ok,
                ping_ok
            );
        }
        return Ok(());
    }

    warn!(
        "Embedded GotaTun health monitor is repairing the Noland-owned tunnel (status_fresh={}, process_active={}, identity_match={}, handshake_recent={}, ping_ok={})",
        status_fresh,
        process_active,
        runtime_matches,
        handshake_ok,
        ping_ok
    );
    let _mutation = context.begin_wireguard_mutation();
    reconnect_managed_gotatun_tunnel(&config_path)?;
    let repaired_runtime = collect_local_wireguard_runtime_state(Some(&expected.peer_public_key))?;
    let _ = note_monitor_repair_health(true);
    context
        .update_state(|state| {
            state.wireguard.config_path = config_path.display().to_string();
            apply_expected_tunnel_to_state(state, &expected);
            state.wireguard.last_runtime_interface = repaired_runtime.interface_name.clone();
        })
        .await?;

    Ok(())
}

fn ensure_local_wireguard_tools() -> AppResult<()> {
    let _ = resolve_noland_net_helper_binary()?;
    #[cfg(target_os = "windows")]
    let _ = resolve_wintun_library()?;
    Ok(())
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

        if cfg!(target_os = "macos") && in_interface_section && lower.starts_with("listenport") {
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

fn parse_default_route_dev(line: &str) -> Option<String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    let dev_index = parts.iter().position(|part| *part == "dev")?;
    let iface = parts.get(dev_index + 1)?;
    Some((*iface).to_string())
}

fn generate_keypair() -> AppResult<(String, String)> {
    let private = X25519StaticSecret::random_from_rng(OsRng);
    let public = X25519PublicKey::from(&private);
    Ok((
        BASE64_STANDARD.encode(private.to_bytes()),
        BASE64_STANDARD.encode(public.as_bytes()),
    ))
}

fn derive_public_key(private_key: &str) -> AppResult<String> {
    let decoded = BASE64_STANDARD
        .decode(private_key.trim())
        .map_err(|error| {
            AppError::InvalidInput(format!(
                "WireGuard private key is not valid base64: {error}"
            ))
        })?;
    let private_bytes: [u8; 32] = decoded.try_into().map_err(|_| {
        AppError::InvalidInput("WireGuard private key must be exactly 32 bytes".to_string())
    })?;
    let private = X25519StaticSecret::from(private_bytes);
    let public = X25519PublicKey::from(&private);
    Ok(BASE64_STANDARD.encode(public.as_bytes()))
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
    let Ok(ip) = server_ip.trim().parse::<IpAddr>() else {
        return false;
    };
    let address = SocketAddr::new(ip, 22);
    match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
        Ok(stream) => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            true
        }
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => true,
        Err(_) => false,
    }
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
