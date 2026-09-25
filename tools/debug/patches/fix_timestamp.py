import re

with open("src-tauri/src/microphone/pipeline.rs", "r") as f:
    code = f.read()

code = code.replace('"fdsrc".to_string(), "fd=0".to_string(),', '"fdsrc".to_string(), "fd=0".to_string(), "do-timestamp=true".to_string(),')

with open("src-tauri/src/microphone/pipeline.rs", "w") as f:
    f.write(code)

