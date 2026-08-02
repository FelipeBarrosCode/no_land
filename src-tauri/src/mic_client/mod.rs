pub mod device_list;
mod permissions;
mod runtime;

use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use tracing::info;

pub use permissions::ensure_microphone_access;

use crate::errors::{AppError, AppResult};
use crate::models::app_state::MicQualityProfile;

/// Configuration for the microphone sender sidecar.
#[derive(Debug, Clone)]
pub struct MicClientConfig {
    pub device_id: Option<String>,
    pub quality_profile: MicQualityProfile,
    pub session_id: u64,
    pub session_secret: Vec<u8>,
    pub ssrc: u32,
    pub remote_addr: String,
}

/// Handle to a running microphone sender sidecar.
pub struct MicClientHandle {
    child: Option<Child>,
}

impl MicClientHandle {
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for MicClientHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn sidecar_log_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(cache_dir) = dirs::cache_dir() {
        candidates.push(cache_dir.join("noland-connect").join("logs"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("src-tauri").join("target").join("logs"));
    }
    candidates
}

fn resolve_sidecar_log_path() -> Option<PathBuf> {
    for directory in sidecar_log_paths() {
        if std::fs::create_dir_all(&directory).is_err() {
            continue;
        }
        return Some(directory.join("noland-mic-sender.log"));
    }
    None
}

fn prepare_sidecar_log_stdio() -> Option<(Stdio, Stdio)> {
    let log_path = resolve_sidecar_log_path()?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok()?;
    let stderr = file.try_clone().ok()?;
    Some((Stdio::from(file), Stdio::from(stderr)))
}

#[cfg(unix)]
fn stale_stream_pids(sidecar: &Path) -> Vec<String> {
    let sidecar_path = sidecar.to_string_lossy().to_string();
    let output = Command::new("pgrep")
        .arg("-f")
        .arg(format!("{} stream", sidecar_path))
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|pid| !pid.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(unix)]
fn cleanup_stale_stream_processes(sidecar: &Path) {
    let mut pids = stale_stream_pids(sidecar);
    for pid in &pids {
        let _ = Command::new("kill").arg("-TERM").arg(pid).status();
    }

    if !pids.is_empty() {
        thread::sleep(Duration::from_millis(350));
    }

    pids = stale_stream_pids(sidecar);
    for pid in &pids {
        let _ = Command::new("kill").arg("-KILL").arg(pid).status();
    }

    if !pids.is_empty() {
        thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(not(unix))]
fn cleanup_stale_stream_processes(_sidecar: &Path) {}

pub fn cleanup_stale_pipeline_processes() -> AppResult<()> {
    let sidecar = runtime::resolve_mic_sender_binary()?;
    cleanup_stale_stream_processes(&sidecar);
    Ok(())
}

/// Start the local microphone sender sidecar.
pub fn start_pipeline(config: MicClientConfig) -> AppResult<MicClientHandle> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = config;
        return Err(AppError::Command(
            "Bundled GStreamer microphone sender is not yet available on this platform".to_string(),
        ));
    }

    let remote: SocketAddr = config.remote_addr.parse().map_err(|error| {
        AppError::InvalidInput(format!(
            "Invalid microphone receiver address '{}': {error}",
            config.remote_addr
        ))
    })?;

    let sidecar = runtime::resolve_mic_sender_binary()?;

    cleanup_stale_stream_processes(&sidecar);

    let mut command = Command::new(&sidecar);
    runtime::configure_gstreamer_command(&mut command, &sidecar);
    let (stdout, stderr) = prepare_sidecar_log_stdio().unwrap_or((Stdio::null(), Stdio::null()));

    command
        .arg("stream")
        .arg("--host")
        .arg(remote.ip().to_string())
        .arg("--port")
        .arg(remote.port().to_string())
        .arg("--bitrate-kbps")
        .arg(config.quality_profile.bitrate_kbps().to_string())
        .arg("--frame-ms")
        .arg(config.quality_profile.frame_ms().to_string())
        .stdout(stdout)
        .stdin(Stdio::null())
        .stderr(stderr);

    if let Some(device_id) = config
        .device_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("default"))
    {
        command.arg("--device-id").arg(device_id);
    }

    let mut child = command.spawn().map_err(|error| {
        AppError::Command(format!(
            "Failed to start microphone sender sidecar '{}': {error}",
            sidecar.display()
        ))
    })?;

    thread::sleep(Duration::from_millis(750));
    match child.try_wait() {
        Ok(Some(status)) => {
            let log_hint = resolve_sidecar_log_path()
                .map(|path| format!(" Check {} for sidecar logs.", path.display()))
                .unwrap_or_default();
            return Err(AppError::Command(format!(
                "Microphone sender sidecar exited immediately with status {status}. macOS microphone permission may still be missing or the capture runtime failed to initialize.{log_hint}"
            )));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(AppError::Command(format!(
                "Failed to verify microphone sender sidecar startup: {error}"
            )));
        }
    }

    info!(
        session_id = config.session_id,
        ssrc = config.ssrc,
        remote_addr = %config.remote_addr,
        bitrate_kbps = config.quality_profile.bitrate_kbps(),
        frame_ms = config.quality_profile.frame_ms(),
        session_secret_len = config.session_secret.len(),
        "Started GStreamer microphone sender sidecar"
    );

    Ok(MicClientHandle { child: Some(child) })
}
