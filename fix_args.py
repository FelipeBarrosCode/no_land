import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

old_call = """        let mut child = match spawn_gstreamer_pipeline(
            capture_sample_rate, 
            capture_channels, 
            &endpoint.host, 
            endpoint.rtp_port,
            ssrc,
            sequence_offset,
            timestamp_offset,
        ) {"""

new_call = """        let mut child = match spawn_gstreamer_pipeline(
            capture_sample_rate, 
            capture_channels, 
            &endpoint.host, 
            endpoint.rtp_port,
            Some(ssrc),
            Some(sequence_offset),
            Some(timestamp_offset),
        ) {"""

code = code.replace(old_call, new_call)

old_call_2 = """            match spawn_gstreamer_pipeline(
                capture_sample_rate, 
                capture_channels, 
                &client_config.remote_host, 
                client_config.rtp_port,
                client_config.ssrc,
                client_config.sequence_offset,
                client_config.timestamp_offset,
            ) {"""

new_call_2 = """            match spawn_gstreamer_pipeline(
                capture_sample_rate, 
                capture_channels, 
                &client_config.remote_host, 
                client_config.rtp_port,
                Some(client_config.ssrc),
                Some(client_config.sequence_offset),
                Some(client_config.timestamp_offset),
            ) {"""

code = code.replace(old_call_2, new_call_2)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

