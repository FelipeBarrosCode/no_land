with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

code = code.replace("use crate::mic_client::MicClientConfig;", "use crate::mic_client::{self, MicClientConfig};")

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)
