use std::{
    ffi::{CStr, CString},
    ptr,
    time::Duration,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::moonlight::{
    domain::{
        transition, AudioConfiguration, ColorRange, ColorSpace, EncryptionMode, MoonlightError,
        SessionSignal, SessionState, StreamPreferences, StreamingMode,
    },
    native,
    platform::NativeSurfaceDescriptor,
};

#[derive(Debug, Clone)]
pub struct NativeStartRequest {
    pub host_id: String,
    pub app_id: u32,
    pub host_address: String,
    pub app_version: String,
    pub gfe_version: Option<String>,
    pub session_url: Option<String>,
    pub server_codec_mode_support: u32,
    pub preferences: StreamPreferences,
    pub supported_video_formats: u32,
    pub remote_input_key: [u8; 16],
    pub remote_input_iv: [u8; 16],
}

#[derive(Debug)]
pub enum RuntimeCommand {
    Start {
        request: NativeStartRequest,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    Stop {
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    AttachSurface {
        surface: NativeSurfaceDescriptor,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    DetachSurface {
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendRelativeMouse {
        delta_x: i16,
        delta_y: i16,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendAbsoluteMouse {
        x: i16,
        y: i16,
        reference_width: i16,
        reference_height: i16,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendMouseButton {
        button: u8,
        pressed: bool,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendKeyboard {
        virtual_key: u16,
        pressed: bool,
        modifiers: u8,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendControllerArrival {
        controller_number: u8,
        active_gamepad_mask: u16,
        controller_type: u8,
        supported_button_flags: u32,
        capabilities: u16,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendControllerState {
        controller_number: i16,
        active_gamepad_mask: i16,
        button_flags: i32,
        left_trigger: u8,
        right_trigger: u8,
        left_stick_x: i16,
        left_stick_y: i16,
        right_stick_x: i16,
        right_stick_y: i16,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    GetState {
        response: oneshot::Sender<SessionState>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatistics {
    pub state: String,
    pub start_count: u64,
    pub stop_count: u64,
    pub surface_attach_count: u64,
    pub surface_detach_count: u64,
    pub dropped_event_count: u64,
    pub last_width: u32,
    pub last_height: u32,
    pub has_surface: bool,
    pub estimated_rtt_ms: Option<u32>,
    pub estimated_rtt_variance_ms: Option<u32>,
    pub video_setup_count: u64,
    pub video_frame_count: u64,
    pub video_frame_event_count: u64,
    pub coalesced_video_frame_event_count: u64,
    pub renderer_ready: bool,
    pub video_session_active: bool,
    pub renderer_submitted_frame_count: u64,
    pub renderer_dropped_frame_count: u64,
    pub audio_init_count: u64,
    pub audio_sample_count: u64,
    pub mouse_move_count: u64,
    pub mouse_position_count: u64,
    pub mouse_button_count: u64,
    pub keyboard_event_count: u64,
    pub controller_arrival_count: u64,
    pub controller_state_count: u64,
    pub last_video_frame_number: i32,
    pub last_video_frame_type: i32,
    pub last_video_frame_length: i32,
    pub last_video_host_processing_latency: u16,
    pub last_video_receive_time_us: u64,
    pub last_video_enqueue_time_us: u64,
    pub last_video_presentation_time_us: u64,
    pub last_video_rtp_timestamp: u32,
    pub last_video_hdr_active: bool,
    pub last_video_colorspace: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventMessage {
    pub kind: String,
    pub code: i32,
    pub message: String,
}

#[derive(Clone)]
pub struct MoonlightRuntimeHandle {
    commands: mpsc::Sender<RuntimeCommand>,
    state: watch::Receiver<SessionState>,
    statistics: watch::Receiver<RuntimeStatistics>,
    latest_event: watch::Receiver<Option<RuntimeEventMessage>>,
    events: broadcast::Sender<RuntimeEventMessage>,
}

impl MoonlightRuntimeHandle {
    pub async fn start(&self, request: NativeStartRequest) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::Start {
                request,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped start response".to_string())
        })?
    }

    pub async fn stop(&self) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::Stop { response: tx })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped stop response".to_string())
        })?
    }

    pub async fn attach_surface(
        &self,
        surface: NativeSurfaceDescriptor,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::AttachSurface {
                surface,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped attach surface response".to_string())
        })?
    }

    pub async fn detach_surface(&self) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::DetachSurface { response: tx })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped detach surface response".to_string())
        })?
    }

    pub async fn send_relative_mouse(
        &self,
        delta_x: i16,
        delta_y: i16,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendRelativeMouse {
                delta_x,
                delta_y,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped relative mouse response".to_string())
        })?
    }

    pub async fn send_absolute_mouse(
        &self,
        x: i16,
        y: i16,
        reference_width: i16,
        reference_height: i16,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendAbsoluteMouse {
                x,
                y,
                reference_width,
                reference_height,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped absolute mouse response".to_string())
        })?
    }

    pub async fn send_mouse_button(&self, button: u8, pressed: bool) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendMouseButton {
                button,
                pressed,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped mouse button response".to_string())
        })?
    }

    pub async fn send_keyboard(
        &self,
        virtual_key: u16,
        pressed: bool,
        modifiers: u8,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendKeyboard {
                virtual_key,
                pressed,
                modifiers,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped keyboard response".to_string())
        })?
    }

    pub async fn send_controller_arrival(
        &self,
        controller_number: u8,
        active_gamepad_mask: u16,
        controller_type: u8,
        supported_button_flags: u32,
        capabilities: u16,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendControllerArrival {
                controller_number,
                active_gamepad_mask,
                controller_type,
                supported_button_flags,
                capabilities,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence(
                "runtime actor dropped controller arrival response".to_string(),
            )
        })?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_controller_state(
        &self,
        controller_number: i16,
        active_gamepad_mask: i16,
        button_flags: i32,
        left_trigger: u8,
        right_trigger: u8,
        left_stick_x: i16,
        left_stick_y: i16,
        right_stick_x: i16,
        right_stick_y: i16,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendControllerState {
                controller_number,
                active_gamepad_mask,
                button_flags,
                left_trigger,
                right_trigger,
                left_stick_x,
                left_stick_y,
                right_stick_x,
                right_stick_y,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence(
                "runtime actor dropped controller state response".to_string(),
            )
        })?
    }

    pub async fn get_state(&self) -> Result<SessionState, MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::GetState { response: tx })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence("runtime actor dropped state response".to_string())
        })
    }

    pub fn subscribe_state(&self) -> watch::Receiver<SessionState> {
        self.state.clone()
    }

    pub fn subscribe_statistics(&self) -> watch::Receiver<RuntimeStatistics> {
        self.statistics.clone()
    }

    pub fn latest_statistics(&self) -> RuntimeStatistics {
        self.statistics.borrow().clone()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<RuntimeEventMessage> {
        self.events.subscribe()
    }

    pub fn subscribe_latest_event(&self) -> watch::Receiver<Option<RuntimeEventMessage>> {
        self.latest_event.clone()
    }

    pub fn latest_event(&self) -> Option<RuntimeEventMessage> {
        self.latest_event.borrow().clone()
    }

    pub fn start_event_bridge<R: Runtime + 'static>(&self, app: AppHandle<R>) {
        let mut state_rx = self.subscribe_state();
        let mut stats_rx = self.subscribe_statistics();
        let mut event_rx = self.subscribe_events();

        tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    result = state_rx.changed() => {
                        if result.is_err() {
                            break;
                        }
                        let payload = state_rx.borrow().clone();
                        let _ = app.emit("moonlight://session-state", payload);
                    }
                    result = stats_rx.changed() => {
                        if result.is_err() {
                            break;
                        }
                        let payload = stats_rx.borrow().clone();
                        let _ = app.emit("moonlight://statistics", payload);
                    }
                    result = event_rx.recv() => {
                        match result {
                            Ok(payload) => {
                                let event_name = match payload.kind.as_str() {
                                    "error" => "moonlight://error",
                                    _ => "moonlight://connection-stage",
                                };
                                let _ = app.emit(event_name, payload);
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        });
    }
}

