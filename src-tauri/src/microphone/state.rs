use cpal::traits::DeviceTrait;
use std::process::Child;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::Mutex;

use crate::microphone::capture::{start_capture, CaptureStream};
use crate::microphone::devices::get_device_by_id;
use crate::microphone::pipeline::spawn_gstreamer_pipeline;
use crate::microphone::types::{MicrophoneError, MicrophoneState, MicrophoneStatus};

static MIC_STATE: OnceLock<Arc<Mutex<MicStateWrapper>>> = OnceLock::new();

pub struct MicStateWrapper {
    pub status: MicrophoneStatus,
    pub stream: Option<CaptureStream>,
    pub child: Option<Child>,
}

pub fn get_mic_state() -> Arc<Mutex<MicStateWrapper>> {
    MIC_STATE
        .get_or_init(|| {
            Arc::new(Mutex::new(MicStateWrapper {
                status: MicrophoneStatus {
                    state: MicrophoneState::Stopped,
                    device_id: None,
                    device_name: None,
                    sample_rate: None,
                    channels: None,
                    destination: None,
                    dropped_samples: 0,
                },
                stream: None,
                child: None,
            }))
        })
        .clone()
}

#[tauri::command]
pub async fn start_microphone(
    device_id: Option<String>,
    destination_host: String,
    destination_port: u16,
) -> Result<MicrophoneStatus, String> {
    let state_arc = get_mic_state();
    let mut state = state_arc.lock().await;

    // Stop existing if any
    if state.stream.is_some() || state.child.is_some() {
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        state.stream = None;
    }

    state.status.state = MicrophoneState::Starting;
    state.status.device_id = device_id.clone();
    state.status.destination = Some(format!("{}:{}", destination_host, destination_port));
    state.status.dropped_samples = 0;

    let device = match get_device_by_id(device_id.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            state.status.state = MicrophoneState::Error;
            return Err(e.to_string());
        }
    };

    let device_name = Some(device.to_string());
    state.status.device_name = device_name;

    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            state.status.state = MicrophoneState::Error;
            return Err(e.to_string());
        }
    };
    let sample_rate = config.sample_rate();
    let channels = config.channels();

    state.status.sample_rate = Some(sample_rate);
    state.status.channels = Some(channels);

    let mut child = match spawn_gstreamer_pipeline(
        sample_rate,
        channels,
        &destination_host,
        destination_port,
        None,
        None,
        None,
        None,
        None,
    ) {
        Ok(c) => c,
        Err(e) => {
            state.status.state = MicrophoneState::Error;
            return Err(e.to_string());
        }
    };

    let stdin = child.stdin.take().ok_or_else(|| {
        state.status.state = MicrophoneState::Error;
        "Failed to open GStreamer stdin".to_string()
    })?;

    let (stream, _, _) = match start_capture(device, stdin) {
        Ok(res) => res,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            state.status.state = MicrophoneState::Error;
            return Err(e.to_string());
        }
    };

    state.stream = Some(stream);
    state.child = Some(child);
    state.status.state = MicrophoneState::Running;

    Ok(state.status.clone())
}

#[tauri::command]
pub async fn stop_microphone() -> Result<(), String> {
    let state_arc = get_mic_state();
    let mut state = state_arc.lock().await;

    if let Some(mut child) = state.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.stream = None;

    state.status.state = MicrophoneState::Stopped;

    Ok(())
}

#[tauri::command]
pub async fn microphone_status() -> Result<MicrophoneStatus, String> {
    let state_arc = get_mic_state();
    let mut state = state_arc.lock().await;

    if state.status.state == MicrophoneState::Running {
        if let Some(child) = &mut state.child {
            if let Ok(Some(_)) = child.try_wait() {
                state.status.state = MicrophoneState::Error;
            }
        }
        if let Some(stream) = &state.stream {
            state.status.dropped_samples = stream
                .metrics
                .dropped_samples
                .load(std::sync::atomic::Ordering::Relaxed);
        }
    }

    Ok(state.status.clone())
}
