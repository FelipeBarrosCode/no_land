import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

# Replace imports
code = code.replace(
    "use crate::mic_client::device_list::{self, MicrophoneDevice};",
    "use crate::microphone::types::MicrophoneDevice;\nuse crate::microphone::devices::{self, list_microphones_sync, get_device_by_id};\nuse crate::microphone::capture::{start_capture, CaptureStream};\nuse crate::microphone::pipeline::spawn_gstreamer_pipeline;"
)
code = code.replace(
    "use crate::mic_client::{self, MicClientConfig, MicClientHandle};",
    "use crate::mic_client::MicClientConfig;"
)

# Replace MIC_HANDLES type
code = code.replace(
    "static MIC_HANDLES: std::sync::OnceLock<SyncMutex<HashMap<u64, MicClientHandle>>> =",
    "static MIC_HANDLES: std::sync::OnceLock<SyncMutex<HashMap<u64, ActiveMicPipeline>>> ="
)
code = code.replace(
    "fn get_mic_handles() -> &'static SyncMutex<HashMap<u64, MicClientHandle>> {",
    "fn get_mic_handles() -> &'static SyncMutex<HashMap<u64, ActiveMicPipeline>> {"
)

# Add ActiveMicPipeline struct
struct_def = """
pub struct ActiveMicPipeline {
    stream: CaptureStream,
    child: std::process::Child,
}

impl ActiveMicPipeline {
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
"""
code = code.replace("pub struct MicPassthroughService;", struct_def + "\n/// Microphone passthrough service.\n///\n/// Manages mic configuration, sessions, and VM agent communication\n/// for native microphone passthrough to provisioned instances.\npub struct MicPassthroughService;")


with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