#[derive(Debug)]
struct NativeRuntime {
    raw: *mut native::nl_runtime_t,
}

unsafe impl Send for NativeRuntime {}

impl NativeRuntime {
    fn create() -> Result<Self, MoonlightError> {
        let mut raw = ptr::null_mut();
        let result = unsafe { native::nl_runtime_create(&mut raw) };
        map_native_result(result, "nl_runtime_create")?;
        if raw.is_null() {
            return Err(MoonlightError::Native(
                "nl_runtime_create returned a null runtime".to_string(),
            ));
        }
        Ok(Self { raw })
    }

    fn version_string() -> String {
        let ptr = unsafe { native::nl_runtime_version_string() };
        if ptr.is_null() {
            return "unknown".to_string();
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    fn start(&mut self, request: &NativeStartRequest) -> Result<(), MoonlightError> {
        let audio_configuration =
            audio_configuration_native(request.preferences.audio.configuration);
        tracing::info!(
            host_id = %request.host_id,
            app_id = request.app_id,
            host_address = %request.host_address,
            app_version = %request.app_version,
            gfe_version = ?request.gfe_version,
            session_url = ?request.session_url,
            width = request.preferences.video.width,
            height = request.preferences.video.height,
            fps = request.preferences.video.fps,
            bitrate_kbps = request.preferences.video.bitrate_kbps,
            packet_size = request.preferences.network.packet_size,
            streaming_mode = ?request.preferences.network.streaming_mode,
            audio_configuration = ?request.preferences.audio.configuration,
            audio_configuration_native = format!("0x{audio_configuration:08X}"),
            supported_video_formats = format!("0x{:08X}", request.supported_video_formats),
            encryption = ?request.preferences.network.encryption,
            "starting moonlight native stream"
        );
        let host_id = CString::new(request.host_id.as_str())
            .map_err(|error| MoonlightError::Validation(error.to_string()))?;
        let host_address = CString::new(request.host_address.as_str())
            .map_err(|error| MoonlightError::Validation(error.to_string()))?;
        let app_version = CString::new(request.app_version.as_str())
            .map_err(|error| MoonlightError::Validation(error.to_string()))?;
        let gfe_version = request
            .gfe_version
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|error| MoonlightError::Validation(error.to_string()))?;
        let session_url = request
            .session_url
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|error| MoonlightError::Validation(error.to_string()))?;

        let mut native_request = native::nl_start_request_t {
            host_id: host_id.as_ptr(),
            app_id: request.app_id,
            session_url: session_url
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            host_address: host_address.as_ptr(),
            server_app_version: app_version.as_ptr(),
            server_gfe_version: gfe_version
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            server_codec_mode_support: request.server_codec_mode_support as i32,
            width: request.preferences.video.width as i32,
            height: request.preferences.video.height as i32,
            fps: request.preferences.video.fps as i32,
            bitrate_kbps: request.preferences.video.bitrate_kbps as i32,
            packet_size: request.preferences.network.packet_size as i32,
            streaming_remotely: streaming_mode_native(request.preferences.network.streaming_mode),
            audio_configuration,
            supported_video_formats: request.supported_video_formats as i32,
            client_refresh_rate_x100: (request.preferences.video.fps * 100) as i32,
            color_space: color_space_native(request.preferences.video.color_space),
            color_range: color_range_native(request.preferences.video.color_range),
            encryption_flags: encryption_mode_native(request.preferences.network.encryption),
            remote_input_aes_key: request.remote_input_key.map(|value| value as i8),
            remote_input_aes_iv: request.remote_input_iv.map(|value| value as i8),
        };
        let result = unsafe { native::nl_runtime_start(self.raw, &mut native_request) };
        map_native_result(result, "nl_runtime_start")
    }

