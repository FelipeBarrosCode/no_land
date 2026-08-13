use std::time::Duration;

use crate::moonlight::{
    application::bootstrap::bootstrap_client_identity,
    domain::{
        build_launch_parameters, negotiate_video_format, select_active_address,
        validate_preferences, ClientVideoCapabilities, LaunchDecisionInput, LaunchResult,
        MoonlightError, MouseMode, RemoteInputCrypto, StreamPreferences, StreamPreferencesPatch,
    },
    infrastructure::{
        gamestream::{
            build_cancel_request, build_launch_or_resume_request, parse_launch_response,
            GameStreamHttpClient,
        },
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
        secrets::SecretStore,
    },
};

#[derive(Debug, Clone)]
pub struct PreparedStreamStart {
    pub launch_result: LaunchResult,
    pub preferences: StreamPreferences,
    pub host_address: String,
    pub app_version: String,
    pub gfe_version: Option<String>,
    pub server_codec_mode_support: u32,
    pub supported_video_formats: u32,
    pub remote_input_key: [u8; 16],
    pub remote_input_iv: [u8; 16],
}

pub async fn start_stream_request(
    repository: &JsonMoonlightStateRepository,
    secret_store: &dyn SecretStore,
    client: &impl GameStreamHttpClient,
    host_id: &str,
    app_id: u32,
    session_preferences: Option<&StreamPreferencesPatch>,
    replace_existing: bool,
) -> Result<PreparedStreamStart, MoonlightError> {
    let configuration = repository.snapshot()?;
    let host = configuration
        .hosts
        .get(host_id)
        .cloned()
        .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} not found")))?;
    let identity = bootstrap_client_identity(repository, secret_store)
        .await?
        .identity
        .persisted();

    let mut merged = crate::moonlight::domain::merge_preferences(
        &configuration.defaults,
        host.preferences_override.as_ref(),
        session_preferences,
    );
    if matches!(
        merged.network.encryption,
        crate::moonlight::domain::EncryptionMode::All
    ) {
        tracing::warn!(
            host_id,
            "downgrading embedded stream encryption from All to Control to avoid silent/corrupted native audio on current client"
        );
        merged.network.encryption = crate::moonlight::domain::EncryptionMode::Control;
    }
    if app_id == 0
        && session_preferences
            .and_then(|patch| patch.input.as_ref())
            .and_then(|input| input.mouse_mode)
            .is_none()
        && host
            .preferences_override
            .as_ref()
            .and_then(|patch| patch.input.as_ref())
            .and_then(|input| input.mouse_mode)
            .is_none()
    {
        merged.input.mouse_mode = MouseMode::Absolute;
    }
    validate_preferences(&merged, None)?;

    let pairing = host
        .pairing
        .clone()
        .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} is not paired")))?;

    let server_info = host.server_info_cache.clone().ok_or_else(|| {
        MoonlightError::Validation(format!(
            "host {host_id} has no cached server information; refresh the host before streaming"
        ))
    })?;

    let negotiated = negotiate_video_format(
        &merged,
        server_info.server_codec_mode_support,
        &default_client_video_capabilities(),
    )?;

    let crypto = RemoteInputCrypto::generate();
    let current_game_id = Some(server_info.current_game_id).filter(|value| *value != 0);
    let operation = crate::moonlight::domain::select_launch_operation(LaunchDecisionInput {
        current_game_id,
        requested_app_id: app_id,
        replace_existing,
    })?;
    let params = build_launch_parameters(app_id, operation, &merged, &crypto);

    tracing::info!(
        host_id,
        app_id,
        operation = ?operation,
        host_address = %select_active_address(&host, None)?.address,
        resolution = %format!("{}x{}x{}", merged.video.width, merged.video.height, merged.video.fps),
        bitrate_kbps = merged.video.bitrate_kbps,
        audio_configuration = ?merged.audio.configuration,
        play_audio_on_host = merged.audio.play_on_host,
        packet_size = merged.network.packet_size,
        streaming_mode = ?merged.network.streaming_mode,
        hdr = merged.video.hdr,
        "prepared embedded moonlight launch request"
    );

    let address = select_active_address(&host, None)?.address;
    let response = client
        .execute(build_launch_or_resume_request(
            address.clone(),
            host.ports.https.unwrap_or(host.ports.http),
            &identity,
            &pairing,
            operation,
            &params,
            match operation {
                crate::moonlight::domain::LaunchOperation::Launch => Duration::from_secs(120),
                crate::moonlight::domain::LaunchOperation::Resume => Duration::from_secs(30),
            },
        ))
        .await?;

    let launch_result = parse_launch_response(&response.body, operation)?;
    tracing::info!(
        host_id,
        operation = ?operation,
        session_url = ?launch_result.rtsp_session_url,
        "received Moonlight launch session URL"
    );
    repository.update(|configuration| {
        configuration.last_selected_host_id = Some(host_id.to_string());
        if let Some(host) = configuration.hosts.get_mut(host_id) {
            host.last_selected_app_id = Some(app_id);
        }
        Ok(())
    })?;

    Ok(PreparedStreamStart {
        launch_result,
        preferences: merged,
        host_address: address,
        app_version: server_info.app_version,
        gfe_version: server_info.gfe_version,
        server_codec_mode_support: server_info.server_codec_mode_support,
        supported_video_formats: negotiated.moonlight_format_mask,
        remote_input_key: crypto.key,
        remote_input_iv: crypto.iv,
    })
}

fn default_client_video_capabilities() -> ClientVideoCapabilities {
    ClientVideoCapabilities {
        supports_h264: true,
        supports_hevc: !cfg!(target_os = "windows"),
        supports_av1: false,
        supports_hdr10: false,
        supports_yuv444: false,
        supports_10bit: false,
    }
}

pub async fn quit_remote_app(
    repository: &JsonMoonlightStateRepository,
    secret_store: &dyn SecretStore,
    client: &impl GameStreamHttpClient,
    host_id: &str,
) -> Result<(), MoonlightError> {
    let configuration = repository.snapshot()?;
    let host = configuration
        .hosts
        .get(host_id)
        .cloned()
        .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} not found")))?;
    let identity = bootstrap_client_identity(repository, secret_store)
        .await?
        .identity
        .persisted();
    let pairing = host
        .pairing
        .clone()
        .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} is not paired")))?;
    let address = host
        .addresses
        .overlay
        .or(host.addresses.lan)
        .or(host.addresses.external)
        .ok_or_else(|| {
            MoonlightError::Validation(format!("host {host_id} has no usable address"))
        })?;
    client
        .execute(build_cancel_request(
            address,
            host.ports.https.unwrap_or(host.ports.http),
            &identity,
            &pairing,
            Duration::from_secs(10),
        ))
        .await?;
    Ok(())
}
