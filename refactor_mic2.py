import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

code = code.replace(
    "let devices = device_list::list_devices()?;",
    "let devices = list_microphones_sync().map_err(|e| AppError::Command(e.to_string()))?;"
)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