    fn stop(&mut self) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_runtime_request_stop(self.raw) };
        map_native_result(result, "nl_runtime_request_stop")
    }

    fn attach_surface(&mut self, surface: &NativeSurfaceDescriptor) -> Result<(), MoonlightError> {
        let native_surface = surface.to_native();
        let result = unsafe { native::nl_runtime_attach_surface(self.raw, &native_surface) };
        map_native_result(result, "nl_runtime_attach_surface")
    }

    fn detach_surface(&mut self) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_runtime_detach_surface(self.raw) };
        map_native_result(result, "nl_runtime_detach_surface")
    }

    fn send_relative_mouse(&mut self, delta_x: i16, delta_y: i16) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_send_relative_mouse(self.raw, delta_x, delta_y) };
        map_native_result(result, "nl_send_relative_mouse")
    }

    fn send_absolute_mouse(
        &mut self,
        x: i16,
        y: i16,
        reference_width: i16,
        reference_height: i16,
    ) -> Result<(), MoonlightError> {
        let result = unsafe {
            native::nl_send_absolute_mouse(self.raw, x, y, reference_width, reference_height)
        };
        map_native_result(result, "nl_send_absolute_mouse")
    }

    fn send_mouse_button(&mut self, button: u8, pressed: bool) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_send_mouse_button(self.raw, button, pressed) };
        map_native_result(result, "nl_send_mouse_button")
    }

    fn send_keyboard(
        &mut self,
        virtual_key: u16,
        pressed: bool,
        modifiers: u8,
    ) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_send_keyboard(self.raw, virtual_key, pressed, modifiers) };
        map_native_result(result, "nl_send_keyboard")
    }

    fn send_controller_arrival(
        &mut self,
        controller_number: u8,
        active_gamepad_mask: u16,
        controller_type: u8,
        supported_button_flags: u32,
        capabilities: u16,
    ) -> Result<(), MoonlightError> {
        let result = unsafe {
            native::nl_send_controller_arrival(
                self.raw,
                controller_number,
                active_gamepad_mask,
                controller_type,
                supported_button_flags,
                capabilities,
            )
        };
        map_native_result(result, "nl_send_controller_arrival")
    }

    #[allow(clippy::too_many_arguments)]
    fn send_controller_state(
        &mut self,
        controller_number: i16,
        active_gamepad_mask: i16,
        button_flags: i32,
        left_trigger: u8,
        right_trigger: u8,
        left_stick_x: i16,
        left_stick_y: i16,
        right_stick_x: i16,
        right_stick_y: i16,
    ) -> Result<(), MoonlightError> {
        let result = unsafe {
            native::nl_send_controller_state(
                self.raw,
                controller_number,
                active_gamepad_mask,
                button_flags,
                left_trigger,
                right_trigger,
                left_stick_x,
                left_stick_y,
                right_stick_x,
                right_stick_y,
            )
        };
        map_native_result(result, "nl_send_controller_state")
    }

    fn read_stats(&self) -> Result<NativeStats, MoonlightError> {
        let mut output = native::nl_stats_t {
            state: native::nl_stream_state_NL_STREAM_STATE_IDLE,
            start_count: 0,
            stop_count: 0,
            surface_attach_count: 0,
            surface_detach_count: 0,
            dropped_event_count: 0,
            last_width: 0,
            last_height: 0,
            has_surface: 0,
            estimated_rtt_ms: 0,
            estimated_rtt_variance_ms: 0,
            has_estimated_rtt: 0,
            video_setup_count: 0,
            video_frame_count: 0,
            video_frame_event_count: 0,
            coalesced_video_frame_event_count: 0,
            renderer_ready: 0,
            video_session_active: 0,
            renderer_submitted_frame_count: 0,
            renderer_dropped_frame_count: 0,
            audio_init_count: 0,
            audio_sample_count: 0,
            mouse_move_count: 0,
            mouse_position_count: 0,
            mouse_button_count: 0,
            keyboard_event_count: 0,
            controller_arrival_count: 0,
            controller_state_count: 0,
            last_video_frame_number: 0,
            last_video_frame_type: 0,
            last_video_frame_length: 0,
            last_video_host_processing_latency: 0,
            last_video_receive_time_us: 0,
            last_video_enqueue_time_us: 0,
            last_video_presentation_time_us: 0,
            last_video_rtp_timestamp: 0,
            last_video_hdr_active: 0,
            last_video_colorspace: 0,
        };
        let result = unsafe { native::nl_runtime_read_stats(self.raw, &mut output) };
        map_native_result(result, "nl_runtime_read_stats")?;
        Ok(NativeStats {
            state: output.state,
            start_count: output.start_count,
            stop_count: output.stop_count,
            surface_attach_count: output.surface_attach_count,
            surface_detach_count: output.surface_detach_count,
            dropped_event_count: output.dropped_event_count,
            last_width: output.last_width,
            last_height: output.last_height,
            has_surface: output.has_surface != 0,
            estimated_rtt_ms: (output.has_estimated_rtt != 0).then_some(output.estimated_rtt_ms),
            estimated_rtt_variance_ms: (output.has_estimated_rtt != 0)
                .then_some(output.estimated_rtt_variance_ms),
            video_setup_count: output.video_setup_count,
            video_frame_count: output.video_frame_count,
            video_frame_event_count: output.video_frame_event_count,
            coalesced_video_frame_event_count: output.coalesced_video_frame_event_count,
            renderer_ready: output.renderer_ready != 0,
            video_session_active: output.video_session_active != 0,
            renderer_submitted_frame_count: output.renderer_submitted_frame_count,
            renderer_dropped_frame_count: output.renderer_dropped_frame_count,
            audio_init_count: output.audio_init_count,
            audio_sample_count: output.audio_sample_count,
            mouse_move_count: output.mouse_move_count,
            mouse_position_count: output.mouse_position_count,
            mouse_button_count: output.mouse_button_count,
            keyboard_event_count: output.keyboard_event_count,
            controller_arrival_count: output.controller_arrival_count,
            controller_state_count: output.controller_state_count,
            last_video_frame_number: output.last_video_frame_number,
            last_video_frame_type: output.last_video_frame_type,
            last_video_frame_length: output.last_video_frame_length,
            last_video_host_processing_latency: output.last_video_host_processing_latency,
            last_video_receive_time_us: output.last_video_receive_time_us,
            last_video_enqueue_time_us: output.last_video_enqueue_time_us,
            last_video_presentation_time_us: output.last_video_presentation_time_us,
            last_video_rtp_timestamp: output.last_video_rtp_timestamp,
            last_video_hdr_active: output.last_video_hdr_active != 0,
            last_video_colorspace: output.last_video_colorspace,
        })
    }

    fn drain_events(&mut self) -> Result<Vec<NativeEvent>, MoonlightError> {
        let mut events = Vec::new();
        loop {
            let mut output = native::nl_event_t {
                kind: native::nl_event_kind_NL_EVENT_NONE,
                state: native::nl_stream_state_NL_STREAM_STATE_IDLE,
                code: 0,
                message: [0; 256],
            };
            let result = unsafe { native::nl_runtime_poll_event(self.raw, &mut output) };
            if result == native::nl_result_NL_RESULT_QUEUE_EMPTY {
                break;
            }
            map_native_result(result, "nl_runtime_poll_event")?;
            let nul_index = output
                .message
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(output.message.len());
            let message = String::from_utf8_lossy(
                &output.message[..nul_index]
                    .iter()
                    .map(|value| *value as u8)
                    .collect::<Vec<_>>(),
            )
            .into_owned();
            events.push(NativeEvent {
                kind: output.kind,
                code: output.code,
                message,
            });
        }
        Ok(events)
    }
}

