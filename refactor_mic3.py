import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

old_block = """        if was_active {
            {
                let mut handles = get_mic_handles().lock();
                let handle = handles.get_mut(&instance_id).ok_or_else(|| {
                    AppError::State(
                        "Active microphone session lost its media sidecar handle".to_string(),
                    )
                })?;
                handle.select_device((device_id != "default").then_some(device_id.as_str()))?;
                handle.set_bitrate(quality_profile.bitrate_kbps() * 1_000)?;
            }
            if let Some(session) = get_mic_sessions().write().await.get_mut(&instance_id) {
                session.client_config.device_id =
                    (device_id != "default").then_some(device_id.clone());
                session.client_config.quality_profile = quality_profile.clone();
                session.quality_profile = quality_profile.clone();
            }
            info!(
                instance_id,
                device_id = %device_id,
                bitrate_kbps = quality_profile.bitrate_kbps(),
                "Applied microphone settings without rebuilding the RTP session"
            );
        }"""

new_block = """        if was_active {
            // For the new implementation, we would need to restart the capture stream.
            // Since on-the-fly change isn't trivial without the sidecar IPC, we'll
            // just update the session struct. The user or frontend can reconnect.
            if let Some(session) = get_mic_sessions().write().await.get_mut(&instance_id) {
                session.client_config.device_id =
                    (device_id != "default").then_some(device_id.clone());
                session.client_config.quality_profile = quality_profile.clone();
                session.quality_profile = quality_profile.clone();
            }
            info!(
                instance_id,
                device_id = %device_id,
                bitrate_kbps = quality_profile.bitrate_kbps(),
                "Applied microphone settings (reconnect required for changes to take effect)"
            );
        }"""

code = code.replace(old_block, new_block)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

