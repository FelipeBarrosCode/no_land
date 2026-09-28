import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

old_block = """        let pipeline_config = MicClientConfig {
            device_id: capture_device_id,
            quality_profile: profile.clone(),
            session_id: session_id.clone(),
            ssrc,
            sequence_offset,
            timestamp_offset,
            remote_host: endpoint.host.clone(),
            rtp_port: endpoint.rtp_port,
            rtcp_port: endpoint.rtcp_port,
            local_rtcp_port,
        };

        let handle = match mic_client::start_pipeline(pipeline_config.clone()) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = Self::call_vm_agent_stop_session(&remote, &target_user, &session_id).await;
                return Err(AppError::Provisioning(format!(
                    "Failed to start local microphone capture for '{}': {}",
                    selected_device_name, error
                )));
            }
        };"""

new_block = """        use cpal::traits::DeviceTrait;
        let device = get_device_by_id(capture_device_id.as_deref()).map_err(|e| AppError::Command(e.to_string()))?;
        let config_default = device.default_input_config().map_err(|e| AppError::Command(e.to_string()))?;
        let capture_sample_rate = config_default.sample_rate().0;
        let capture_channels = config_default.channels();

        let mut child = match spawn_gstreamer_pipeline(capture_sample_rate, capture_channels, &endpoint.host, endpoint.rtp_port) {
            Ok(c) => c,
            Err(e) => {
                let _ = Self::call_vm_agent_stop_session(&remote, &target_user, &session_id).await;
                return Err(AppError::Provisioning(format!("Failed to spawn gstreamer: {}", e)));
            }
        };

        let stdin = child.stdin.take().ok_or_else(|| {
            AppError::Command("Failed to get gstreamer stdin".to_string())
        })?;

        let (stream, _, _) = match start_capture(device, stdin) {
            Ok(res) => res,
            Err(e) => {
                let _ = child.kill();
                let _ = Self::call_vm_agent_stop_session(&remote, &target_user, &session_id).await;
                return Err(AppError::Provisioning(format!("Failed to start capture: {}", e)));
            }
        };

        let handle = ActiveMicPipeline {
            stream,
            child,
        };

        let pipeline_config = MicClientConfig {
            device_id: capture_device_id,
            quality_profile: profile.clone(),
            session_id: session_id.clone(),
            ssrc,
            sequence_offset,
            timestamp_offset,
            remote_host: endpoint.host.clone(),
            rtp_port: endpoint.rtp_port,
            rtcp_port: endpoint.rtcp_port,
            local_rtcp_port,
        };"""

code = code.replace(old_block, new_block)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