impl Drop for NativeRuntime {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { native::nl_runtime_destroy(self.raw) };
            self.raw = ptr::null_mut();
        }
    }
}

#[derive(Debug, Clone)]
struct NativeStats {
    state: native::nl_stream_state_t,
    start_count: u64,
    stop_count: u64,
    surface_attach_count: u64,
    surface_detach_count: u64,
    dropped_event_count: u64,
    last_width: u32,
    last_height: u32,
    has_surface: bool,
    estimated_rtt_ms: Option<u32>,
    estimated_rtt_variance_ms: Option<u32>,
    video_setup_count: u64,
    video_frame_count: u64,
    video_frame_event_count: u64,
    coalesced_video_frame_event_count: u64,
    renderer_ready: bool,
    video_session_active: bool,
    renderer_submitted_frame_count: u64,
    renderer_dropped_frame_count: u64,
    audio_init_count: u64,
    audio_sample_count: u64,
    mouse_move_count: u64,
    mouse_position_count: u64,
    mouse_button_count: u64,
    keyboard_event_count: u64,
    controller_arrival_count: u64,
    controller_state_count: u64,
    last_video_frame_number: i32,
    last_video_frame_type: i32,
    last_video_frame_length: i32,
    last_video_host_processing_latency: u16,
    last_video_receive_time_us: u64,
    last_video_enqueue_time_us: u64,
    last_video_presentation_time_us: u64,
    last_video_rtp_timestamp: u32,
    last_video_hdr_active: bool,
    last_video_colorspace: u8,
}

