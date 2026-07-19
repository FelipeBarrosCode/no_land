use std::time::Duration;

use crate::moonlight::{
    domain::{
        build_launch_parameters, negotiate_video_format, select_active_address,
        validate_preferences, ClientVideoCapabilities, LaunchDecisionInput, LaunchResult,
        MoonlightError, RemoteInputCrypto, StreamPreferences, StreamPreferencesPatch,
    },
    infrastructure::{
        gamestream::{
            build_cancel_request, build_launch_or_resume_request, parse_launch_response,
            GameStreamHttpClient,
        },
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
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

    let merged = crate::moonlight::domain::merge_preferences(
        &configuration.defaults,
        host.preferences_override.as_ref(),
        session_preferences,
    );
    validate_preferences(&merged, None)?;

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

    let address = select_active_address(&host, None)?.address;
    let response = client
        .execute(build_launch_or_resume_request(
            address.clone(),
            host.ports.http,
            operation,
            &params,
            Duration::from_secs(15),
        ))
        .await?;

    let launch_result = parse_launch_response(&response.body, operation)?;
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
        supports_hevc: true,
        supports_av1: false,
        supports_hdr10: false,
        supports_yuv444: false,
        supports_10bit: false,
    }
}

pub async fn quit_remote_app(
    repository: &JsonMoonlightStateRepository,
    client: &impl GameStreamHttpClient,
    host_id: &str,
) -> Result<(), MoonlightError> {
    let host = repository.get_host(host_id)?;
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
            host.ports.http,
            Duration::from_secs(10),
        ))
        .await?;
    Ok(())
}
