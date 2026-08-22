use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

use super::runtime;

/// Information about a microphone device reported by the same sidecar that
/// performs capture. `default` is a synthetic, stable selection that follows
/// the operating system's current default input device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub sample_rates: Vec<u32>,
    pub channels: u16,
}

/// Enumerate recording devices through the media sidecar so the UI and capture
/// runtime always use the same backend and identifier space.
pub fn list_devices() -> AppResult<Vec<MicrophoneDevice>> {
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok(Vec::new());
    }

    #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
    {
        let mut devices = list_devices_via_sidecar()?;
        devices.insert(
            0,
            MicrophoneDevice {
                id: "default".to_string(),
                name: "System Default".to_string(),
                is_default: true,
                sample_rates: vec![48_000],
                channels: 1,
            },
        );
        Ok(devices)
    }
}

fn list_devices_via_sidecar() -> AppResult<Vec<MicrophoneDevice>> {
    let sidecar = runtime::resolve_mic_sender_binary()?;

    let mut command = Command::new(&sidecar);
    runtime::configure_gstreamer_command(&mut command, &sidecar);
    command
        .arg("list-devices")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = run_command_with_timeout(command, Duration::from_secs(8)).map_err(|error| {
        AppError::Command(format!(
            "Failed to run microphone sender sidecar '{}': {error}",
            sidecar.display()
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!(
                "Microphone device enumeration failed with exit status {}",
                output.status
            )
        } else {
            stderr
        };
        return Err(AppError::Command(detail));
    }

    serde_json::from_slice::<Vec<MicrophoneDevice>>(&output.stdout).map_err(|error| {
        AppError::Serialization(format!(
            "Failed to parse microphone device list from sender sidecar: {error}"
        ))
    })
}

fn run_command_with_timeout(mut command: Command, timeout: Duration) -> std::io::Result<Output> {
    let child = command.spawn()?;
    wait_for_output_with_timeout(child, timeout)
}

fn wait_for_output_with_timeout(mut child: Child, timeout: Duration) -> std::io::Result<Output> {
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "command timed out after {}s: {}",
                    timeout.as_secs(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}