#[derive(Debug, Clone)]
struct NativeEvent {
    kind: native::nl_event_kind_t,
    code: i32,
    message: String,
}

fn audio_configuration_native(configuration: AudioConfiguration) -> i32 {
    match configuration {
        AudioConfiguration::Stereo => 0x000302CA,
        AudioConfiguration::Surround51 => 0x003F06CA,
        AudioConfiguration::Surround71 => 0x063F08CA,
    }
}

fn streaming_mode_native(mode: StreamingMode) -> i32 {
    match mode {
        StreamingMode::Local => 0,
        StreamingMode::Remote => 1,
    }
}

fn color_space_native(color_space: ColorSpace) -> i32 {
    match color_space {
        ColorSpace::Rec709 => 1,
        ColorSpace::Rec2020 => 2,
    }
}

fn color_range_native(color_range: ColorRange) -> i32 {
    match color_range {
        ColorRange::Limited => 0,
        ColorRange::Full => 1,
    }
}

fn encryption_mode_native(mode: EncryptionMode) -> i32 {
    match mode {
        EncryptionMode::None => 0,
        EncryptionMode::Control => 0,
        EncryptionMode::All => -1,
    }
}

fn session_state_label(state: &SessionState) -> String {
    match state {
        SessionState::Idle => "idle",
        SessionState::Preparing => "preparing",
        SessionState::Launching => "launching",
        SessionState::CreatingSurface => "creatingSurface",
        SessionState::Connecting => "connecting",
        SessionState::Streaming => "streaming",
        SessionState::Reconnecting => "reconnecting",
        SessionState::Stopping => "stopping",
    }
    .to_string()
}

fn native_event_kind_label(kind: native::nl_event_kind_t) -> String {
    match kind {
        value if value == native::nl_event_kind_NL_EVENT_CONNECTED => "connected",
        value if value == native::nl_event_kind_NL_EVENT_STOPPED => "stopped",
        value if value == native::nl_event_kind_NL_EVENT_SURFACE_ATTACHED => "surfaceAttached",
        value if value == native::nl_event_kind_NL_EVENT_SURFACE_DETACHED => "surfaceDetached",
        value if value == native::nl_event_kind_NL_EVENT_STAGE_STARTING => "stageStarting",
        value if value == native::nl_event_kind_NL_EVENT_STAGE_COMPLETE => "stageComplete",
        value if value == native::nl_event_kind_NL_EVENT_STAGE_FAILED => "stageFailed",
        value if value == native::nl_event_kind_NL_EVENT_TERMINATED => "terminated",
        value if value == native::nl_event_kind_NL_EVENT_VIDEO_FRAME => "videoFrame",
        value if value == native::nl_event_kind_NL_EVENT_ERROR => "error",
        _ => "stateChanged",
    }
    .to_string()
}

