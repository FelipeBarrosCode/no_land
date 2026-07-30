use std::collections::HashSet;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

use super::runtime;

/// Information about a single microphone device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub sample_rates: Vec<u32>,
    pub channels: u8,
}

/// Enumerate all available recording devices.
pub fn list_devices() -> AppResult<Vec<MicrophoneDevice>> {
    #[cfg(target_os = "macos")]
    {
        return list_devices_macos().or_else(|mac_error| {
            list_devices_via_sidecar().map_err(|sidecar_error| {
                AppError::Command(format!(
                    "Failed to enumerate microphones via macOS system_profiler ({mac_error}) or sender sidecar ({sidecar_error})"
                ))
            })
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        list_devices_via_sidecar()
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

#[cfg(target_os = "macos")]
fn list_devices_macos() -> AppResult<Vec<MicrophoneDevice>> {
    let mut command = Command::new("system_profiler");
    command
        .arg("SPAudioDataType")
        .arg("-json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = run_command_with_timeout(command, Duration::from_secs(8)).map_err(|error| {
        AppError::Command(format!(
            "Failed to run macOS microphone enumeration via system_profiler: {error}"
        ))
    })?;

    if !output.status.success() {
        return Err(AppError::Command(format!(
            "system_profiler microphone enumeration failed with exit status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let parsed: SystemProfilerAudioResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| {
            AppError::Serialization(format!(
                "Failed to parse system_profiler microphone enumeration JSON: {error}"
            ))
        })?;

    let mut seen = HashSet::new();
    let mut devices = vec![MicrophoneDevice {
        id: "default".to_string(),
        name: "System Default".to_string(),
        is_default: true,
        sample_rates: vec![48_000],
        channels: 1,
    }];

    for host in parsed.audio_data {
        for item in host.items {
            let is_input = item.input_channels.unwrap_or(0) > 0;
            let name = match item.name {
                Some(name) if !name.trim().is_empty() => name,
                _ => continue,
            };
            if !is_input || !seen.insert(name.clone()) {
                continue;
            }

            devices.push(MicrophoneDevice {
                id: name.clone(),
                name,
                is_default: item.default_input_device.as_deref() == Some("spaudio_yes"),
                sample_rates: vec![item.sample_rate.unwrap_or(48_000)],
                channels: item.input_channels.unwrap_or(1).clamp(1, u8::MAX as u32) as u8,
            });
        }
    }

    Ok(devices)
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

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct SystemProfilerAudioResponse {
    #[serde(rename = "SPAudioDataType")]
    audio_data: Vec<SystemProfilerAudioHost>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct SystemProfilerAudioHost {
    #[serde(rename = "_items", default)]
    items: Vec<SystemProfilerAudioItem>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct SystemProfilerAudioItem {
    #[serde(rename = "_name")]
    name: Option<String>,
    #[serde(rename = "coreaudio_default_audio_input_device")]
    default_input_device: Option<String>,
    #[serde(rename = "coreaudio_device_input")]
    input_channels: Option<u32>,
    #[serde(rename = "coreaudio_device_srate")]
    sample_rate: Option<u32>,
}

/// Check whether any recording device is available.
pub fn has_recording_device() -> bool {
    list_devices()
        .map(|devices| devices.iter().any(|device| !device.is_default))
        .unwrap_or(false)
}
