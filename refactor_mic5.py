import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

old_block = """                        if !handle.is_running() {
                            Some("sidecar exited".to_string())
                        } else {
                            match handle.status() {
                                Ok(status)
                                    if status.get("health").and_then(serde_json::Value::as_str)
                                        == Some("failed") =>
                                {
                                    Some(
                                        status
                                            .get("lastError")
                                            .and_then(serde_json::Value::as_str)
                                            .unwrap_or("sidecar pipeline failed")
                                            .to_string(),
                                    )
                                }
                                Ok(_) => None,
                                Err(error) => Some(format!("sidecar IPC unhealthy: {error}")),
                            }
                        }"""

new_block = """                        if !handle.is_running() {
                            Some("gstreamer pipeline exited".to_string())
                        } else {
                            None
                        }"""

code = code.replace(old_block, new_block)

old_block2 = """            match mic_client::start_pipeline(client_config) {
                Ok(mut replacement) => {"""

new_block2 = """            
            use cpal::traits::DeviceTrait;
            let device_res = get_device_by_id(client_config.device_id.as_deref());
            if device_res.is_err() { continue; }
            let device = device_res.unwrap();
            
            let config_res = device.default_input_config();
            if config_res.is_err() { continue; }
            let config_default = config_res.unwrap();
            
            let capture_sample_rate = config_default.sample_rate().0;
            let capture_channels = config_default.channels();
            
            match spawn_gstreamer_pipeline(capture_sample_rate, capture_channels, &client_config.remote_host, client_config.rtp_port) {
                Ok(mut child) => {
                    let stdin = child.stdin.take().unwrap();
                    let (stream, _, _) = start_capture(device, stdin).unwrap();
                    let mut replacement = ActiveMicPipeline { stream, child };"""

code = code.replace(old_block2, new_block2)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