fn runtime_statistics_from_native(state: &SessionState, stats: &NativeStats) -> RuntimeStatistics {
    let _ = stats.state;
    RuntimeStatistics {
        state: session_state_label(state),
        start_count: stats.start_count,
        stop_count: stats.stop_count,
        surface_attach_count: stats.surface_attach_count,
        surface_detach_count: stats.surface_detach_count,
        dropped_event_count: stats.dropped_event_count,
        last_width: stats.last_width,
        last_height: stats.last_height,
        has_surface: stats.has_surface,
        estimated_rtt_ms: stats.estimated_rtt_ms,
        estimated_rtt_variance_ms: stats.estimated_rtt_variance_ms,
        video_setup_count: stats.video_setup_count,
        video_frame_count: stats.video_frame_count,
        video_frame_event_count: stats.video_frame_event_count,
        coalesced_video_frame_event_count: stats.coalesced_video_frame_event_count,
        renderer_ready: stats.renderer_ready,
        video_session_active: stats.video_session_active,
        renderer_submitted_frame_count: stats.renderer_submitted_frame_count,
        renderer_dropped_frame_count: stats.renderer_dropped_frame_count,
        audio_init_count: stats.audio_init_count,
        audio_sample_count: stats.audio_sample_count,
        mouse_move_count: stats.mouse_move_count,
        mouse_position_count: stats.mouse_position_count,
        mouse_button_count: stats.mouse_button_count,
        keyboard_event_count: stats.keyboard_event_count,
        controller_arrival_count: stats.controller_arrival_count,
        controller_state_count: stats.controller_state_count,
        last_video_frame_number: stats.last_video_frame_number,
        last_video_frame_type: stats.last_video_frame_type,
        last_video_frame_length: stats.last_video_frame_length,
        last_video_host_processing_latency: stats.last_video_host_processing_latency,
        last_video_receive_time_us: stats.last_video_receive_time_us,
        last_video_enqueue_time_us: stats.last_video_enqueue_time_us,
        last_video_presentation_time_us: stats.last_video_presentation_time_us,
        last_video_rtp_timestamp: stats.last_video_rtp_timestamp,
        last_video_hdr_active: stats.last_video_hdr_active,
        last_video_colorspace: stats.last_video_colorspace,
    }
}

fn process_native_event(
    state: &mut SessionState,
    state_tx: &watch::Sender<SessionState>,
    latest_event_tx: &watch::Sender<Option<RuntimeEventMessage>>,
    event_tx: &broadcast::Sender<RuntimeEventMessage>,
    event: NativeEvent,
) {
    tracing::debug!(kind = event.kind, code = event.code, message = %event.message, "moonlight native event");

    if event.kind == native::nl_event_kind_NL_EVENT_CONNECTED {
        if let Ok(next) = transition(state, SessionSignal::ConnectionEstablished) {
            *state = next;
            let _ = state_tx.send(state.clone());
        }
    } else if (event.kind == native::nl_event_kind_NL_EVENT_STOPPED
        || event.kind == native::nl_event_kind_NL_EVENT_TERMINATED)
        && *state == SessionState::Stopping
    {
        if let Ok(next) = transition(state, SessionSignal::Stopped) {
            *state = next;
            let _ = state_tx.send(state.clone());
        }
    } else if event.kind == native::nl_event_kind_NL_EVENT_STAGE_FAILED
        || event.kind == native::nl_event_kind_NL_EVENT_STOPPED
        || event.kind == native::nl_event_kind_NL_EVENT_TERMINATED
    {
        if *state != SessionState::Idle {
            *state = SessionState::Idle;
            let _ = state_tx.send(state.clone());
        }
    }

    let payload = RuntimeEventMessage {
        kind: native_event_kind_label(event.kind),
        code: event.code,
        message: event.message,
    };

    let should_preserve_existing_failure = if event.kind == native::nl_event_kind_NL_EVENT_STOPPED {
        matches!(
            latest_event_tx
                .borrow()
                .as_ref()
                .map(|existing| existing.kind.as_str()),
            Some("stageFailed" | "error" | "terminated")
        )
    } else {
        false
    };

    if !should_preserve_existing_failure {
        let _ = latest_event_tx.send(Some(payload.clone()));
    }
    let _ = event_tx.send(payload);
}

fn map_native_result(result: native::nl_result_t, operation: &str) -> Result<(), MoonlightError> {
    match result {
        value if value == native::nl_result_NL_RESULT_OK => Ok(()),
        value if value == native::nl_result_NL_RESULT_INVALID_ARGUMENT => Err(
            MoonlightError::Native(format!("{operation} failed: invalid argument")),
        ),
        value if value == native::nl_result_NL_RESULT_OUT_OF_MEMORY => Err(MoonlightError::Native(
            format!("{operation} failed: out of memory"),
        )),
        value if value == native::nl_result_NL_RESULT_NOT_READY => Err(MoonlightError::Native(
            format!("{operation} failed: not ready"),
        )),
        value if value == native::nl_result_NL_RESULT_INVALID_STATE => Err(MoonlightError::Native(
            format!("{operation} failed: invalid state"),
        )),
        value if value == native::nl_result_NL_RESULT_QUEUE_EMPTY => Err(MoonlightError::Native(
            format!("{operation} failed: queue empty"),
        )),
        other => Err(MoonlightError::Native(format!(
            "{operation} failed with unknown native result {other}"
        ))),
    }
}

