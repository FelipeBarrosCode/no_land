use crate::errors::{AppError, AppResult};

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn noland_macos_ensure_microphone_access() -> i32;
}

pub fn ensure_microphone_access() -> AppResult<()> {
    #[cfg(target_os = "macos")]
    {
        match unsafe { noland_macos_ensure_microphone_access() } {
            0 => Ok(()),
            1 => Err(AppError::InvalidInput(
                "Microphone access is required. Allow Noland Connect in System Settings > Privacy & Security > Microphone, then try again.".to_string(),
            )),
            2 => Err(AppError::Timeout(
                "Timed out waiting for the macOS microphone permission prompt. Re-open Noland Connect and try enabling the microphone again.".to_string(),
            )),
            other => Err(AppError::Command(format!(
                "Failed to request macOS microphone access: status {other}"
            ))),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}
