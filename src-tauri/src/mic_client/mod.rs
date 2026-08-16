pub mod device_list;
mod permissions;
mod runtime;

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tracing::{info, warn};

pub use permissions::ensure_microphone_access;
pub use runtime::configure_embedded_stream_runtime;

use crate::errors::{AppError, AppResult};
use crate::models::app_state::MicQualityProfile;

const IPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Negotiated configuration for the independently supervised media sidecar.
#[derive(Debug, Clone)]
pub struct MicClientConfig {
    pub device_id: Option<String>,
    pub quality_profile: MicQualityProfile,
    pub session_id: String,
    pub ssrc: u32,
    pub sequence_offset: u16,
    pub timestamp_offset: u32,
    pub remote_host: String,
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub local_rtcp_port: u16,
}

/// Handle to the JSON-lines sidecar control plane. The audio callback,
/// GStreamer pipeline, and RTP sockets all remain in the child process.
pub struct MicClientHandle {
    child: Child,
    stdin: ChildStdin,
    output: Receiver<Value>,
    next_request_id: u64,
}

impl MicClientHandle {
    pub fn stop(&mut self) {
        let _ = self.command(json!({ "command": "stopSession" }));
        let _ = self.command(json!({ "command": "shutdown" }));

        let deadline = Instant::now() + Duration::from_millis(750);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn select_device(&mut self, device_id: Option<&str>) -> AppResult<Value> {
        self.command(json!({
            "command": "selectDevice",
            "deviceId": device_id.filter(|value| !value.eq_ignore_ascii_case("default")),
        }))
    }

    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> AppResult<Value> {
        self.command(json!({ "command": "setBitrate", "bitrate": bitrate_bps }))
    }

    pub fn set_muted(&mut self, muted: bool) -> AppResult<Value> {
        self.command(json!({ "command": if muted { "mute" } else { "unmute" } }))
    }

    pub fn status(&mut self) -> AppResult<Value> {
        let status = self.command(json!({ "command": "getStatus" }))?;
        Ok(status)
    }

    pub fn metrics(&mut self) -> AppResult<Value> {
        let metrics = self.command(json!({ "command": "getMetrics" }))?;
        Ok(metrics)
    }

    fn command(&mut self, mut command: Value) -> AppResult<Value> {
        if !self.is_running() {
            return Err(AppError::Command(
                "Noland microphone media sidecar is not running".to_string(),
            ));
        }

        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let request_id = self.next_request_id;
        command["id"] = json!(request_id);
        serde_json::to_writer(&mut self.stdin, &command).map_err(|error| {
            AppError::Serialization(format!(
                "Failed serializing microphone sidecar request: {error}"
            ))
        })?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;

        let deadline = Instant::now() + IPC_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AppError::Timeout(format!(
                    "Microphone sidecar did not answer request {request_id} within {}s",
                    IPC_TIMEOUT.as_secs()
                )));
            }

            let message = match self.output.recv_timeout(remaining) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(AppError::Timeout(format!(
                        "Microphone sidecar did not answer request {request_id} within {}s",
                        IPC_TIMEOUT.as_secs()
                    )))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AppError::Command(
                        "Microphone sidecar IPC closed unexpectedly".to_string(),
                    ))
                }
            };

            if message.get("type").and_then(Value::as_str) == Some("event") {
                continue;
            }
            if message.get("type").and_then(Value::as_str) != Some("response")
                || message.get("id").and_then(Value::as_u64) != Some(request_id)
            {
                continue;
            }
            if message.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            return Err(AppError::Command(
                message
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown microphone sidecar error")
                    .to_string(),
            ));
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
    sidecar_log_paths().into_iter().find_map(|directory| {
        std::fs::create_dir_all(&directory)
            .ok()
            .map(|_| directory.join("noland-media-sidecar.log"))
    })
}

fn prepare_sidecar_stderr() -> Stdio {
    let Some(log_path) = resolve_sidecar_log_path() else {
        return Stdio::null();
    };
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null())
}

#[cfg(unix)]
fn stale_stream_pids(sidecar: &Path) -> Vec<String> {
    let output = Command::new("pgrep")
        .arg("-f")
        .arg(format!("{} (stream|daemon)", sidecar.to_string_lossy()))
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
    let pids = stale_stream_pids(sidecar);
    for pid in &pids {
        let _ = Command::new("kill").arg("-TERM").arg(pid).status();
    }
    if !pids.is_empty() {
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(not(unix))]
fn cleanup_stale_stream_processes(_sidecar: &Path) {}

pub fn cleanup_stale_pipeline_processes() -> AppResult<()> {
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    return Ok(());

    #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
    {
        let sidecar = runtime::resolve_mic_sender_binary()?;
        cleanup_stale_stream_processes(&sidecar);
        Ok(())
    }
}

/// Start and handshake with the local media sidecar over JSON-lines stdio IPC.
pub fn start_pipeline(config: MicClientConfig) -> AppResult<MicClientHandle> {
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        let _ = config;
        return Err(AppError::Command(
            "Microphone forwarding is unavailable on Windows ARM64 because the bundled GStreamer SDK is not published for that target."
                .to_string(),
        ));
    }

    let sidecar = runtime::resolve_mic_sender_binary()?;
    cleanup_stale_stream_processes(&sidecar);

    let mut command = Command::new(&sidecar);
    runtime::configure_gstreamer_command(&mut command, &sidecar);
    command
        .arg("daemon")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(prepare_sidecar_stderr());

    let mut child = command.spawn().map_err(|error| {
        AppError::Command(format!(
            "Failed to start Noland media sidecar '{}': {error}",
            sidecar.display()
        ))
    })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        AppError::Command("Microphone sidecar stdin pipe is unavailable".to_string())
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        AppError::Command("Microphone sidecar stdout pipe is unavailable".to_string())
    })?;
    let (output_tx, output_rx) = mpsc::channel();
    thread::Builder::new()
        .name("noland-mic-sidecar-ipc".to_string())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => match serde_json::from_str::<Value>(&line) {
                        Ok(message) => {
                            if output_tx.send(message).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            warn!(%error, line, "Ignoring invalid media sidecar IPC output")
                        }
                    },
                    Err(error) => {
                        warn!(%error, "Media sidecar IPC reader failed");
                        return;
                    }
                }
            }
        })
        .map_err(|error| {
            AppError::Command(format!("Failed starting sidecar IPC reader: {error}"))
        })?;

    let mut handle = MicClientHandle {
        child,
        stdin,
        output: output_rx,
        next_request_id: 0,
    };

    handle.status()?;
    handle.select_device(config.device_id.as_deref())?;
    handle.command(json!({
        "command": "startSession",
        "config": {
            "sessionId": config.session_id,
            "host": config.remote_host,
            "rtpPort": config.rtp_port,
            "rtcpPort": config.rtcp_port,
            "rtcpListenPort": config.local_rtcp_port,
            "bitrate": config.quality_profile.bitrate_kbps() * 1000,
            "frameMs": 10,
            "fec": true,
            "packetLossPercent": 5,
            "dtx": false,
            "ssrc": config.ssrc,
            "sequenceOffset": config.sequence_offset,
            "timestampOffset": config.timestamp_offset,
            "source": "microphone"
        }
    }))?;

    info!(
        session_id = %config.session_id,
        ssrc = config.ssrc,
        remote_host = %config.remote_host,
        rtp_port = config.rtp_port,
        rtcp_port = config.rtcp_port,
        local_rtcp_port = config.local_rtcp_port,
        "Started Noland microphone media sidecar"
    );
    Ok(handle)
}