pub fn spawn_runtime_actor() -> MoonlightRuntimeHandle {
    let (command_tx, mut command_rx) = mpsc::channel::<RuntimeCommand>(32);
    let (state_tx, state_rx) = watch::channel(SessionState::Idle);
    let (stats_tx, stats_rx) = watch::channel(RuntimeStatistics {
        state: session_state_label(&SessionState::Idle),
        start_count: 0,
        stop_count: 0,
        surface_attach_count: 0,
        surface_detach_count: 0,
        dropped_event_count: 0,
        last_width: 0,
        last_height: 0,
        has_surface: false,
        estimated_rtt_ms: None,
        estimated_rtt_variance_ms: None,
        video_setup_count: 0,
        video_frame_count: 0,
        video_frame_event_count: 0,
        coalesced_video_frame_event_count: 0,
        renderer_ready: false,
        video_session_active: false,
        renderer_submitted_frame_count: 0,
        renderer_dropped_frame_count: 0,
        audio_init_count: 0,
        audio_sample_count: 0,
        mouse_move_count: 0,
        mouse_position_count: 0,
        mouse_button_count: 0,
        keyboard_event_count: 0,
        controller_arrival_count: 0,
        controller_state_count: 0,
        last_video_frame_number: 0,
        last_video_frame_type: 0,
        last_video_frame_length: 0,
        last_video_host_processing_latency: 0,
        last_video_receive_time_us: 0,
        last_video_enqueue_time_us: 0,
        last_video_presentation_time_us: 0,
        last_video_rtp_timestamp: 0,
        last_video_hdr_active: false,
        last_video_colorspace: 0,
    });
    let (latest_event_tx, latest_event_rx) = watch::channel::<Option<RuntimeEventMessage>>(None);
    let (event_tx, _) = broadcast::channel::<RuntimeEventMessage>(128);
    let handle = MoonlightRuntimeHandle {
        commands: command_tx,
        state: state_rx,
        statistics: stats_rx,
        latest_event: latest_event_rx,
        events: event_tx.clone(),
    };

    tauri::async_runtime::spawn(async move {
        let native_runtime = NativeRuntime::create();
        let mut native_runtime = match native_runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!("failed to create native moonlight runtime: {error}");
                return;
            }
        };
        tracing::info!(
            "moonlight native runtime initialized: {}",
            NativeRuntime::version_string()
        );

        let mut state = SessionState::Idle;
        let mut tick = tokio::time::interval(Duration::from_millis(250));

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Ok(native_events) = native_runtime.drain_events() {
                        for event in native_events {
                            process_native_event(&mut state, &state_tx, &latest_event_tx, &event_tx, event);
                        }
                    }
                    if let Ok(stats) = native_runtime.read_stats() {
                        let _ = stats_tx.send(runtime_statistics_from_native(&state, &stats));
                    }
                }
                maybe_command = command_rx.recv() => {
                    let Some(command) = maybe_command else { break; };
                    match command {
                        RuntimeCommand::Start { request, response } => {
                            let result = (|| -> Result<(), MoonlightError> {
                                state = transition(&state, SessionSignal::StartRequested)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::PreparationCompleted)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::LaunchCompleted)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::SurfaceCreated)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::ConnectionStarted)?;
                                let _ = state_tx.send(state.clone());
                                native_runtime.start(&request)?;
                                if let Ok(native_events) = native_runtime.drain_events() {
                                    for event in native_events {
                                        process_native_event(&mut state, &state_tx, &latest_event_tx, &event_tx, event);
                                    }
                                }
                                if let Ok(stats) = native_runtime.read_stats() {
                                    let _ = stats_tx.send(runtime_statistics_from_native(&state, &stats));
                                }
                                Ok(())
                            })();
                            let _ = response.send(result);
                        }
                        RuntimeCommand::Stop { response } => {
                            let result = (|| -> Result<(), MoonlightError> {
                                state = match state {
                                    SessionState::Idle => SessionState::Idle,
                                    SessionState::Stopping => SessionState::Stopping,
                                    _ => transition(&state, SessionSignal::StopRequested)?,
                                };
                                let _ = state_tx.send(state.clone());
                                native_runtime.stop()?;
                                if let Ok(native_events) = native_runtime.drain_events() {
                                    for event in native_events {
                                        process_native_event(&mut state, &state_tx, &latest_event_tx, &event_tx, event);
                                    }
                                }
                                if state == SessionState::Stopping {
                                    state = transition(&state, SessionSignal::Stopped)?;
                                    let _ = state_tx.send(state.clone());
                                }
                                if let Ok(stats) = native_runtime.read_stats() {
                                    let _ = stats_tx.send(runtime_statistics_from_native(&state, &stats));
                                }
                                Ok(())
                            })();
                            let _ = response.send(result);
                        }
                        RuntimeCommand::AttachSurface { surface, response } => {
                            let result = (|| -> Result<(), MoonlightError> {
                                native_runtime.attach_surface(&surface)?;
                                if let Ok(native_events) = native_runtime.drain_events() {
                                    for event in native_events {
                                        process_native_event(&mut state, &state_tx, &latest_event_tx, &event_tx, event);
                                    }
                                }
                                Ok(())
                            })();
                            let _ = response.send(result);
                        }
                        RuntimeCommand::DetachSurface { response } => {
                            let result = (|| -> Result<(), MoonlightError> {
                                native_runtime.detach_surface()?;
                                if let Ok(native_events) = native_runtime.drain_events() {
                                    for event in native_events {
                                        process_native_event(&mut state, &state_tx, &latest_event_tx, &event_tx, event);
                                    }
                                }
                                Ok(())
                            })();
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendRelativeMouse { delta_x, delta_y, response } => {
                            let result = native_runtime.send_relative_mouse(delta_x, delta_y);
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendAbsoluteMouse { x, y, reference_width, reference_height, response } => {
                            let result = native_runtime.send_absolute_mouse(x, y, reference_width, reference_height);
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendMouseButton { button, pressed, response } => {
                            let result = native_runtime.send_mouse_button(button, pressed);
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendKeyboard { virtual_key, pressed, modifiers, response } => {
                            let result = native_runtime.send_keyboard(virtual_key, pressed, modifiers);
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendControllerArrival {
                            controller_number,
                            active_gamepad_mask,
                            controller_type,
                            supported_button_flags,
                            capabilities,
                            response,
                        } => {
                            let result = native_runtime.send_controller_arrival(
                                controller_number,
                                active_gamepad_mask,
                                controller_type,
                                supported_button_flags,
                                capabilities,
                            );
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendControllerState {
                            controller_number,
                            active_gamepad_mask,
                            button_flags,
                            left_trigger,
                            right_trigger,
                            left_stick_x,
                            left_stick_y,
                            right_stick_x,
                            right_stick_y,
                            response,
                        } => {
                            let result = native_runtime.send_controller_state(
                                controller_number,
                                active_gamepad_mask,
                                button_flags,
                                left_trigger,
                                right_trigger,
                                left_stick_x,
                                left_stick_y,
                                right_stick_x,
                                right_stick_y,
                            );
                            let _ = response.send(result);
                        }
                        RuntimeCommand::GetState { response } => {
                            let _ = response.send(state.clone());
                        }
                        RuntimeCommand::Shutdown => {
                            let _ = native_runtime.stop();
                            let _ = native_runtime.detach_surface();
                            break;
                        }
                    }
                }
            }
        }
    });

    handle
}

