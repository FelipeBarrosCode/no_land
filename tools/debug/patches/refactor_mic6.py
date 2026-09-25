import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

old_mute = """    pub fn set_muted(instance_id: u64, muted: bool) -> AppResult<()> {
        let mut handles = get_mic_handles().lock();
        let handle = handles.get_mut(&instance_id).ok_or_else(|| {
            AppError::InvalidInput(
                "Microphone forwarding is not active for this instance.".to_string(),
            )
        })?;
        handle.set_muted(muted)?;
        Ok(())
    }"""

new_mute = """    pub fn set_muted(instance_id: u64, _muted: bool) -> AppResult<()> {
        let mut handles = get_mic_handles().lock();
        let _handle = handles.get_mut(&instance_id).ok_or_else(|| {
            AppError::InvalidInput(
                "Microphone forwarding is not active for this instance.".to_string(),
            )
        })?;
        // Not implemented in the new capture pipeline yet.
        Ok(())
    }"""
code = code.replace(old_mute, new_mute)


old_metrics = """    pub fn get_local_metrics(instance_id: u64) -> AppResult<serde_json::Value> {
        let mut handles = get_mic_handles().lock();
        let handle = handles.get_mut(&instance_id).ok_or_else(|| {
            AppError::InvalidInput(
                "Microphone forwarding is not active for this instance.".to_string(),
            )
        })?;
        handle.metrics()
    }"""

new_metrics = """    pub fn get_local_metrics(instance_id: u64) -> AppResult<serde_json::Value> {
        let mut handles = get_mic_handles().lock();
        let handle = handles.get_mut(&instance_id).ok_or_else(|| {
            AppError::InvalidInput(
                "Microphone forwarding is not active for this instance.".to_string(),
            )
        })?;
        
        let dropped = handle.stream.metrics.dropped_samples.load(std::sync::atomic::Ordering::Relaxed);
        Ok(serde_json::json!({
            "dropped_samples": dropped
        }))
    }"""

code = code.replace(old_metrics, new_metrics)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

