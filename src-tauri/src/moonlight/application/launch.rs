use std::time::Duration;

use reqwest::Url;

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
        play_local_audio = !merged.audio.play_on_host,
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
            Duration::from_secs(15),
        ))
        .await?;

    let mut launch_result = parse_launch_response(&response.body, operation)?;
    launch_result.rtsp_session_url =
        normalize_rtsp_session_url(launch_result.rtsp_session_url, &address);
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

fn normalize_rtsp_session_url(session_url: Option<String>, host_address: &str) -> Option<String> {
    let Some(session_url) = session_url else {
        return None;
    };
    if host_address.trim().is_empty() {
        return Some(session_url);
    }

    let mut parsed = match Url::parse(&session_url) {
        Ok(parsed) => parsed,
        Err(_) => return Some(session_url),
    };
    let current_host = parsed.host_str().map(str::to_string);
    if current_host.as_deref() == Some(host_address) {
        return Some(session_url);
    }

    match parsed.set_host(Some(host_address)) {
        Ok(()) => {
            let normalized = parsed.to_string();
            tracing::info!(
                original_session_url = %session_url,
                normalized_session_url = %normalized,
                host_address,
                "normalized RTSP session URL host to selected active address"
            );
            Some(normalized)
        }
        Err(_) => Some(session_url),
    }
}

fn default_client_video_capabilities() -> ClientVideoCapabilities {
    ClientVideoCapabilities {
        supports_h264: true,
        supports_hevc: true,
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

#[cfg(test)]
mod tests {
    use super::normalize_rtsp_session_url;

    #[test]
    fn normalizes_rtsp_session_url_host_to_selected_address() {
        let normalized =
            normalize_rtsp_session_url(Some("rtsp://ubuntu:48010".to_string()), "100.117.88.18");
        assert_eq!(normalized.as_deref(), Some("rtsp://100.117.88.18:48010"));
    }

    #[test]
    fn preserves_rtsp_session_url_when_host_already_matches() {
        let normalized = normalize_rtsp_session_url(
            Some("rtsp://100.117.88.18:48010".to_string()),
            "100.117.88.18",
        );
        assert_eq!(normalized.as_deref(), Some("rtsp://100.117.88.18:48010"));
    }
}
