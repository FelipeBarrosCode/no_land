import re

with open("src-tauri/src/microphone/state.rs", "r") as f:
    code = f.read()

old_call = """    let mut child = match spawn_gstreamer_pipeline(sample_rate, channels, &destination_host, destination_port) {"""
new_call = """    let mut child = match spawn_gstreamer_pipeline(sample_rate, channels, &destination_host, destination_port, None, None, None) {"""

code = code.replace(old_call, new_call)

with open("src-tauri/src/microphone/state.rs", "w") as f:
    f.write(code)

