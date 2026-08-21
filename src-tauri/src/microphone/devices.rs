use crate::microphone::types::{MicrophoneDevice, MicrophoneError};
use cpal::traits::{DeviceTrait, HostTrait};

pub fn list_microphones_sync() -> Result<Vec<MicrophoneDevice>, MicrophoneError> {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = crate::mic_client::ensure_microphone_access() {
            tracing::warn!("Microphone access denied: {}", e);
            return Err(MicrophoneError::PermissionDenied);
        }
    }

    let host = cpal::default_host();

    let default_in = host.default_input_device();
    let default_name = default_in.as_ref().and_then(|d| Some(d.to_string()));

    let devices = host
        .input_devices()
        .map_err(|_| MicrophoneError::NoInputDevice)?;

    let mut mic_list = Vec::new();

    for device in devices {
        let name = device.to_string();
        {
            let is_default = default_name.as_ref().map(|dn| dn == &name).unwrap_or(false);

            mic_list.push(MicrophoneDevice {
                id: name.clone(), // using name as id since cpal doesn't provide a persistent id
                name,
                is_default,
            });
        }
    }

    // Sort so default is first
    mic_list.sort_by(|a, b| b.is_default.cmp(&a.is_default));

    Ok(mic_list)
}

#[tauri::command]
pub async fn list_microphones() -> Result<Vec<MicrophoneDevice>, String> {
    list_microphones_sync().map_err(|e| e.to_string())
}

pub fn get_device_by_id(id: Option<&str>) -> Result<cpal::Device, MicrophoneError> {
    let host = cpal::default_host();

    if let Some(device_id) = id {
        let devices = host
            .input_devices()
            .map_err(|_| MicrophoneError::NoInputDevice)?;
        for device in devices {
            let name = device.to_string();
            {
                if name == device_id {
                    return Ok(device);
                }
            }
        }
    }

    host.default_input_device()
        .ok_or(MicrophoneError::NoInputDevice)
}
