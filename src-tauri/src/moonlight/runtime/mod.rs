use std::{
    ffi::{CStr, CString},
    path::PathBuf,
    ptr,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::moonlight::{
    adaptive_packet_size::{AdaptivePacketSizeController, PacketSizeObservation},
    domain::{
        transition, AudioConfiguration, ColorRange, ColorSpace, EncryptionMode, FrameBufferMode,
        MoonlightError, PacingMode, RemoteStreamMode, SessionSignal, SessionState,
        StreamPreferences, StreamingMode,
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
    pub session_generation: u64,
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
    SendVerticalScroll {
        amount: i16,
        high_resolution: bool,
        response: oneshot::Sender<Result<(), MoonlightError>>,
    },
    SendHorizontalScroll {
        amount: i16,
        high_resolution: bool,
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
    RestartWithPacketSize {
        source_generation: u64,
        target: u16,
        score: u8,
        reason: String,
    },
    GetState {
        response: oneshot::Sender<SessionState>,
    },
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
    pub session_generation: u64,
    pub video_packets_interval: u32,
    pub fec_packets_interval: u32,
    pub fec_recoveries_interval: u32,
    pub fec_failures_interval: u32,
    pub out_of_sequence_packets_interval: u32,
    pub invalid_packets_interval: u32,
    pub invalid_fec_packets_interval: u32,
    pub pending_core_video_frames: i32,
    pub decoder_queue_depth: u16,
    pub render_queue_depth: u16,
    pub average_decode_pipeline_us: u64,
    pub average_render_queue_dwell_us: u64,
    pub late_frame_count: u64,
    pub adaptive_stale_drop_count: u64,
    pub pacer_backlog_drop_count: u64,
    pub renderer_error_drop_count: u64,
    pub maximum_lateness_us: u64,
    pub decoder_backpressure_time_us: u64,
    pub last_drop_lateness_us: u64,
    pub rendered_fps_x100: u32,
    pub consecutive_late_frames: u32,
    pub late_tolerance_us: u32,
    pub decoder_backpressured: bool,
    pub smoothing_queue_depth: u8,
    pub smoothing_queue_capacity: u8,
    pub max_smoothing_queue_depth: u8,
    pub smoothing_overflow_drops: u64,
    pub smoothing_underflow_repeats: u64,
    pub smoothing_reserve_budget_us: u64,
    pub frame_timing_ring_count: u32,
    pub reconnect_attempt_count: u64,
    pub reconnect_success_count: u64,
    pub resolved_remote_stream_mode: String,
    pub requested_packet_size: u32,
    pub stream_fps: u32,
    pub client_refresh_rate_x100: u32,
    pub configured_pacing_mode: String,
    pub effective_pacing_mode: String,
    pub adaptive_packet_size_enabled: bool,
    pub packet_size_controller_state: String,
    pub packet_path_label: String,
    pub packet_path_mtu_hint: Option<u32>,
    pub packet_size_last_good: Option<u16>,
    pub packet_size_bad_window_count: u8,
    pub packet_size_confidence: f32,
    pub packet_path_fingerprint: String,
    pub adaptive_packet_reconnect_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventMessage {
    pub kind: String,
    pub code: i32,
    pub session_generation: u64,
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

    pub async fn send_vertical_scroll(
        &self,
        amount: i16,
        high_resolution: bool,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendVerticalScroll {
                amount,
                high_resolution,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence(
                "runtime actor dropped vertical scroll response".to_string(),
            )
        })?
    }

    pub async fn send_horizontal_scroll(
        &self,
        amount: i16,
        high_resolution: bool,
    ) -> Result<(), MoonlightError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::SendHorizontalScroll {
                amount,
                high_resolution,
                response: tx,
            })
            .await
            .map_err(|_| MoonlightError::Persistence("runtime actor is unavailable".to_string()))?;
        rx.await.map_err(|_| {
            MoonlightError::Persistence(
                "runtime actor dropped horizontal scroll response".to_string(),
            )
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
        let resolved_remote = resolve_remote_stream_config(&request.preferences);
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
            client_refresh_rate_x100 = client_refresh_rate_x100(&request.preferences),
            bitrate_kbps = request.preferences.video.bitrate_kbps,
            packet_size = resolved_remote.packet_size,
            streaming_mode = ?resolved_remote.mode,
            audio_configuration = ?request.preferences.audio.configuration,
            audio_configuration_native = format!("0x{audio_configuration:08X}"),
            audio_target_buffer_ms = request.preferences.audio.target_buffer_ms,
            audio_maximum_buffer_ms = request.preferences.audio.maximum_buffer_ms,
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
            packet_size: resolved_remote.packet_size as i32,
            streaming_remotely: resolved_remote.streaming_remotely,
            audio_configuration,
            audio_target_buffer_ms: request.preferences.audio.target_buffer_ms,
            audio_maximum_buffer_ms: request.preferences.audio.maximum_buffer_ms,
            supported_video_formats: request.supported_video_formats as i32,
            client_refresh_rate_x100: client_refresh_rate_x100(&request.preferences) as i32,
            color_space: color_space_native(request.preferences.video.color_space),
            color_range: color_range_native(request.preferences.video.color_range),
            encryption_flags: encryption_mode_native(request.preferences.network.encryption),
            remote_input_aes_key: request.remote_input_key.map(|value| value as i8),
            remote_input_aes_iv: request.remote_input_iv.map(|value| value as i8),
            session_generation: request.session_generation,
            latency_config: native::nl_latency_config_t {
                telemetry_enabled: u8::from(request.preferences.latency.telemetry_enabled),
                adaptive_late_frame_drop_enabled: u8::from(
                    request.preferences.latency.adaptive_late_frame_drop_enabled,
                ),
                decoder_backpressure_policy_enabled: u8::from(
                    request
                        .preferences
                        .latency
                        .decoder_backpressure_policy_enabled,
                ),
                auto_reconnect_on_unexpected_termination: u8::from(
                    request
                        .preferences
                        .latency
                        .auto_reconnect_on_unexpected_termination,
                ),
                vsync_enabled: u8::from(request.preferences.latency.vsync_enabled),
                pacing_mode: pacing_mode_native(request.preferences.latency.pacing_mode),
                frame_buffer_mode: frame_buffer_mode_native(
                    request.preferences.latency.frame_buffer_mode,
                ),
                remote_stream_mode: remote_stream_mode_native(resolved_remote.mode),
                remote_packet_size: resolved_remote.packet_size as u32,
                late_frame_tolerance_us: request.preferences.latency.late_frame_tolerance_us,
            },
        };
        let result = unsafe { native::nl_runtime_start(self.raw, &mut native_request) };
        map_native_result(result, "nl_runtime_start")
    }

    fn stop(&mut self) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_runtime_request_stop(self.raw) };
        map_native_result(result, "nl_runtime_request_stop")
    }

    fn record_reconnect_result(&mut self, attempt_started: bool, succeeded: bool) {
        unsafe { native::nl_runtime_record_reconnect_result(self.raw, attempt_started, succeeded) };
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

    fn send_vertical_scroll(
        &mut self,
        amount: i16,
        high_resolution: bool,
    ) -> Result<(), MoonlightError> {
        let result = unsafe { native::nl_send_vertical_scroll(self.raw, amount, high_resolution) };
        map_native_result(result, "nl_send_vertical_scroll")
    }

    fn send_horizontal_scroll(
        &mut self,
        amount: i16,
        high_resolution: bool,
    ) -> Result<(), MoonlightError> {
        let result =
            unsafe { native::nl_send_horizontal_scroll(self.raw, amount, high_resolution) };
        map_native_result(result, "nl_send_horizontal_scroll")
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
            session_generation: 0,
            video_packets_interval: 0,
            fec_packets_interval: 0,
            fec_recoveries_interval: 0,
            fec_failures_interval: 0,
            out_of_sequence_packets_interval: 0,
            invalid_packets_interval: 0,
            invalid_fec_packets_interval: 0,
            pending_core_video_frames: -1,
            decoder_queue_depth: 0,
            render_queue_depth: 0,
            average_decode_pipeline_us: 0,
            average_render_queue_dwell_us: 0,
            late_frame_count: 0,
            adaptive_stale_drop_count: 0,
            pacer_backlog_drop_count: 0,
            renderer_error_drop_count: 0,
            maximum_lateness_us: 0,
            decoder_backpressure_time_us: 0,
            last_drop_lateness_us: 0,
            rendered_fps_x100: 0,
            consecutive_late_frames: 0,
            late_tolerance_us: 0,
            decoder_backpressured: 0,
            smoothing_queue_depth: 0,
            smoothing_queue_capacity: 0,
            max_smoothing_queue_depth: 0,
            smoothing_overflow_drops: 0,
            smoothing_underflow_repeats: 0,
            smoothing_reserve_budget_us: 0,
            frame_timing_ring_count: 0,
            reconnect_attempt_count: 0,
            reconnect_success_count: 0,
            resolved_remote_stream_mode: native::nl_remote_stream_mode_NL_REMOTE_STREAM_MODE_AUTO,
            requested_packet_size: 0,
            stream_fps: 0,
            client_refresh_rate_x100: 0,
            configured_pacing_mode: native::nl_pacing_mode_NL_PACING_MODE_OFF,
            effective_pacing_mode: native::nl_pacing_mode_NL_PACING_MODE_OFF,
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
            session_generation: output.session_generation,
            video_packets_interval: output.video_packets_interval,
            fec_packets_interval: output.fec_packets_interval,
            fec_recoveries_interval: output.fec_recoveries_interval,
            fec_failures_interval: output.fec_failures_interval,
            out_of_sequence_packets_interval: output.out_of_sequence_packets_interval,
            invalid_packets_interval: output.invalid_packets_interval,
            invalid_fec_packets_interval: output.invalid_fec_packets_interval,
            pending_core_video_frames: output.pending_core_video_frames,
            decoder_queue_depth: output.decoder_queue_depth,
            render_queue_depth: output.render_queue_depth,
            average_decode_pipeline_us: output.average_decode_pipeline_us,
            average_render_queue_dwell_us: output.average_render_queue_dwell_us,
            late_frame_count: output.late_frame_count,
            adaptive_stale_drop_count: output.adaptive_stale_drop_count,
            pacer_backlog_drop_count: output.pacer_backlog_drop_count,
            renderer_error_drop_count: output.renderer_error_drop_count,
            maximum_lateness_us: output.maximum_lateness_us,
            decoder_backpressure_time_us: output.decoder_backpressure_time_us,
            last_drop_lateness_us: output.last_drop_lateness_us,
            rendered_fps_x100: output.rendered_fps_x100,
            consecutive_late_frames: output.consecutive_late_frames,
            late_tolerance_us: output.late_tolerance_us,
            decoder_backpressured: output.decoder_backpressured != 0,
            smoothing_queue_depth: output.smoothing_queue_depth,
            smoothing_queue_capacity: output.smoothing_queue_capacity,
            max_smoothing_queue_depth: output.max_smoothing_queue_depth,
            smoothing_overflow_drops: output.smoothing_overflow_drops,
            smoothing_underflow_repeats: output.smoothing_underflow_repeats,
            smoothing_reserve_budget_us: output.smoothing_reserve_budget_us,
            frame_timing_ring_count: output.frame_timing_ring_count,
            reconnect_attempt_count: output.reconnect_attempt_count,
            reconnect_success_count: output.reconnect_success_count,
            resolved_remote_stream_mode: output.resolved_remote_stream_mode,
            requested_packet_size: output.requested_packet_size,
            stream_fps: output.stream_fps,
            client_refresh_rate_x100: output.client_refresh_rate_x100,
            configured_pacing_mode: output.configured_pacing_mode,
            effective_pacing_mode: output.effective_pacing_mode,
        })
    }

    fn drain_events(&mut self) -> Result<Vec<NativeEvent>, MoonlightError> {
        let mut events = Vec::new();
        loop {
            let mut output = native::nl_event_t {
                kind: native::nl_event_kind_NL_EVENT_NONE,
                state: native::nl_stream_state_NL_STREAM_STATE_IDLE,
                code: 0,
                session_generation: 0,
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
                session_generation: output.session_generation,
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
    session_generation: u64,
    video_packets_interval: u32,
    fec_packets_interval: u32,
    fec_recoveries_interval: u32,
    fec_failures_interval: u32,
    out_of_sequence_packets_interval: u32,
    invalid_packets_interval: u32,
    invalid_fec_packets_interval: u32,
    pending_core_video_frames: i32,
    decoder_queue_depth: u16,
    render_queue_depth: u16,
    average_decode_pipeline_us: u64,
    average_render_queue_dwell_us: u64,
    late_frame_count: u64,
    adaptive_stale_drop_count: u64,
    pacer_backlog_drop_count: u64,
    renderer_error_drop_count: u64,
    maximum_lateness_us: u64,
    decoder_backpressure_time_us: u64,
    last_drop_lateness_us: u64,
    rendered_fps_x100: u32,
    consecutive_late_frames: u32,
    late_tolerance_us: u32,
    decoder_backpressured: bool,
    smoothing_queue_depth: u8,
    smoothing_queue_capacity: u8,
    max_smoothing_queue_depth: u8,
    smoothing_overflow_drops: u64,
    smoothing_underflow_repeats: u64,
    smoothing_reserve_budget_us: u64,
    frame_timing_ring_count: u32,
    reconnect_attempt_count: u64,
    reconnect_success_count: u64,
    resolved_remote_stream_mode: native::nl_remote_stream_mode_t,
    requested_packet_size: u32,
    stream_fps: u32,
    client_refresh_rate_x100: u32,
    configured_pacing_mode: native::nl_pacing_mode_t,
    effective_pacing_mode: native::nl_pacing_mode_t,
}

#[derive(Debug, Clone)]
struct NativeEvent {
    kind: native::nl_event_kind_t,
    code: i32,
    session_generation: u64,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectCause {
    UnexpectedFailure,
    PacketSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingPacketReconnect {
    source_generation: u64,
    target: u16,
    score: u8,
    reason: String,
}

fn audio_configuration_native(configuration: AudioConfiguration) -> i32 {
    match configuration {
        AudioConfiguration::Stereo => 0x000302CA,
        AudioConfiguration::Surround51 => 0x003F06CA,
        AudioConfiguration::Surround71 => 0x063F08CA,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedRemoteStreamConfig {
    mode: RemoteStreamMode,
    streaming_remotely: i32,
    packet_size: u16,
}

fn client_refresh_rate_x100(preferences: &StreamPreferences) -> u32 {
    if preferences.video.client_refresh_rate_x100 == 0 {
        preferences.video.fps.saturating_mul(100)
    } else {
        preferences.video.client_refresh_rate_x100
    }
}

fn resolve_remote_stream_config(preferences: &StreamPreferences) -> ResolvedRemoteStreamConfig {
    let mode = match preferences.latency.remote_stream_mode {
        RemoteStreamMode::Auto => match preferences.network.streaming_mode {
            StreamingMode::Local => RemoteStreamMode::ForceLocal,
            StreamingMode::Remote => RemoteStreamMode::ForceRemote,
            StreamingMode::Auto => RemoteStreamMode::Auto,
        },
        explicit => explicit,
    };
    let packet_size = match mode {
        RemoteStreamMode::ForceRemote => preferences.latency.remote_packet_size,
        RemoteStreamMode::Auto | RemoteStreamMode::ForceLocal => preferences.network.packet_size,
    };
    let streaming_remotely = match mode {
        RemoteStreamMode::ForceLocal => 0,
        RemoteStreamMode::ForceRemote => 1,
        RemoteStreamMode::Auto => 2,
    };
    ResolvedRemoteStreamConfig {
        mode,
        streaming_remotely,
        packet_size,
    }
}

fn apply_packet_size(request: &mut NativeStartRequest, mode: RemoteStreamMode, packet_size: u16) {
    request.preferences.latency.remote_stream_mode = mode;
    match mode {
        RemoteStreamMode::ForceRemote => {
            request.preferences.latency.remote_packet_size = packet_size;
        }
        RemoteStreamMode::Auto | RemoteStreamMode::ForceLocal => {
            request.preferences.network.packet_size = packet_size;
        }
    }
}

fn prepare_packet_size_controller(
    app_data_dir: &std::path::Path,
    request: &mut NativeStartRequest,
) -> AdaptivePacketSizeController {
    let configured = resolve_remote_stream_config(&request.preferences);
    let controller = AdaptivePacketSizeController::prepare(
        app_data_dir,
        &request.host_id,
        &request.host_address,
        configured.mode,
        configured.packet_size,
        request.preferences.latency.adaptive_packet_size_enabled,
    );
    if controller.snapshot().enabled {
        apply_packet_size(
            request,
            controller.resolved_remote_mode(),
            controller.selected_packet_size(),
        );
    }
    controller
}

fn packet_size_observation(stats: &NativeStats) -> PacketSizeObservation {
    PacketSizeObservation {
        generation: stats.session_generation,
        video_packets: u64::from(stats.video_packets_interval),
        fec_packets: u64::from(stats.fec_packets_interval),
        fec_recoveries: u64::from(stats.fec_recoveries_interval),
        fec_failures: u64::from(stats.fec_failures_interval),
        out_of_sequence: u64::from(stats.out_of_sequence_packets_interval),
        invalid_packets: u64::from(stats.invalid_packets_interval),
        invalid_fec_packets: u64::from(stats.invalid_fec_packets_interval),
        estimated_rtt_ms: stats.estimated_rtt_ms,
        estimated_rtt_variance_ms: stats.estimated_rtt_variance_ms,
    }
}

#[allow(clippy::too_many_arguments)]
fn should_evaluate_packet_size_policy(
    state: &SessionState,
    desired_running: bool,
    active_generation: u64,
    stats_generation: u64,
    video_session_active: bool,
    renderer_ready: bool,
    failure_reconnect_requested: bool,
    reconnect_in_flight: Option<ReconnectCause>,
    packet_reconnect_pending: bool,
) -> bool {
    *state == SessionState::Streaming
        && desired_running
        && active_generation != 0
        && stats_generation == active_generation
        && video_session_active
        && renderer_ready
        && !failure_reconnect_requested
        && reconnect_in_flight != Some(ReconnectCause::UnexpectedFailure)
        && reconnect_in_flight != Some(ReconnectCause::PacketSize)
        && !packet_reconnect_pending
}

fn next_external_generation(
    state: &SessionState,
    active_generation: u64,
) -> Result<(SessionState, u64), MoonlightError> {
    let next_state = transition(state, SessionSignal::StartRequested)?;
    Ok((next_state, active_generation.wrapping_add(1).max(1)))
}

fn pacing_mode_native(mode: PacingMode) -> native::nl_pacing_mode_t {
    match mode {
        PacingMode::Off => native::nl_pacing_mode_NL_PACING_MODE_OFF,
        PacingMode::Automatic => native::nl_pacing_mode_NL_PACING_MODE_AUTOMATIC,
        PacingMode::Software => native::nl_pacing_mode_NL_PACING_MODE_SOFTWARE,
        PacingMode::HardwareMultiple => native::nl_pacing_mode_NL_PACING_MODE_HARDWARE_MULTIPLE,
    }
}

fn frame_buffer_mode_native(mode: FrameBufferMode) -> native::nl_frame_buffer_mode_t {
    match mode {
        FrameBufferMode::Off => native::nl_frame_buffer_mode_NL_FRAME_BUFFER_MODE_OFF,
        FrameBufferMode::OneFrame => native::nl_frame_buffer_mode_NL_FRAME_BUFFER_MODE_ONE_FRAME,
        FrameBufferMode::TwoFrames => native::nl_frame_buffer_mode_NL_FRAME_BUFFER_MODE_TWO_FRAMES,
        FrameBufferMode::ThreeFrames => {
            native::nl_frame_buffer_mode_NL_FRAME_BUFFER_MODE_THREE_FRAMES
        }
    }
}

fn remote_stream_mode_native(mode: RemoteStreamMode) -> native::nl_remote_stream_mode_t {
    match mode {
        RemoteStreamMode::Auto => native::nl_remote_stream_mode_NL_REMOTE_STREAM_MODE_AUTO,
        RemoteStreamMode::ForceRemote => {
            native::nl_remote_stream_mode_NL_REMOTE_STREAM_MODE_FORCE_REMOTE
        }
        RemoteStreamMode::ForceLocal => {
            native::nl_remote_stream_mode_NL_REMOTE_STREAM_MODE_FORCE_LOCAL
        }
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

fn pacing_mode_label(mode: native::nl_pacing_mode_t) -> String {
    match mode {
        value if value == native::nl_pacing_mode_NL_PACING_MODE_AUTOMATIC => "automatic",
        value if value == native::nl_pacing_mode_NL_PACING_MODE_SOFTWARE => "software",
        value if value == native::nl_pacing_mode_NL_PACING_MODE_HARDWARE_MULTIPLE => {
            "hardwareMultiple"
        }
        _ => "off",
    }
    .to_string()
}

fn remote_stream_mode_label(mode: native::nl_remote_stream_mode_t) -> String {
    match mode {
        value if value == native::nl_remote_stream_mode_NL_REMOTE_STREAM_MODE_FORCE_REMOTE => {
            "forceRemote"
        }
        value if value == native::nl_remote_stream_mode_NL_REMOTE_STREAM_MODE_FORCE_LOCAL => {
            "forceLocal"
        }
        _ => "auto",
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

fn runtime_statistics_from_native(
    state: &SessionState,
    stats: &NativeStats,
    packet_size_controller: Option<&AdaptivePacketSizeController>,
    adaptive_packet_reconnect_count: u64,
) -> RuntimeStatistics {
    let _ = stats.state;
    let controller_snapshot = packet_size_controller.map(AdaptivePacketSizeController::snapshot);
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
        session_generation: stats.session_generation,
        video_packets_interval: stats.video_packets_interval,
        fec_packets_interval: stats.fec_packets_interval,
        fec_recoveries_interval: stats.fec_recoveries_interval,
        fec_failures_interval: stats.fec_failures_interval,
        out_of_sequence_packets_interval: stats.out_of_sequence_packets_interval,
        invalid_packets_interval: stats.invalid_packets_interval,
        invalid_fec_packets_interval: stats.invalid_fec_packets_interval,
        pending_core_video_frames: stats.pending_core_video_frames,
        decoder_queue_depth: stats.decoder_queue_depth,
        render_queue_depth: stats.render_queue_depth,
        average_decode_pipeline_us: stats.average_decode_pipeline_us,
        average_render_queue_dwell_us: stats.average_render_queue_dwell_us,
        late_frame_count: stats.late_frame_count,
        adaptive_stale_drop_count: stats.adaptive_stale_drop_count,
        pacer_backlog_drop_count: stats.pacer_backlog_drop_count,
        renderer_error_drop_count: stats.renderer_error_drop_count,
        maximum_lateness_us: stats.maximum_lateness_us,
        decoder_backpressure_time_us: stats.decoder_backpressure_time_us,
        last_drop_lateness_us: stats.last_drop_lateness_us,
        rendered_fps_x100: stats.rendered_fps_x100,
        consecutive_late_frames: stats.consecutive_late_frames,
        late_tolerance_us: stats.late_tolerance_us,
        decoder_backpressured: stats.decoder_backpressured,
        smoothing_queue_depth: stats.smoothing_queue_depth,
        smoothing_queue_capacity: stats.smoothing_queue_capacity,
        max_smoothing_queue_depth: stats.max_smoothing_queue_depth,
        smoothing_overflow_drops: stats.smoothing_overflow_drops,
        smoothing_underflow_repeats: stats.smoothing_underflow_repeats,
        smoothing_reserve_budget_us: stats.smoothing_reserve_budget_us,
        frame_timing_ring_count: stats.frame_timing_ring_count,
        reconnect_attempt_count: stats.reconnect_attempt_count,
        reconnect_success_count: stats.reconnect_success_count,
        resolved_remote_stream_mode: remote_stream_mode_label(stats.resolved_remote_stream_mode),
        requested_packet_size: stats.requested_packet_size,
        stream_fps: stats.stream_fps,
        client_refresh_rate_x100: stats.client_refresh_rate_x100,
        configured_pacing_mode: pacing_mode_label(stats.configured_pacing_mode),
        effective_pacing_mode: pacing_mode_label(stats.effective_pacing_mode),
        adaptive_packet_size_enabled: controller_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.enabled),
        packet_size_controller_state: controller_snapshot
            .as_ref()
            .map_or_else(String::new, |snapshot| snapshot.state_label.clone()),
        packet_path_label: controller_snapshot
            .as_ref()
            .map_or_else(String::new, |snapshot| snapshot.path_label.clone()),
        packet_path_mtu_hint: controller_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.mtu_hint),
        packet_size_last_good: controller_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.last_good),
        packet_size_bad_window_count: controller_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.bad_window_count),
        packet_size_confidence: controller_snapshot
            .as_ref()
            .map_or(0.0, |snapshot| snapshot.confidence),
        packet_path_fingerprint: controller_snapshot
            .as_ref()
            .map_or_else(String::new, |snapshot| snapshot.fingerprint.clone()),
        adaptive_packet_reconnect_count,
    }
}

fn should_reset_unexpected_reconnect_budget(
    state: &SessionState,
    reconnect_attempted: bool,
    stable_since: Option<Instant>,
    now: Instant,
) -> bool {
    *state == SessionState::Streaming
        && reconnect_attempted
        && stable_since.is_some_and(|since| now.duration_since(since) >= Duration::from_secs(30))
}

fn should_auto_reconnect(
    state: &SessionState,
    event: &NativeEvent,
    desired_running: bool,
    active_request: Option<&NativeStartRequest>,
    reconnect_attempted: bool,
) -> bool {
    *state == SessionState::Streaming
        && event.kind == native::nl_event_kind_NL_EVENT_TERMINATED
        && event.code != 0
        && desired_running
        && !reconnect_attempted
        && active_request.is_some_and(|request| {
            request.preferences.reconnection.enabled
                && request
                    .preferences
                    .latency
                    .auto_reconnect_on_unexpected_termination
        })
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
    } else if event.kind == native::nl_event_kind_NL_EVENT_TERMINATED
        && event.code != 0
        && *state == SessionState::Streaming
    {
        if let Ok(next) = transition(state, SessionSignal::ConnectionLost) {
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
        session_generation: event.session_generation,
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
    } else if event.kind == native::nl_event_kind_NL_EVENT_ERROR {
        matches!(
            latest_event_tx
                .borrow()
                .as_ref()
                .map(|existing| existing.kind.as_str()),
            Some("stageFailed" | "terminated")
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

fn format_runtime_event_for_error(event: &RuntimeEventMessage) -> String {
    if event.message.trim().is_empty() {
        format!("{} ({})", event.kind, event.code)
    } else {
        format!("{}: {} ({})", event.kind, event.message, event.code)
    }
}

fn enrich_native_start_error(
    error: MoonlightError,
    latest_event: Option<&RuntimeEventMessage>,
    stats: Option<&RuntimeStatistics>,
) -> MoonlightError {
    let mut details = Vec::new();

    if let Some(event) = latest_event {
        details.push(format!(
            "latest_event={}",
            format_runtime_event_for_error(event)
        ));
    }

    if let Some(stats) = stats {
        details.push(format!(
            "state={} renderer_ready={} video_session_active={} video_frame_count={} submitted_frames={} dropped_frames={} audio_samples={}",
            stats.state,
            stats.renderer_ready,
            stats.video_session_active,
            stats.video_frame_count,
            stats.renderer_submitted_frame_count,
            stats.renderer_dropped_frame_count,
            stats.audio_sample_count,
        ));
    }

    if details.is_empty() {
        error
    } else {
        MoonlightError::Native(format!("{error}; {}", details.join("; ")))
    }
}

pub fn spawn_runtime_actor(app_data_dir: PathBuf) -> MoonlightRuntimeHandle {
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
        session_generation: 0,
        video_packets_interval: 0,
        fec_packets_interval: 0,
        fec_recoveries_interval: 0,
        fec_failures_interval: 0,
        out_of_sequence_packets_interval: 0,
        invalid_packets_interval: 0,
        invalid_fec_packets_interval: 0,
        pending_core_video_frames: -1,
        decoder_queue_depth: 0,
        render_queue_depth: 0,
        average_decode_pipeline_us: 0,
        average_render_queue_dwell_us: 0,
        late_frame_count: 0,
        adaptive_stale_drop_count: 0,
        pacer_backlog_drop_count: 0,
        renderer_error_drop_count: 0,
        maximum_lateness_us: 0,
        decoder_backpressure_time_us: 0,
        last_drop_lateness_us: 0,
        rendered_fps_x100: 0,
        consecutive_late_frames: 0,
        late_tolerance_us: 0,
        decoder_backpressured: false,
        smoothing_queue_depth: 0,
        smoothing_queue_capacity: 0,
        max_smoothing_queue_depth: 0,
        smoothing_overflow_drops: 0,
        smoothing_underflow_repeats: 0,
        smoothing_reserve_budget_us: 0,
        frame_timing_ring_count: 0,
        reconnect_attempt_count: 0,
        reconnect_success_count: 0,
        resolved_remote_stream_mode: "auto".to_string(),
        requested_packet_size: 0,
        stream_fps: 0,
        client_refresh_rate_x100: 0,
        configured_pacing_mode: "off".to_string(),
        effective_pacing_mode: "off".to_string(),
        adaptive_packet_size_enabled: false,
        packet_size_controller_state: String::new(),
        packet_path_label: String::new(),
        packet_path_mtu_hint: None,
        packet_size_last_good: None,
        packet_size_bad_window_count: 0,
        packet_size_confidence: 0.0,
        packet_path_fingerprint: String::new(),
        adaptive_packet_reconnect_count: 0,
    });
    let (latest_event_tx, latest_event_rx) = watch::channel::<Option<RuntimeEventMessage>>(None);
    let (event_tx, _) = broadcast::channel::<RuntimeEventMessage>(128);
    let handle = MoonlightRuntimeHandle {
        commands: command_tx.clone(),
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
        let mut last_video_frame_count = 0_u64;
        let mut last_video_progress_at = Instant::now();
        let mut active_request: Option<NativeStartRequest> = None;
        let mut desired_running = false;
        let mut active_generation = 0_u64;
        let mut unexpected_reconnect_attempted = false;
        let mut unexpected_reconnect_stable_since: Option<Instant> = None;
        let mut reconnect_in_flight: Option<ReconnectCause> = None;
        let mut packet_reconnect_pending: Option<PendingPacketReconnect> = None;
        let mut packet_size_controller: Option<AdaptivePacketSizeController> = None;
        let mut adaptive_packet_reconnect_count = 0_u64;

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let mut failure_reconnect_requested = false;
                    let mut terminal_cleanup_requested = false;
                    if let Ok(native_events) = native_runtime.drain_events() {
                        for event in native_events {
                            if event.session_generation != 0
                                && event.session_generation != active_generation
                            {
                                tracing::debug!(
                                    event_generation = event.session_generation,
                                    active_generation,
                                    "ignoring stale moonlight native event"
                                );
                                continue;
                            }

                            let connected = event.kind == native::nl_event_kind_NL_EVENT_CONNECTED;
                            let terminal = event.kind == native::nl_event_kind_NL_EVENT_STAGE_FAILED
                                || event.kind == native::nl_event_kind_NL_EVENT_STOPPED
                                || event.kind == native::nl_event_kind_NL_EVENT_TERMINATED;
                            let reconnect_teardown = terminal
                                && state == SessionState::Reconnecting
                                && (packet_reconnect_pending.is_some()
                                    || failure_reconnect_requested);
                            if reconnect_teardown {
                                tracing::debug!(
                                    kind = event.kind,
                                    generation = event.session_generation,
                                    "drained reconnect teardown event"
                                );
                                continue;
                            }

                            let should_reconnect = should_auto_reconnect(
                                &state,
                                &event,
                                desired_running,
                                active_request.as_ref(),
                                unexpected_reconnect_attempted,
                            );
                            failure_reconnect_requested |= should_reconnect;
                            terminal_cleanup_requested |= terminal && !should_reconnect;
                            let matching_connected = connected
                                && event.session_generation == active_generation;
                            process_native_event(
                                &mut state,
                                &state_tx,
                                &latest_event_tx,
                                &event_tx,
                                event,
                            );

                            if matching_connected {
                                let now = Instant::now();
                                let completed_reconnect = reconnect_in_flight.take();
                                if completed_reconnect.is_some() {
                                    native_runtime.record_reconnect_result(false, true);
                                }
                                match completed_reconnect {
                                    Some(ReconnectCause::UnexpectedFailure) => {
                                        unexpected_reconnect_stable_since = Some(now);
                                    }
                                    Some(ReconnectCause::PacketSize) if unexpected_reconnect_attempted => {
                                        unexpected_reconnect_stable_since = Some(now);
                                    }
                                    None => {
                                        unexpected_reconnect_attempted = false;
                                        unexpected_reconnect_stable_since = None;
                                    }
                                    _ => {}
                                }
                                if let Some(controller) = packet_size_controller.as_mut() {
                                    controller.on_connected(active_generation, now);
                                }
                            }
                        }
                    }

                    if should_reset_unexpected_reconnect_budget(
                        &state,
                        unexpected_reconnect_attempted,
                        unexpected_reconnect_stable_since,
                        Instant::now(),
                    ) {
                        unexpected_reconnect_attempted = false;
                        unexpected_reconnect_stable_since = None;
                    }

                    if failure_reconnect_requested {
                        unexpected_reconnect_attempted = true;
                        unexpected_reconnect_stable_since = None;
                        if let Some(mut request) = active_request.clone() {
                            let _ = native_runtime.stop();
                            let _ = native_runtime.drain_events();
                            active_generation = active_generation.wrapping_add(1).max(1);
                            request.session_generation = active_generation;
                            active_request = Some(request.clone());
                            if let Ok(next) = transition(&state, SessionSignal::ReconnectRequested) {
                                state = next;
                                let _ = state_tx.send(state.clone());
                            }
                            native_runtime.record_reconnect_result(true, false);
                            match native_runtime.start(&request) {
                                Ok(()) => {
                                    reconnect_in_flight = Some(ReconnectCause::UnexpectedFailure);
                                    last_video_frame_count = 0;
                                    last_video_progress_at = Instant::now();
                                }
                                Err(error) => {
                                    tracing::error!(%error, "one-shot moonlight reconnect failed");
                                    reconnect_in_flight = None;
                                    desired_running = false;
                                    active_request = None;
                                    packet_reconnect_pending = None;
                                    packet_size_controller = None;
                                    unexpected_reconnect_attempted = false;
                                    unexpected_reconnect_stable_since = None;
                                    let _ = native_runtime.stop();
                                    let _ = native_runtime.drain_events();
                                    state = SessionState::Idle;
                                    let _ = state_tx.send(state.clone());
                                }
                            }
                        } else {
                            terminal_cleanup_requested = true;
                        }
                    }

                    if terminal_cleanup_requested && !failure_reconnect_requested {
                        desired_running = false;
                        active_request = None;
                        packet_reconnect_pending = None;
                        packet_size_controller = None;
                        reconnect_in_flight = None;
                        unexpected_reconnect_attempted = false;
                        unexpected_reconnect_stable_since = None;
                        let _ = native_runtime.stop();
                        let _ = native_runtime.drain_events();
                        if state != SessionState::Idle {
                            state = SessionState::Idle;
                            let _ = state_tx.send(state.clone());
                        }
                    }
                    if let Ok(stats) = native_runtime.read_stats() {
                        if stats.video_frame_count != last_video_frame_count {
                            last_video_frame_count = stats.video_frame_count;
                            last_video_progress_at = Instant::now();
                        }

                        let should_stop_for_stall = matches!(
                            state,
                            SessionState::Streaming | SessionState::Reconnecting
                        ) && stats.video_session_active
                            && stats.renderer_ready
                            && stats.video_frame_count > 0
                            && Instant::now().duration_since(last_video_progress_at)
                                > Duration::from_secs(10);

                        if should_stop_for_stall {
                            let payload = RuntimeEventMessage {
                                kind: "error".to_string(),
                                code: -4100,
                                session_generation: active_generation,
                                message: "Video frames stopped arriving for 10 seconds. Ending stream."
                                    .to_string(),
                            };
                            let _ = latest_event_tx.send(Some(payload.clone()));
                            let _ = event_tx.send(payload);
                            desired_running = false;
                            active_request = None;
                            packet_reconnect_pending = None;
                            packet_size_controller = None;
                            reconnect_in_flight = None;
                            unexpected_reconnect_attempted = false;
                            unexpected_reconnect_stable_since = None;
                            let _ = native_runtime.stop();
                            let _ = native_runtime.drain_events();
                            if let Ok(next) = transition(&state, SessionSignal::StopRequested) {
                                state = next;
                                let _ = state_tx.send(state.clone());
                            }
                            if let Ok(next) = transition(&state, SessionSignal::Stopped) {
                                state = next;
                            } else {
                                state = SessionState::Idle;
                            }
                            let _ = state_tx.send(state.clone());
                        }

                        let should_evaluate = should_evaluate_packet_size_policy(
                            &state,
                            desired_running,
                            active_generation,
                            stats.session_generation,
                            stats.video_session_active,
                            stats.renderer_ready,
                            failure_reconnect_requested,
                            reconnect_in_flight,
                            packet_reconnect_pending.is_some(),
                        );
                        let decision = if should_evaluate {
                            packet_size_controller.as_mut().and_then(|controller| {
                                controller.observe(packet_size_observation(&stats), Instant::now())
                            })
                        } else {
                            None
                        };

                        if let Some(decision) = decision {
                            if let Some(controller) = packet_size_controller.as_mut() {
                                controller.commit_downshift(decision.to);
                            }
                            let pending = PendingPacketReconnect {
                                source_generation: active_generation,
                                target: decision.to,
                                score: decision.score,
                                reason: decision.reason.clone(),
                            };
                            if let Ok(next) = transition(
                                &state,
                                SessionSignal::ControlledReconnectRequested,
                            ) {
                                state = next;
                                packet_reconnect_pending = Some(pending.clone());
                                let _ = state_tx.send(state.clone());
                                let payload = RuntimeEventMessage {
                                    kind: "packetSizeReconnecting".to_string(),
                                    code: 0,
                                    session_generation: active_generation,
                                    message: format!(
                                        "packet size reconnect from={} to={} score={} reason={}",
                                        decision.from,
                                        decision.to,
                                        decision.score,
                                        decision.reason,
                                    ),
                                };
                                let _ = latest_event_tx.send(Some(payload.clone()));
                                let _ = event_tx.send(payload);
                                let _ = stats_tx.send(runtime_statistics_from_native(
                                    &state,
                                    &stats,
                                    packet_size_controller.as_ref(),
                                    adaptive_packet_reconnect_count,
                                ));
                                let _ = native_runtime.stop();
                                let _ = native_runtime.drain_events();
                                let command = RuntimeCommand::RestartWithPacketSize {
                                    source_generation: pending.source_generation,
                                    target: pending.target,
                                    score: pending.score,
                                    reason: pending.reason,
                                };
                                if let Err(error) = command_tx.try_send(command) {
                                    tracing::error!(%error, "failed to queue packet-size reconnect");
                                    desired_running = false;
                                    active_request = None;
                                    packet_reconnect_pending = None;
                                    packet_size_controller = None;
                                    reconnect_in_flight = None;
                                    unexpected_reconnect_attempted = false;
                                    unexpected_reconnect_stable_since = None;
                                    state = SessionState::Idle;
                                    let _ = state_tx.send(state.clone());
                                }
                                continue;
                            }
                        }

                        let _ = stats_tx.send(runtime_statistics_from_native(
                            &state,
                            &stats,
                            packet_size_controller.as_ref(),
                            adaptive_packet_reconnect_count,
                        ));
                    }
                }
                maybe_command = command_rx.recv() => {
                    let Some(command) = maybe_command else { break; };
                    match command {
                        RuntimeCommand::Start { mut request, response } => {
                            let result = (|| -> Result<(), MoonlightError> {
                                let (preparing_state, next_generation) =
                                    next_external_generation(&state, active_generation)?;
                                let controller =
                                    prepare_packet_size_controller(&app_data_dir, &mut request);

                                state = preparing_state;
                                active_generation = next_generation;
                                request.session_generation = active_generation;
                                desired_running = true;
                                unexpected_reconnect_attempted = false;
                                unexpected_reconnect_stable_since = None;
                                reconnect_in_flight = None;
                                packet_reconnect_pending = None;
                                packet_size_controller = Some(controller);
                                active_request = Some(request.clone());
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::PreparationCompleted)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::LaunchCompleted)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::SurfaceCreated)?;
                                let _ = state_tx.send(state.clone());
                                state = transition(&state, SessionSignal::ConnectionStarted)?;
                                let _ = state_tx.send(state.clone());
                                last_video_frame_count = 0;
                                last_video_progress_at = Instant::now();

                                if let Err(error) = native_runtime.start(&request) {
                                    if let Ok(native_events) = native_runtime.drain_events() {
                                        for event in native_events {
                                            process_native_event(&mut state, &state_tx, &latest_event_tx, &event_tx, event);
                                        }
                                    }

                                    let runtime_stats = native_runtime.read_stats().ok().map(|stats| {
                                        let runtime_stats = runtime_statistics_from_native(
                                            &state,
                                            &stats,
                                            packet_size_controller.as_ref(),
                                            adaptive_packet_reconnect_count,
                                        );
                                        let _ = stats_tx.send(runtime_stats.clone());
                                        runtime_stats
                                    });
                                    let latest_event = latest_event_tx.borrow().clone();
                                    let enriched_error = enrich_native_start_error(
                                        error,
                                        latest_event.as_ref(),
                                        runtime_stats.as_ref(),
                                    );
                                    desired_running = false;
                                    active_request = None;
                                    packet_reconnect_pending = None;
                                    packet_size_controller = None;
                                    reconnect_in_flight = None;
                                    let _ = native_runtime.stop();
                                    let _ = native_runtime.drain_events();
                                    state = SessionState::Idle;
                                    let _ = state_tx.send(state.clone());
                                    tracing::error!(
                                        host_id = %request.host_id,
                                        app_id = request.app_id,
                                        error = %enriched_error,
                                        latest_event = ?latest_event,
                                        runtime_stats = ?runtime_stats,
                                        "moonlight native start failed"
                                    );
                                    return Err(enriched_error);
                                }

                                if let Ok(stats) = native_runtime.read_stats() {
                                    let _ = stats_tx.send(runtime_statistics_from_native(
                                        &state,
                                        &stats,
                                        packet_size_controller.as_ref(),
                                        adaptive_packet_reconnect_count,
                                    ));
                                }
                                Ok(())
                            })();
                            let _ = response.send(result);
                        }
                        RuntimeCommand::Stop { response } => {
                            let result = (|| -> Result<(), MoonlightError> {
                                desired_running = false;
                                active_request = None;
                                unexpected_reconnect_attempted = false;
                                unexpected_reconnect_stable_since = None;
                                reconnect_in_flight = None;
                                packet_reconnect_pending = None;
                                packet_size_controller = None;
                                state = match state {
                                    SessionState::Idle => SessionState::Idle,
                                    SessionState::Stopping => SessionState::Stopping,
                                    _ => transition(&state, SessionSignal::StopRequested)?,
                                };
                                let _ = state_tx.send(state.clone());
                                native_runtime.stop()?;
                                last_video_frame_count = 0;
                                last_video_progress_at = Instant::now();
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
                                    let _ = stats_tx.send(runtime_statistics_from_native(
                                        &state,
                                        &stats,
                                        packet_size_controller.as_ref(),
                                        adaptive_packet_reconnect_count,
                                    ));
                                }
                                Ok(())
                            })();
                            let _ = response.send(result);
                        }
                        RuntimeCommand::RestartWithPacketSize {
                            source_generation,
                            target,
                            score,
                            reason,
                        } => {
                            let matches_pending = packet_reconnect_pending.as_ref().is_some_and(
                                |pending| {
                                    pending.source_generation == source_generation
                                        && pending.target == target
                                        && pending.score == score
                                        && pending.reason == reason
                                },
                            );
                            if !matches_pending
                                || source_generation != active_generation
                                || state != SessionState::Reconnecting
                                || !desired_running
                                || active_request.is_none()
                                || packet_size_controller.is_none()
                                || reconnect_in_flight.is_some()
                            {
                                tracing::debug!(
                                    source_generation,
                                    active_generation,
                                    target,
                                    score,
                                    %reason,
                                    "ignoring stale packet-size reconnect command"
                                );
                                continue;
                            }

                            let mut request = active_request.clone().expect("checked above");
                            let controller = packet_size_controller.as_ref().expect("checked above");
                            active_generation = active_generation.wrapping_add(1).max(1);
                            request.session_generation = active_generation;
                            apply_packet_size(
                                &mut request,
                                controller.resolved_remote_mode(),
                                target,
                            );
                            active_request = Some(request.clone());
                            packet_reconnect_pending = None;
                            state = match transition(&state, SessionSignal::ReconnectRequested) {
                                Ok(next) => next,
                                Err(error) => {
                                    tracing::error!(%error, "packet-size reconnect transition failed");
                                    desired_running = false;
                                    active_request = None;
                                    packet_size_controller = None;
                                    reconnect_in_flight = None;
                                    unexpected_reconnect_attempted = false;
                                    unexpected_reconnect_stable_since = None;
                                    let _ = native_runtime.stop();
                                    let _ = native_runtime.drain_events();
                                    let _ = state_tx.send(SessionState::Idle);
                                    state = SessionState::Idle;
                                    continue;
                                }
                            };
                            let _ = state_tx.send(state.clone());
                            adaptive_packet_reconnect_count =
                                adaptive_packet_reconnect_count.saturating_add(1);
                            native_runtime.record_reconnect_result(true, false);
                            match native_runtime.start(&request) {
                                Ok(()) => {
                                    reconnect_in_flight = Some(ReconnectCause::PacketSize);
                                    last_video_frame_count = 0;
                                    last_video_progress_at = Instant::now();
                                }
                                Err(error) => {
                                    tracing::error!(%error, "packet-size reconnect failed");
                                    desired_running = false;
                                    active_request = None;
                                    packet_size_controller = None;
                                    reconnect_in_flight = None;
                                    unexpected_reconnect_attempted = false;
                                    unexpected_reconnect_stable_since = None;
                                    let _ = native_runtime.stop();
                                    let _ = native_runtime.drain_events();
                                    state = SessionState::Idle;
                                    let _ = state_tx.send(state.clone());
                                }
                            }
                        }
                        RuntimeCommand::AttachSurface { surface, response } => {
                            let result = native_runtime.attach_surface(&surface);
                            let _ = response.send(result);
                        }
                        RuntimeCommand::DetachSurface { response } => {
                            let result = native_runtime.detach_surface();
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
                        RuntimeCommand::SendVerticalScroll { amount, high_resolution, response } => {
                            let result = native_runtime.send_vertical_scroll(amount, high_resolution);
                            let _ = response.send(result);
                        }
                        RuntimeCommand::SendHorizontalScroll { amount, high_resolution, response } => {
                            let result = native_runtime.send_horizontal_scroll(amount, high_resolution);
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

                    }
                }
            }
        }
    });

    handle
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "macos"))]
    use crate::moonlight::platform::NativeSurfaceDescriptor;
    use crate::moonlight::{
        domain::{
            AudioConfiguration, RemoteStreamMode, SessionState, StreamPreferences, StreamingMode,
        },
        native,
    };

    use super::{
        apply_packet_size, audio_configuration_native, client_refresh_rate_x100,
        next_external_generation, prepare_packet_size_controller, process_native_event,
        resolve_remote_stream_config, should_auto_reconnect, should_evaluate_packet_size_policy,
        should_reset_unexpected_reconnect_budget, NativeEvent, NativeRuntime, NativeStartRequest,
        ReconnectCause, RuntimeEventMessage,
    };
    use std::{
        fs,
        mem::size_of,
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use tokio::sync::{broadcast, watch};

    #[test]
    fn handwritten_native_bindings_match_c_abi_sizes() {
        assert_eq!(size_of::<native::nl_start_request_t>(), unsafe {
            native::nl_sizeof_start_request()
        },);
        assert_eq!(size_of::<native::nl_event_t>(), unsafe {
            native::nl_sizeof_event()
        },);
        assert_eq!(size_of::<native::nl_stats_t>(), unsafe {
            native::nl_sizeof_stats()
        },);
    }

    #[test]
    fn stereo_audio_configuration_matches_moonlight_layout() {
        assert_eq!(
            audio_configuration_native(AudioConfiguration::Stereo),
            0x000302CA
        );
    }

    #[test]
    fn generic_start_error_does_not_replace_failed_stage() {
        let (state_tx, _state_rx) =
            watch::channel(crate::moonlight::domain::SessionState::Connecting);
        let (latest_event_tx, _latest_event_rx) =
            watch::channel::<Option<RuntimeEventMessage>>(None);
        let (event_tx, _) = broadcast::channel(4);
        let mut state = crate::moonlight::domain::SessionState::Connecting;

        process_native_event(
            &mut state,
            &state_tx,
            &latest_event_tx,
            &event_tx,
            NativeEvent {
                kind: native::nl_event_kind_NL_EVENT_STAGE_FAILED,
                code: -1,
                session_generation: 1,
                message: "RTSP handshake failed (-1)".to_string(),
            },
        );
        process_native_event(
            &mut state,
            &state_tx,
            &latest_event_tx,
            &event_tx,
            NativeEvent {
                kind: native::nl_event_kind_NL_EVENT_ERROR,
                code: -1,
                session_generation: 1,
                message: "LiStartConnection returned -1".to_string(),
            },
        );
        process_native_event(
            &mut state,
            &state_tx,
            &latest_event_tx,
            &event_tx,
            NativeEvent {
                kind: native::nl_event_kind_NL_EVENT_STOPPED,
                code: -1,
                session_generation: 1,
                message: "stopped (-1)".to_string(),
            },
        );

        let latest = latest_event_tx.borrow().clone().unwrap();
        assert_eq!(latest.kind, "stageFailed");
        assert_eq!(latest.message, "RTSP handshake failed (-1)");
    }

    fn sample_start_request() -> NativeStartRequest {
        NativeStartRequest {
            host_id: "host".to_string(),
            app_id: 1,
            host_address: "10.77.0.1".to_string(),
            app_version: "1".to_string(),
            gfe_version: None,
            session_url: Some("rtsp://example/session".to_string()),
            server_codec_mode_support: 1,
            preferences: StreamPreferences::default(),
            supported_video_formats: 1,
            remote_input_key: [0; 16],
            remote_input_iv: [0; 16],
            session_generation: 1,
        }
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "no-land-runtime-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn applying_packet_size_uses_remote_field_only_for_force_remote() {
        let mut request = sample_start_request();
        request.preferences.latency.remote_packet_size = 1392;
        request.preferences.network.packet_size = 1280;

        apply_packet_size(&mut request, RemoteStreamMode::ForceRemote, 1152);
        assert_eq!(request.preferences.latency.remote_packet_size, 1152);
        assert_eq!(request.preferences.network.packet_size, 1280);

        apply_packet_size(&mut request, RemoteStreamMode::ForceLocal, 1088);
        assert_eq!(request.preferences.latency.remote_packet_size, 1152);
        assert_eq!(request.preferences.network.packet_size, 1088);

        apply_packet_size(&mut request, RemoteStreamMode::Auto, 1024);
        assert_eq!(request.preferences.latency.remote_packet_size, 1152);
        assert_eq!(request.preferences.network.packet_size, 1024);
    }

    #[test]
    fn controller_selected_initial_remote_packet_is_applied_to_remote_field() {
        let root = temp_test_dir("initial-remote");
        let mut request = sample_start_request();
        request.host_address = "203.0.113.1".to_string();
        request.preferences.latency.remote_stream_mode = RemoteStreamMode::ForceRemote;
        request.preferences.latency.remote_packet_size = 1392;
        request.preferences.network.packet_size = 1280;
        request.preferences.latency.adaptive_packet_size_enabled = true;

        let controller = prepare_packet_size_controller(&root, &mut request);
        assert!(controller.snapshot().enabled);
        assert_eq!(
            request.preferences.latency.remote_packet_size,
            controller.selected_packet_size()
        );
        assert_eq!(request.preferences.network.packet_size, 1280);
        assert_eq!(
            request.preferences.latency.remote_stream_mode,
            controller.resolved_remote_mode()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_controller_preserves_configured_mode_and_packet_fields() {
        let root = temp_test_dir("disabled");
        let mut request = sample_start_request();
        request.host_address = "203.0.113.1".to_string();
        request.preferences.latency.remote_stream_mode = RemoteStreamMode::Auto;
        request.preferences.latency.remote_packet_size = 1152;
        request.preferences.network.streaming_mode = StreamingMode::Remote;
        request.preferences.network.packet_size = 1280;
        request.preferences.latency.adaptive_packet_size_enabled = false;

        let controller = prepare_packet_size_controller(&root, &mut request);
        assert!(!controller.snapshot().enabled);
        assert_eq!(
            request.preferences.latency.remote_stream_mode,
            RemoteStreamMode::Auto
        );
        assert_eq!(request.preferences.latency.remote_packet_size, 1152);
        assert_eq!(request.preferences.network.packet_size, 1280);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_start_does_not_advance_bookkeeping() {
        let state = SessionState::Streaming;
        let generation = 41;
        assert!(next_external_generation(&state, generation).is_err());
        assert_eq!(state, SessionState::Streaming);
        assert_eq!(generation, 41);
    }

    #[test]
    fn stale_generation_stats_suppress_packet_policy() {
        assert!(!should_evaluate_packet_size_policy(
            &SessionState::Streaming,
            true,
            8,
            7,
            true,
            true,
            false,
            None,
            false,
        ));
        assert!(should_evaluate_packet_size_policy(
            &SessionState::Streaming,
            true,
            8,
            8,
            true,
            true,
            false,
            None,
            false,
        ));
        assert!(!should_evaluate_packet_size_policy(
            &SessionState::Streaming,
            true,
            8,
            8,
            true,
            true,
            true,
            Some(ReconnectCause::UnexpectedFailure),
            false,
        ));
    }

    #[test]
    fn client_refresh_rate_is_independent_from_stream_fps() {
        let mut preferences = StreamPreferences::default();
        assert_eq!(client_refresh_rate_x100(&preferences), 6000);
        preferences.video.client_refresh_rate_x100 = 12_000;
        assert_eq!(client_refresh_rate_x100(&preferences), 12_000);
        preferences.video.fps = 120;
        preferences.video.client_refresh_rate_x100 = 24_000;
        assert_eq!(client_refresh_rate_x100(&preferences), 24_000);
    }

    #[test]
    fn remote_stream_resolution_is_tunnel_safe() {
        let preferences = StreamPreferences::default();
        let resolved = resolve_remote_stream_config(&preferences);
        assert_eq!(resolved.mode, RemoteStreamMode::ForceRemote);
        assert_eq!(resolved.streaming_remotely, 1);
        assert_eq!(resolved.packet_size, 1024);
    }

    #[test]
    fn remote_stream_resolution_preserves_auto_and_local() {
        let mut preferences = StreamPreferences::default();
        preferences.network.streaming_mode = StreamingMode::Auto;
        preferences.network.packet_size = 1392;
        let resolved = resolve_remote_stream_config(&preferences);
        assert_eq!(resolved.mode, RemoteStreamMode::Auto);
        assert_eq!(resolved.streaming_remotely, 2);
        assert_eq!(resolved.packet_size, 1392);

        preferences.latency.remote_stream_mode = RemoteStreamMode::ForceLocal;
        let resolved = resolve_remote_stream_config(&preferences);
        assert_eq!(resolved.mode, RemoteStreamMode::ForceLocal);
        assert_eq!(resolved.streaming_remotely, 0);
        assert_eq!(resolved.packet_size, 1392);
    }

    #[test]
    fn unexpected_reconnect_budget_resets_only_after_stability() {
        let start = Instant::now();
        assert!(!should_reset_unexpected_reconnect_budget(
            &SessionState::Streaming,
            true,
            Some(start),
            start + Duration::from_secs(29),
        ));
        assert!(should_reset_unexpected_reconnect_budget(
            &SessionState::Streaming,
            true,
            Some(start),
            start + Duration::from_secs(30),
        ));
        assert!(!should_reset_unexpected_reconnect_budget(
            &SessionState::Reconnecting,
            true,
            Some(start),
            start + Duration::from_secs(60),
        ));
    }

    #[test]
    fn reconnect_requires_unexpected_current_stream_failure() {
        let request = sample_start_request();
        let unexpected = NativeEvent {
            kind: native::nl_event_kind_NL_EVENT_TERMINATED,
            code: -1,
            session_generation: 1,
            message: "lost".to_string(),
        };
        assert!(should_auto_reconnect(
            &SessionState::Streaming,
            &unexpected,
            true,
            Some(&request),
            false,
        ));
        assert!(!should_auto_reconnect(
            &SessionState::Streaming,
            &unexpected,
            false,
            Some(&request),
            false,
        ));
        assert!(!should_auto_reconnect(
            &SessionState::Streaming,
            &unexpected,
            true,
            Some(&request),
            true,
        ));

        let graceful = NativeEvent {
            code: 0,
            ..unexpected
        };
        assert!(!should_auto_reconnect(
            &SessionState::Streaming,
            &graceful,
            true,
            Some(&request),
            false,
        ));
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