#[cfg(test)]
mod tests {
    use crate::moonlight::{
        domain::{
            AudioConfiguration, AudioPreferences, Codec, ColorRange, ColorSpace, DecoderPreference,
            EncryptionMode, InputPreferences, MouseMode, NetworkPreferences,
            ReconnectionPreferences, StreamPreferences, StreamingMode, VideoPreferences,
            WindowMode, WindowPreferences,
        },
        native,
        platform::NativeSurfaceDescriptor,
    };

    use super::{audio_configuration_native, NativeRuntime};

    #[test]
    fn stereo_audio_configuration_matches_moonlight_layout() {
        assert_eq!(
            audio_configuration_native(AudioConfiguration::Stereo),
            0x000302CA
        );
    }

    #[test]
    fn native_runtime_stop_is_noop_while_idle() {
        let mut runtime = NativeRuntime::create().unwrap();
        runtime.stop().unwrap();
        let stats = runtime.read_stats().unwrap();
        assert_eq!(stats.start_count, 0);
        assert_eq!(stats.stop_count, 0);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn native_runtime_can_attach_and_detach_surface() {
        let mut runtime = NativeRuntime::create().unwrap();
        let surface = NativeSurfaceDescriptor {
            surface_type: native::nl_surface_type_NL_SURFACE_MACOS_NSVIEW,
            window_handle: 1,
            display_handle: 0,
            width: 800,
            height: 600,
            scale_factor: 2.0,
        };
        runtime.attach_surface(&surface).unwrap();
        let stats = runtime.read_stats().unwrap();
        assert!(stats.has_surface);
        runtime.detach_surface().unwrap();
        let stats = runtime.read_stats().unwrap();
        assert!(!stats.has_surface);
    }
}
