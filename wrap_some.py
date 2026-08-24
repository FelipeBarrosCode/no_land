import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

code = code.replace("ssrc,\n            sequence_offset,\n            timestamp_offset,", "Some(ssrc),\n            Some(sequence_offset),\n            Some(timestamp_offset),")
code = code.replace("client_config.ssrc,\n                client_config.sequence_offset,\n                client_config.timestamp_offset,", "Some(client_config.ssrc),\n                Some(client_config.sequence_offset),\n                Some(client_config.timestamp_offset),")

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

