use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

use crate::errors::{AppError, AppResult};

/// Information about a single microphone device.
#[derive(Debug, Clone, Serialize)]
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
    let host = cpal::default_host();

    let devices = host
        .input_devices()
        .map_err(|e| AppError::Command(format!("Failed to enumerate input devices: {e}")))?;

    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let mut result: Vec<MicrophoneDevice> = Vec::new();

    // Add "System Default" first
    result.push(MicrophoneDevice {
        id: "default".to_string(),
        name: "System Default".to_string(),
        is_default: true,
        sample_rates: vec![48000],
        channels: 1,
    });

    for device in devices {
        let name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());
        let is_default = default_name.as_deref() == Some(&name);

        let config_ref = device.default_input_config().ok();
        let channels = config_ref.as_ref().map(|c| c.channels() as u8).unwrap_or(1);
        let sample_rates = config_ref
            .as_ref()
            .map(|c| {
                let rate = c.sample_rate().0;
                vec![rate]
            })
            .unwrap_or_else(|| vec![48000]);

        // Skip duplicates
        if result.iter().any(|d| d.name == name) {
            continue;
        }

        result.push(MicrophoneDevice {
            id: name.clone(),
            name,
            is_default,
            sample_rates,
            channels,
        });
    }

    Ok(result)
}

/// Check whether any recording device is available.
pub fn has_recording_device() -> bool {
    list_devices()
        .map(|devices| devices.len() > 1) // More than just the "System Default" entry
        .unwrap_or(false)
}
