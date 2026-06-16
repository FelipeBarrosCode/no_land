use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

use super::super::core::session::TunnelSession;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelBridgeRequest<'a> {
    command: &'a str,
    session: &'a TunnelSession,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MacosTunnelStatus {
    pub manager_installed: bool,
    pub manager_enabled: bool,
    pub provider_running: bool,
    pub route_ready: bool,
    pub tunnel_ip: String,
    pub sunshine_reachable: bool,
    pub state: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacosNativeDriver;

impl MacosNativeDriver {
    pub fn start(&self, session: &TunnelSession) -> AppResult<MacosTunnelStatus> {
        let status = run_bridge_command("start", session)?;
        ensure_manager_ready("start macOS native tunnel", &status)?;

        for _ in 0..10 {
            let current = self.status(session)?;
            if current.provider_running {
                return Ok(current);
            }
            thread::sleep(Duration::from_millis(500));
        }

        let current = self.status(session)?;
        ensure_manager_ready("observe running macOS tunnel state after start", &current)?;
        if !current.route_ready {
            return Err(AppError::Command(
                "macOS tunnel manager started but the packet tunnel provider did not report route readiness yet"
                    .to_string(),
            ));
        }
        Ok(current)
    }

    pub fn stop(&self, session: &TunnelSession) -> AppResult<MacosTunnelStatus> {
        run_bridge_command("stop", session)
    }

    pub fn status(&self, session: &TunnelSession) -> AppResult<MacosTunnelStatus> {
        run_bridge_command("status", session)
    }
}

fn run_bridge_command(command: &str, session: &TunnelSession) -> AppResult<MacosTunnelStatus> {
    let bridge_path = resolve_bridge_path()?;
    let request = serde_json::to_vec(&TunnelBridgeRequest { command, session }).map_err(|error| {
        AppError::Command(format!(
            "Failed serializing macOS WireGuard bridge request for `{command}`: {error}"
        ))
    })?;

    let mut child = Command::new(&bridge_path)
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            AppError::Command(format!(
                "Failed launching macOS WireGuard bridge {}: {error}",
                bridge_path.display()
            ))
        })?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(&request).map_err(|error| {
            AppError::Command(format!(
                "Failed writing session payload to macOS WireGuard bridge stdin: {error}"
            ))
        })?;
    }

    let output = child.wait_with_output().map_err(|error| {
        AppError::Command(format!(
            "Failed waiting for macOS WireGuard bridge `{command}` result: {error}"
        ))
    })?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "macOS WireGuard bridge `{command}` failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    serde_json::from_slice::<MacosTunnelStatus>(&output.stdout).map_err(|error| {
        AppError::Command(format!(
            "Failed parsing macOS WireGuard bridge `{command}` output: {error}. stdout: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    })
}

fn ensure_manager_ready(action: &str, status: &MacosTunnelStatus) -> AppResult<()> {
    if status.manager_installed && status.manager_enabled && status.provider_running {
        return Ok(());
    }

    let details = status
        .last_error
        .clone()
        .unwrap_or_else(|| "Bridge did not report a specific provider failure.".to_string());
    Err(AppError::Command(format!(
        "Failed to {action}: manager_installed={}, manager_enabled={}, provider_running={}, state={}, error={details}",
        status.manager_installed,
        status.manager_enabled,
        status.provider_running,
        status.state,
    )))
}

fn resolve_bridge_path() -> AppResult<PathBuf> {
    if let Ok(override_path) = env::var("NOLAND_MACOS_BRIDGE_PATH") {
        let candidate = PathBuf::from(override_path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    let current_exe = env::current_exe().map_err(|error| {
        AppError::Command(format!("Failed resolving current executable path: {error}"))
    })?;

    let candidates = bridge_path_candidates(&current_exe);
    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            AppError::NotFound(
                "macOS WireGuard bridge not found. Build NolandTunnelBridge and set NOLAND_MACOS_BRIDGE_PATH or bundle it with the app."
                    .to_string(),
            )
        })
}

fn bridge_path_candidates(current_exe: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(exe_dir) = current_exe.parent() {
        paths.push(exe_dir.join("NolandTunnelBridge"));
        paths.push(exe_dir.join("../Resources/NolandTunnelBridge"));
        paths.push(exe_dir.join("../MacOS/NolandTunnelBridge"));
    }

    if let Some(src_tauri_dir) = locate_src_tauri_root(current_exe) {
        paths.push(src_tauri_dir.join("platform/macos/.build/release/NolandTunnelBridge"));
        paths.push(src_tauri_dir.join("platform/macos/bin/NolandTunnelBridge"));
    }

    paths
}

fn locate_src_tauri_root(current_exe: &Path) -> Option<PathBuf> {
    current_exe
        .ancestors()
        .find(|ancestor| ancestor.join("tauri.conf.json").exists())
        .map(Path::to_path_buf)
}
