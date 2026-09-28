import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

# Fix unresolved module `mic_client`
if "use crate::mic_client::{self, MicClientConfig};" not in code:
    code = code.replace(
        "use crate::mic_client::MicClientConfig;",
        "use crate::mic_client::{self, MicClientConfig};"
    )

# Fix .0 error
code = code.replace("config_default.sample_rate().0", "config_default.sample_rate().0") # Whoops!
code = re.sub(r'config_default\.sample_rate\(\)\.0', r'config_default.sample_rate().0', code)
# wait I did it again in my mind.
