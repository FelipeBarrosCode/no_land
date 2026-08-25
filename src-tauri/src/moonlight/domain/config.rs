use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{host::PersistedHost, identity::PersistedIdentity, MoonlightError};

// ServerCodecModeSupport values from moonlight-common-c/Limelight.h.
pub const SCM_H264: u32 = 0x0000_0001;
pub const SCM_HEVC: u32 = 0x0000_0100;
pub const SCM_HEVC_MAIN10: u32 = 0x0000_0200;
pub const SCM_AV1_MAIN8: u32 = 0x0001_0000;
pub const SCM_AV1_MAIN10: u32 = 0x0002_0000;
pub const SCM_H264_HIGH8_444: u32 = 0x0004_0000;
pub const SCM_HEVC_REXT8_444: u32 = 0x0008_0000;
pub const SCM_HEVC_REXT10_444: u32 = 0x0010_0000;
pub const SCM_AV1_HIGH8_444: u32 = 0x0020_0000;
pub const SCM_AV1_HIGH10_444: u32 = 0x0040_0000;

// StreamConfiguration.supportedVideoFormats values from moonlight-common-c/Limelight.h.
pub const VIDEO_FORMAT_H264: u32 = 0x0001;
pub const VIDEO_FORMAT_H264_HIGH8_444: u32 = 0x0004;
pub const VIDEO_FORMAT_H265: u32 = 0x0100;
pub const VIDEO_FORMAT_H265_MAIN10: u32 = 0x0200;
pub const VIDEO_FORMAT_H265_REXT8_444: u32 = 0x0400;
pub const VIDEO_FORMAT_H265_REXT10_444: u32 = 0x0800;
pub const VIDEO_FORMAT_AV1_MAIN8: u32 = 0x1000;
pub const VIDEO_FORMAT_AV1_MAIN10: u32 = 0x2000;
pub const VIDEO_FORMAT_AV1_HIGH8_444: u32 = 0x4000;
pub const VIDEO_FORMAT_AV1_HIGH10_444: u32 = 0x8000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightConfiguration {
    pub schema_version: u32,
    #[serde(default)]
    pub identity: Option<PersistedIdentity>,
    pub defaults: StreamPreferences,
    #[serde(default)]
    pub hosts: BTreeMap<String, PersistedHost>,
    pub last_selected_host_id: Option<String>,
}

