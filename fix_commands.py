import re

with open("src-tauri/src/commands/mod.rs", "r") as f:
    code = f.read()

code = code.replace("mic_client::device_list::MicrophoneDevice,", "microphone::types::MicrophoneDevice,")

with open("src-tauri/src/commands/mod.rs", "w") as f:
    f.write(code)

