use rand_core::{OsRng, RngCore};

use super::{AudioConfiguration, MoonlightError, StreamPreferences};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInputCrypto {
    pub key: [u8; 16],
    pub iv: [u8; 16],
}

impl RemoteInputCrypto {
    pub fn generate() -> Self {
        let mut key = [0u8; 16];
        let mut iv = [0u8; 16];
        OsRng.fill_bytes(&mut key);

        let ri_key_id = OsRng.next_u32();
        iv[..4].copy_from_slice(&ri_key_id.to_be_bytes());

        Self { key, iv }
    }

    pub fn key_hex(&self) -> String {
        hex_encode(&self.key)
    }

    pub fn iv_decimal(&self) -> String {
        self.ri_key_id().to_string()
    }

    pub fn ri_key_id(&self) -> u32 {
        u32::from_be_bytes([self.iv[0], self.iv[1], self.iv[2], self.iv[3]])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOperation {
    Launch,
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchDecisionInput {
    pub current_game_id: Option<u32>,
    pub requested_app_id: u32,
    pub replace_existing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchResult {
    pub operation: LaunchOperation,
    pub rtsp_session_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequestParameters {
    pub app_id: u32,
    pub mode: String,
    pub ri_key_hex: String,
    pub ri_key_id: String,
    pub audio_configuration: AudioConfiguration,
    pub play_local_audio: bool,
    pub persist_gamepads_after_disconnect: bool,
    pub hdr: bool,
}

pub fn select_launch_operation(
    input: LaunchDecisionInput,
) -> Result<LaunchOperation, MoonlightError> {
    match input.current_game_id {
        None | Some(0) => Ok(LaunchOperation::Launch),
        Some(current) if current == input.requested_app_id => Ok(LaunchOperation::Resume),
        Some(_) if input.replace_existing => Ok(LaunchOperation::Launch),
        Some(current) => Err(MoonlightError::Validation(format!(
            "different app {current} is already running; explicit replacement is required"
        ))),
    }
}

pub fn build_launch_parameters(
    app_id: u32,
    operation: LaunchOperation,
    preferences: &StreamPreferences,
    crypto: &RemoteInputCrypto,
) -> LaunchRequestParameters {
    LaunchRequestParameters {
        app_id,
        mode: format!(
            "{}x{}x{}",
            preferences.video.width, preferences.video.height, preferences.video.fps
        ),
        ri_key_hex: crypto.key_hex(),
        ri_key_id: crypto.iv_decimal(),
        audio_configuration: preferences.audio.configuration,
        play_local_audio: !preferences.audio.play_on_host,
        persist_gamepads_after_disconnect: preferences.input.persist_controllers_on_disconnect,
        hdr: preferences.video.hdr,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        build_launch_parameters, select_launch_operation, LaunchDecisionInput, LaunchOperation,
        RemoteInputCrypto,
    };
    use crate::moonlight::domain::StreamPreferences;

    #[test]
    fn chooses_launch_when_nothing_running() {
        let decision = select_launch_operation(LaunchDecisionInput {
            current_game_id: Some(0),
            requested_app_id: 10,
            replace_existing: false,
        })
        .unwrap();
        assert_eq!(decision, LaunchOperation::Launch);
    }

    #[test]
    fn chooses_resume_for_same_running_app() {
        let decision = select_launch_operation(LaunchDecisionInput {
            current_game_id: Some(10),
            requested_app_id: 10,
            replace_existing: false,
        })
        .unwrap();
        assert_eq!(decision, LaunchOperation::Resume);
    }

    #[test]
    fn rejects_different_running_app_without_replace() {
        assert!(select_launch_operation(LaunchDecisionInput {
            current_game_id: Some(99),
            requested_app_id: 10,
            replace_existing: false,
        })
        .is_err());
    }

    #[test]
    fn builds_launch_query_parameters() {
        let prefs = StreamPreferences::default();
        let crypto = RemoteInputCrypto {
            key: [1; 16],
            iv: [0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        };
        let params = build_launch_parameters(7, LaunchOperation::Launch, &prefs, &crypto);
        assert_eq!(params.app_id, 7);
        assert_eq!(params.mode, "1920x1080x60");
        assert_eq!(params.ri_key_hex.len(), 32);
        assert!(!params.ri_key_id.is_empty());
        assert_eq!(params.ri_key_id, crypto.ri_key_id().to_string());
    }
}