impl Default for MoonlightConfiguration {
    fn default() -> Self {
        Self {
            schema_version: 1,
            identity: None,
            defaults: StreamPreferences::default(),
            hosts: BTreeMap::new(),
            last_selected_host_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamPreferences {
    pub video: VideoPreferences,
    pub audio: AudioPreferences,
    pub network: NetworkPreferences,
    pub input: InputPreferences,
    pub window: WindowPreferences,
    pub reconnection: ReconnectionPreferences,
    #[serde(default)]
    pub latency: NolandLatencyConfig,
}

impl Default for StreamPreferences {
    fn default() -> Self {
        Self {
            video: VideoPreferences::default(),
            audio: AudioPreferences::default(),
            network: NetworkPreferences::default(),
            input: InputPreferences::default(),
            window: WindowPreferences::default(),
            reconnection: ReconnectionPreferences::default(),
            latency: NolandLatencyConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoPreferences {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    #[serde(default)]
    pub client_refresh_rate_x100: u32,
    pub bitrate_kbps: u32,
    pub codec_preference: Vec<Codec>,
    pub decoder_preference: DecoderPreference,
    pub hdr: bool,
    pub yuv444: bool,
    pub color_space: ColorSpace,
    pub color_range: ColorRange,
}

impl Default for VideoPreferences {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            client_refresh_rate_x100: 0,
            bitrate_kbps: 25_000,
            codec_preference: vec![Codec::Hevc, Codec::H264],
            decoder_preference: DecoderPreference::Hardware,
            hdr: false,
            yuv444: false,
            color_space: ColorSpace::Rec709,
            color_range: ColorRange::Limited,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioPreferences {
    pub configuration: AudioConfiguration,
    pub play_on_host: bool,
    pub output_device_id: Option<String>,
    pub target_buffer_ms: u32,
    pub maximum_buffer_ms: u32,
}

impl Default for AudioPreferences {
    fn default() -> Self {
        Self {
            configuration: AudioConfiguration::Stereo,
            play_on_host: false,
            output_device_id: None,
            target_buffer_ms: 20,
            maximum_buffer_ms: 80,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPreferences {
    pub packet_size: u16,
    pub streaming_mode: StreamingMode,
    pub encryption: EncryptionMode,
    pub connect_timeout_ms: u32,
}

impl Default for NetworkPreferences {
    fn default() -> Self {
        Self {
            packet_size: 1024,
            streaming_mode: StreamingMode::Remote,
            encryption: EncryptionMode::All,
            connect_timeout_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InputPreferences {
    pub mouse_mode: MouseMode,
    pub capture_mouse: bool,
    pub release_shortcut: String,
    pub persist_controllers_on_disconnect: bool,
}

impl Default for InputPreferences {
    fn default() -> Self {
        Self {
            mouse_mode: MouseMode::Relative,
            capture_mouse: true,
            release_shortcut: "Control+Alt+Shift+Q".to_string(),
            persist_controllers_on_disconnect: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowPreferences {
    pub mode: WindowMode,
    pub display_id: Option<String>,
    pub show_statistics: bool,
    pub keep_launcher_visible: bool,
}

impl Default for WindowPreferences {
    fn default() -> Self {
        Self {
            mode: WindowMode::FullscreenDesktop,
            display_id: None,
            show_statistics: false,
            keep_launcher_visible: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectionPreferences {
    pub enabled: bool,
    pub maximum_attempts: u32,
    pub initial_delay_ms: u32,
    pub maximum_delay_ms: u32,
}

impl Default for ReconnectionPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_attempts: 1,
            initial_delay_ms: 0,
            maximum_delay_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DecoderPreference {
    Hardware,
    Software,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ColorSpace {
    Rec709,
    Rec2020,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ColorRange {
    Limited,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AudioConfiguration {
    Stereo,
    Surround51,
    Surround71,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StreamingMode {
    Local,
    Remote,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PacingMode {
    Off,
    Automatic,
    Software,
    HardwareMultiple,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FrameBufferMode {
    Off,
    OneFrame,
    TwoFrames,
    ThreeFrames,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RemoteStreamMode {
    Auto,
    ForceRemote,
    ForceLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NolandLatencyConfig {
    pub telemetry_enabled: bool,
    pub adaptive_late_frame_drop_enabled: bool,
    #[serde(default)]
    pub adaptive_packet_size_enabled: bool,
    pub decoder_backpressure_policy_enabled: bool,
    pub pacing_mode: PacingMode,
    pub frame_buffer_mode: FrameBufferMode,
    pub auto_reconnect_on_unexpected_termination: bool,
    pub remote_stream_mode: RemoteStreamMode,
    pub remote_packet_size: u16,
    pub late_frame_tolerance_us: u32,
    pub vsync_enabled: bool,
}

impl Default for NolandLatencyConfig {
    fn default() -> Self {
        Self {
            telemetry_enabled: cfg!(debug_assertions),
            adaptive_late_frame_drop_enabled: false,
            adaptive_packet_size_enabled: false,
            decoder_backpressure_policy_enabled: false,
            pacing_mode: PacingMode::Off,
            frame_buffer_mode: FrameBufferMode::Off,
            auto_reconnect_on_unexpected_termination: true,
            remote_stream_mode: RemoteStreamMode::Auto,
            remote_packet_size: 1024,
            late_frame_tolerance_us: 0,
            vsync_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionMode {
    None,
    Control,
    All,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseMode {
    Relative,
    Absolute,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WindowMode {
    FullscreenDesktop,
    FullscreenWindow,
    Windowed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StreamPreferencesPatch {
    pub video: Option<VideoPreferencesPatch>,
    pub audio: Option<AudioPreferencesPatch>,
    pub network: Option<NetworkPreferencesPatch>,
    pub input: Option<InputPreferencesPatch>,
    pub window: Option<WindowPreferencesPatch>,
    pub reconnection: Option<ReconnectionPreferencesPatch>,
    pub latency: Option<NolandLatencyConfigPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct VideoPreferencesPatch {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub client_refresh_rate_x100: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub codec_preference: Option<Vec<Codec>>,
    pub decoder_preference: Option<DecoderPreference>,
    pub hdr: Option<bool>,
    pub yuv444: Option<bool>,
    pub color_space: Option<ColorSpace>,
    pub color_range: Option<ColorRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioPreferencesPatch {
    pub configuration: Option<AudioConfiguration>,
    pub play_on_host: Option<bool>,
    pub output_device_id: Option<Option<String>>,
    pub target_buffer_ms: Option<u32>,
    pub maximum_buffer_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPreferencesPatch {
    pub packet_size: Option<u16>,
    pub streaming_mode: Option<StreamingMode>,
    pub encryption: Option<EncryptionMode>,
    pub connect_timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InputPreferencesPatch {
    pub mouse_mode: Option<MouseMode>,
    pub capture_mouse: Option<bool>,
    pub release_shortcut: Option<String>,
    pub persist_controllers_on_disconnect: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowPreferencesPatch {
    pub mode: Option<WindowMode>,
    pub display_id: Option<Option<String>>,
    pub show_statistics: Option<bool>,
    pub keep_launcher_visible: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectionPreferencesPatch {
    pub enabled: Option<bool>,
    pub maximum_attempts: Option<u32>,
    pub initial_delay_ms: Option<u32>,
    pub maximum_delay_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct NolandLatencyConfigPatch {
    pub telemetry_enabled: Option<bool>,
    pub adaptive_late_frame_drop_enabled: Option<bool>,
    pub adaptive_packet_size_enabled: Option<bool>,
    pub decoder_backpressure_policy_enabled: Option<bool>,
    pub pacing_mode: Option<PacingMode>,
    pub frame_buffer_mode: Option<FrameBufferMode>,
    pub auto_reconnect_on_unexpected_termination: Option<bool>,
    pub remote_stream_mode: Option<RemoteStreamMode>,
    pub remote_packet_size: Option<u16>,
    pub late_frame_tolerance_us: Option<u32>,
    pub vsync_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientVideoCapabilities {
    pub supports_h264: bool,
    pub supports_hevc: bool,
    pub supports_av1: bool,
    pub supports_hdr10: bool,
    pub supports_yuv444: bool,
    pub supports_10bit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedVideoFormat {
    pub codec: Codec,
    pub bit_depth: BitDepth,
    pub chroma: ChromaFormat,
    pub hdr: bool,
    pub moonlight_format_mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Depth8,
    Depth10,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaFormat {
    Yuv420,
    Yuv444,
}

pub fn merge_preferences(
    defaults: &StreamPreferences,
    host: Option<&StreamPreferencesPatch>,
    session: Option<&StreamPreferencesPatch>,
) -> StreamPreferences {
    let mut merged = defaults.clone();

    if let Some(host_patch) = host {
        apply_preferences_patch(&mut merged, host_patch);
    }

    if let Some(session_patch) = session {
        apply_preferences_patch(&mut merged, session_patch);
    }

    merged
}

fn apply_preferences_patch(target: &mut StreamPreferences, patch: &StreamPreferencesPatch) {
    if let Some(video) = &patch.video {
        apply_video_patch(&mut target.video, video);
    }
    if let Some(audio) = &patch.audio {
        apply_audio_patch(&mut target.audio, audio);
    }
    if let Some(network) = &patch.network {
        apply_network_patch(&mut target.network, network);
    }
    if let Some(input) = &patch.input {
        apply_input_patch(&mut target.input, input);
    }
    if let Some(window) = &patch.window {
        apply_window_patch(&mut target.window, window);
    }
    if let Some(reconnection) = &patch.reconnection {
        apply_reconnection_patch(&mut target.reconnection, reconnection);
    }
    if let Some(latency) = &patch.latency {
        apply_latency_patch(&mut target.latency, latency);
    }
}

fn apply_video_patch(target: &mut VideoPreferences, patch: &VideoPreferencesPatch) {
    if let Some(value) = patch.width {
        target.width = value;
    }
    if let Some(value) = patch.height {
        target.height = value;
    }
    if let Some(value) = patch.fps {
        target.fps = value;
    }
    if let Some(value) = patch.client_refresh_rate_x100 {
        target.client_refresh_rate_x100 = value;
    }
    if let Some(value) = patch.bitrate_kbps {
        target.bitrate_kbps = value;
    }
    if let Some(value) = &patch.codec_preference {
        target.codec_preference = value.clone();
    }
    if let Some(value) = patch.decoder_preference {
        target.decoder_preference = value;
    }
    if let Some(value) = patch.hdr {
        target.hdr = value;
    }
    if let Some(value) = patch.yuv444 {
        target.yuv444 = value;
    }
    if let Some(value) = patch.color_space {
        target.color_space = value;
    }
    if let Some(value) = patch.color_range {
        target.color_range = value;
    }
}

fn apply_audio_patch(target: &mut AudioPreferences, patch: &AudioPreferencesPatch) {
    if let Some(value) = patch.configuration {
        target.configuration = value;
    }
    if let Some(value) = patch.play_on_host {
        target.play_on_host = value;
    }
    if let Some(value) = &patch.output_device_id {
        target.output_device_id = value.clone();
    }
    if let Some(value) = patch.target_buffer_ms {
        target.target_buffer_ms = value;
    }
    if let Some(value) = patch.maximum_buffer_ms {
        target.maximum_buffer_ms = value;
    }
}

fn apply_network_patch(target: &mut NetworkPreferences, patch: &NetworkPreferencesPatch) {
    if let Some(value) = patch.packet_size {
        target.packet_size = value;
    }
    if let Some(value) = patch.streaming_mode {
        target.streaming_mode = value;
    }
    if let Some(value) = patch.encryption {
        target.encryption = value;
    }
    if let Some(value) = patch.connect_timeout_ms {
        target.connect_timeout_ms = value;
    }
}

fn apply_input_patch(target: &mut InputPreferences, patch: &InputPreferencesPatch) {
    if let Some(value) = patch.mouse_mode {
        target.mouse_mode = value;
    }
    if let Some(value) = patch.capture_mouse {
        target.capture_mouse = value;
    }
    if let Some(value) = &patch.release_shortcut {
        target.release_shortcut = value.clone();
    }
    if let Some(value) = patch.persist_controllers_on_disconnect {
        target.persist_controllers_on_disconnect = value;
    }
}

fn apply_window_patch(target: &mut WindowPreferences, patch: &WindowPreferencesPatch) {
    if let Some(value) = patch.mode {
        target.mode = value;
    }
    if let Some(value) = &patch.display_id {
        target.display_id = value.clone();
    }
    if let Some(value) = patch.show_statistics {
        target.show_statistics = value;
    }
    if let Some(value) = patch.keep_launcher_visible {
        target.keep_launcher_visible = value;
    }
}

fn apply_reconnection_patch(
    target: &mut ReconnectionPreferences,
    patch: &ReconnectionPreferencesPatch,
) {
    if let Some(value) = patch.enabled {
        target.enabled = value;
    }
    if let Some(value) = patch.maximum_attempts {
        target.maximum_attempts = value;
    }
    if let Some(value) = patch.initial_delay_ms {
        target.initial_delay_ms = value;
    }
    if let Some(value) = patch.maximum_delay_ms {
        target.maximum_delay_ms = value;
    }
}

fn apply_latency_patch(target: &mut NolandLatencyConfig, patch: &NolandLatencyConfigPatch) {
    if let Some(value) = patch.telemetry_enabled {
        target.telemetry_enabled = value;
    }
    if let Some(value) = patch.adaptive_late_frame_drop_enabled {
        target.adaptive_late_frame_drop_enabled = value;
    }
    if let Some(value) = patch.adaptive_packet_size_enabled {
        target.adaptive_packet_size_enabled = value;
    }
    if let Some(value) = patch.decoder_backpressure_policy_enabled {
        target.decoder_backpressure_policy_enabled = value;
    }
    if let Some(value) = patch.pacing_mode {
        target.pacing_mode = value;
    }
    if let Some(value) = patch.frame_buffer_mode {
        target.frame_buffer_mode = value;
    }
    if let Some(value) = patch.auto_reconnect_on_unexpected_termination {
        target.auto_reconnect_on_unexpected_termination = value;
    }
    if let Some(value) = patch.remote_stream_mode {
        target.remote_stream_mode = value;
    }
    if let Some(value) = patch.remote_packet_size {
        target.remote_packet_size = value;
    }
    if let Some(value) = patch.late_frame_tolerance_us {
        target.late_frame_tolerance_us = value;
    }
    if let Some(value) = patch.vsync_enabled {
        target.vsync_enabled = value;
    }
}

pub fn validate_preferences(
    preferences: &StreamPreferences,
    capabilities: Option<&ClientVideoCapabilities>,
) -> Result<(), MoonlightError> {
    let video = &preferences.video;
    let network = &preferences.network;

    if video.width < 640 {
        return Err(MoonlightError::Validation(
            "width must be at least 640".to_string(),
        ));
    }
    if video.height < 360 {
        return Err(MoonlightError::Validation(
            "height must be at least 360".to_string(),
        ));
    }
    if !matches!(video.fps, 30 | 60 | 90 | 120 | 144 | 240) {
        return Err(MoonlightError::Validation(
            "fps must be one of 30, 60, 90, 120, 144, or 240".to_string(),
        ));
    }
    if video.client_refresh_rate_x100 != 0
        && !(2_400..=100_000).contains(&video.client_refresh_rate_x100)
    {
        return Err(MoonlightError::Validation(
            "client refresh rate must be 0 or between 24.00 and 1000.00 Hz".to_string(),
        ));
    }
    if !(1_000..=150_000).contains(&video.bitrate_kbps) {
        return Err(MoonlightError::Validation(
            "bitrate must be between 1000 and 150000 Kbps".to_string(),
        ));
    }
    if !(512..=1_400).contains(&network.packet_size) {
        return Err(MoonlightError::Validation(
            "packetSize must be between 512 and 1400".to_string(),
        ));
    }
    let resolved_remote_mode = match preferences.latency.remote_stream_mode {
        RemoteStreamMode::Auto => match network.streaming_mode {
            StreamingMode::Local => RemoteStreamMode::ForceLocal,
            StreamingMode::Remote => RemoteStreamMode::ForceRemote,
            StreamingMode::Auto => RemoteStreamMode::Auto,
        },
        explicit => explicit,
    };
    if (resolved_remote_mode == RemoteStreamMode::ForceRemote
        || preferences.latency.adaptive_packet_size_enabled)
        && (!(960..=1_392).contains(&preferences.latency.remote_packet_size)
            || preferences.latency.remote_packet_size % 16 != 0)
    {
        return Err(MoonlightError::Validation(
            "remote packet size must be between 960 and 1392 and divisible by 16".to_string(),
        ));
    }
    if preferences.reconnection.enabled
        && (preferences.reconnection.maximum_attempts != 1
            || preferences.reconnection.initial_delay_ms != 0
            || preferences.reconnection.maximum_delay_ms != 0)
    {
        return Err(MoonlightError::Validation(
            "the embedded client currently supports exactly one immediate reconnect attempt"
                .to_string(),
        ));
    }
    if preferences.latency.frame_buffer_mode != FrameBufferMode::Off
        && preferences.latency.adaptive_late_frame_drop_enabled
    {
        return Err(MoonlightError::Validation(
            "smoothing reserve and adaptive late-frame dropping are mutually exclusive".to_string(),
        ));
    }
    if !preferences.latency.vsync_enabled && preferences.latency.pacing_mode != PacingMode::Off {
        return Err(MoonlightError::Validation(
            "frame pacing must be off when V-Sync is disabled".to_string(),
        ));
    }

    if let Some(capabilities) = capabilities {
        if video.hdr && !(capabilities.supports_hdr10 && capabilities.supports_10bit) {
            return Err(MoonlightError::Validation(
                "HDR requires a 10-bit-capable decoder and display".to_string(),
            ));
        }
        // Codec and 4:4:4 preferences are negotiated opportunistically. Unsupported
        // entries fall through to the next codec/profile instead of rejecting the request.
    }

    Ok(())
}

pub fn negotiate_video_format(
    preferences: &StreamPreferences,
    host_support_mask: u32,
    capabilities: &ClientVideoCapabilities,
) -> Result<NegotiatedVideoFormat, MoonlightError> {
    validate_preferences(preferences, Some(capabilities))?;

    let hdr = preferences.video.hdr;
    let wants_yuv444 = preferences.video.yuv444 && capabilities.supports_yuv444;

    for codec in &preferences.video.codec_preference {
        let selected = match codec {
            Codec::H264 if capabilities.supports_h264 && !hdr => {
                if wants_yuv444 && host_support_mask & SCM_H264_HIGH8_444 != 0 {
                    Some((
                        VIDEO_FORMAT_H264_HIGH8_444,
                        BitDepth::Depth8,
                        ChromaFormat::Yuv444,
                    ))
                } else if host_support_mask & SCM_H264 != 0 {
                    Some((VIDEO_FORMAT_H264, BitDepth::Depth8, ChromaFormat::Yuv420))
                } else {
                    None
                }
            }
            Codec::Hevc if capabilities.supports_hevc => {
                if hdr && capabilities.supports_10bit {
                    if wants_yuv444 && host_support_mask & SCM_HEVC_REXT10_444 != 0 {
                        Some((
                            VIDEO_FORMAT_H265_REXT10_444,
                            BitDepth::Depth10,
                            ChromaFormat::Yuv444,
                        ))
                    } else if host_support_mask & SCM_HEVC_MAIN10 != 0 {
                        Some((
                            VIDEO_FORMAT_H265_MAIN10,
                            BitDepth::Depth10,
                            ChromaFormat::Yuv420,
                        ))
                    } else {
                        None
                    }
                } else if !hdr {
                    if wants_yuv444 && host_support_mask & SCM_HEVC_REXT8_444 != 0 {
                        Some((
                            VIDEO_FORMAT_H265_REXT8_444,
                            BitDepth::Depth8,
                            ChromaFormat::Yuv444,
                        ))
                    } else if host_support_mask & SCM_HEVC != 0 {
                        Some((VIDEO_FORMAT_H265, BitDepth::Depth8, ChromaFormat::Yuv420))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Codec::Av1 if capabilities.supports_av1 => {
                if hdr && capabilities.supports_10bit {
                    if wants_yuv444 && host_support_mask & SCM_AV1_HIGH10_444 != 0 {
                        Some((
                            VIDEO_FORMAT_AV1_HIGH10_444,
                            BitDepth::Depth10,
                            ChromaFormat::Yuv444,
                        ))
                    } else if host_support_mask & SCM_AV1_MAIN10 != 0 {
                        Some((
                            VIDEO_FORMAT_AV1_MAIN10,
                            BitDepth::Depth10,
                            ChromaFormat::Yuv420,
                        ))
                    } else {
                        None
                    }
                } else if !hdr {
                    if wants_yuv444 && host_support_mask & SCM_AV1_HIGH8_444 != 0 {
                        Some((
                            VIDEO_FORMAT_AV1_HIGH8_444,
                            BitDepth::Depth8,
                            ChromaFormat::Yuv444,
                        ))
                    } else if host_support_mask & SCM_AV1_MAIN8 != 0 {
                        Some((
                            VIDEO_FORMAT_AV1_MAIN8,
                            BitDepth::Depth8,
                            ChromaFormat::Yuv420,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some((moonlight_format_mask, bit_depth, chroma)) = selected {
            return Ok(NegotiatedVideoFormat {
                codec: *codec,
                bit_depth,
                chroma,
                hdr,
                moonlight_format_mask,
            });
        }
    }

    Err(MoonlightError::Validation(
        "no compatible codec profile remains after negotiation".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_caps() -> ClientVideoCapabilities {
        ClientVideoCapabilities {
            supports_h264: true,
            supports_hevc: true,
            supports_av1: false,
            supports_hdr10: false,
            supports_yuv444: false,
            supports_10bit: false,
        }
    }

    #[test]
    fn merges_host_then_session_preferences() {
        let defaults = StreamPreferences::default();
        let host = StreamPreferencesPatch {
            video: Some(VideoPreferencesPatch {
                bitrate_kbps: Some(30_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let session = StreamPreferencesPatch {
            video: Some(VideoPreferencesPatch {
                fps: Some(120),
                bitrate_kbps: Some(40_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merge_preferences(&defaults, Some(&host), Some(&session));
        assert_eq!(merged.video.fps, 120);
        assert_eq!(merged.video.bitrate_kbps, 40_000);
    }

    #[test]
    fn rejects_invalid_bitrate() {
        let mut prefs = StreamPreferences::default();
        prefs.video.bitrate_kbps = 999;
        assert!(validate_preferences(&prefs, None).is_err());
    }

    #[test]
    fn rejects_invalid_packet_size() {
        let mut prefs = StreamPreferences::default();
        prefs.network.packet_size = 1500;
        assert!(validate_preferences(&prefs, None).is_err());
    }

    #[test]
    fn adaptive_packet_size_is_disabled_by_default() {
        assert!(!NolandLatencyConfig::default().adaptive_packet_size_enabled);
    }

    #[test]
    fn latency_patch_updates_adaptive_packet_size() {
        let defaults = StreamPreferences::default();
        let patch = StreamPreferencesPatch {
            latency: Some(NolandLatencyConfigPatch {
                adaptive_packet_size_enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merge_preferences(&defaults, Some(&patch), None);
        assert!(merged.latency.adaptive_packet_size_enabled);
    }

    #[test]
    fn explicit_local_mode_overrides_legacy_remote_classification() {
        let mut prefs = StreamPreferences::default();
        prefs.latency.remote_stream_mode = RemoteStreamMode::ForceLocal;
        prefs.latency.remote_packet_size = 959;
        assert!(validate_preferences(&prefs, None).is_ok());
    }

    #[test]
    fn forced_remote_accepts_safe_packet_sizes() {
        for packet_size in [960, 1024, 1152, 1280, 1392] {
            let mut prefs = StreamPreferences::default();
            prefs.latency.remote_stream_mode = RemoteStreamMode::ForceRemote;
            prefs.latency.remote_packet_size = packet_size;
            assert!(
                validate_preferences(&prefs, None).is_ok(),
                "expected packet size {packet_size} to be valid"
            );
        }
    }

    #[test]
    fn forced_remote_rejects_unsafe_packet_sizes() {
        for packet_size in [959, 1400, 1185] {
            let mut prefs = StreamPreferences::default();
            prefs.latency.remote_stream_mode = RemoteStreamMode::ForceRemote;
            prefs.latency.remote_packet_size = packet_size;
            assert!(
                validate_preferences(&prefs, None).is_err(),
                "expected packet size {packet_size} to be invalid"
            );
        }
    }

    #[test]
    fn adaptive_packet_size_requires_safe_packet_size() {
        let mut prefs = StreamPreferences::default();
        prefs.latency.remote_stream_mode = RemoteStreamMode::ForceLocal;
        prefs.latency.adaptive_packet_size_enabled = true;
        prefs.latency.remote_packet_size = 1185;
        assert!(validate_preferences(&prefs, None).is_err());

        prefs.latency.remote_packet_size = 1152;
        assert!(validate_preferences(&prefs, None).is_ok());
    }

    #[test]
    fn rejects_unsupported_reconnect_retry_or_delay_settings() {
        let mut prefs = StreamPreferences::default();
        prefs.reconnection.maximum_attempts = 2;
        assert!(validate_preferences(&prefs, None).is_err());

        prefs.reconnection.maximum_attempts = 1;
        prefs.reconnection.initial_delay_ms = 100;
        assert!(validate_preferences(&prefs, None).is_err());

        prefs.reconnection.enabled = false;
        assert!(validate_preferences(&prefs, None).is_ok());
    }

    #[test]
    fn unavailable_av1_preference_falls_back_to_h264() {
        let mut prefs = StreamPreferences::default();
        prefs.video.codec_preference = vec![Codec::Av1, Codec::H264];
        let negotiated = negotiate_video_format(&prefs, SCM_H264, &local_caps()).unwrap();
        assert_eq!(negotiated.codec, Codec::H264);
        assert_eq!(negotiated.moonlight_format_mask, VIDEO_FORMAT_H264);
    }

    #[test]
    fn rejects_hdr_without_ten_bit_decoder() {
        let mut prefs = StreamPreferences::default();
        prefs.video.hdr = true;
        assert!(validate_preferences(&prefs, Some(&local_caps())).is_err());
    }

    #[test]
    fn falls_back_to_h264_when_hevc_missing_on_host() {
        let prefs = StreamPreferences::default();
        let negotiated = negotiate_video_format(
            &prefs,
            SCM_H264,
            &ClientVideoCapabilities {
                supports_h264: true,
                supports_hevc: true,
                supports_av1: false,
                supports_hdr10: false,
                supports_yuv444: false,
                supports_10bit: false,
            },
        )
        .unwrap();
        assert_eq!(negotiated.codec, Codec::H264);
    }

    #[test]
    fn removes_yuv444_when_not_supported() {
        let mut prefs = StreamPreferences::default();
        prefs.video.yuv444 = true;
        prefs.video.codec_preference = vec![Codec::Hevc, Codec::H264];
        let negotiated = negotiate_video_format(
            &prefs,
            SCM_HEVC | SCM_H264,
            &ClientVideoCapabilities {
                supports_h264: true,
                supports_hevc: true,
                supports_av1: false,
                supports_hdr10: false,
                supports_yuv444: false,
                supports_10bit: false,
            },
        )
        .unwrap();
        assert_eq!(negotiated.chroma, ChromaFormat::Yuv420);
        assert_eq!(negotiated.moonlight_format_mask, VIDEO_FORMAT_H265);
    }

    #[test]
    fn sunshine_codec_mask_maps_to_exact_hevc_format() {
        let prefs = StreamPreferences::default();
        let negotiated = negotiate_video_format(&prefs, 0x0003_0501, &local_caps()).unwrap();
        assert_eq!(negotiated.codec, Codec::Hevc);
        assert_eq!(negotiated.bit_depth, BitDepth::Depth8);
        assert_eq!(negotiated.chroma, ChromaFormat::Yuv420);
        assert_eq!(negotiated.moonlight_format_mask, VIDEO_FORMAT_H265);
    }

    #[test]
    fn h264_is_not_selected_for_hdr() {
        let mut prefs = StreamPreferences::default();
        prefs.video.hdr = true;
        prefs.video.codec_preference = vec![Codec::H264];
        let caps = ClientVideoCapabilities {
            supports_h264: true,
            supports_hevc: false,
            supports_av1: false,
            supports_hdr10: true,
            supports_yuv444: false,
            supports_10bit: true,
        };
        assert!(negotiate_video_format(&prefs, SCM_H264, &caps).is_err());
    }
}
