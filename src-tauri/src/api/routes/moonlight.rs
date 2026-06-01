use std::{collections::BTreeMap, sync::Arc};

use axum::{extract::State, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    models::app_state::PostWireGuardSetupState,
    services::{
        moonlight::{
            MoonlightCodecPreference, MoonlightConfigureOptions, MoonlightConfigureResult,
            MoonlightNetworkPreference, MoonlightService,
        },
        os_detection::OsDetection,
        post_wireguard_setup::{
            detect_moonlight_client, setup_moonlight_sunshine, submit_moonlight_pin_to_sunshine,
            verify_sunshine_api, MoonlightDetectionResult, SunshineVerificationResult,
        },
    },
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinRequest {
    pub pin: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureMoonlightRequest {
    pub apply: bool,
    pub force_close: bool,
    pub native: bool,
    pub network: Option<String>,
    pub prefer_codec: Option<String>,
    pub max_bitrate: Option<u32>,
    pub fps: Option<u32>,
    pub resolution: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreBackupRequest {
    pub backup_file: String,
}

pub async fn verify_sunshine(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<SunshineVerificationResult>, ApiError> {
    let result = verify_sunshine_api(&state.app, &state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn detect_moonlight(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<MoonlightDetectionResult>, ApiError> {
    let result = detect_moonlight_client(&state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn setup_moonlight_sunshine_route(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<PostWireGuardSetupState>, ApiError> {
    let result = setup_moonlight_sunshine(&state.app, &state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn submit_pin(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<PinRequest>,
) -> Result<Json<PostWireGuardSetupState>, ApiError> {
    let result = submit_moonlight_pin_to_sunshine(&state.app, &state.context, payload.pin)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn moonlight_download_url(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let os = OsDetection::new();
    if os.is_windows() {
        Ok(Json(state.context.config.moonlight_download_url_windows.clone()))
    } else if os.is_macos() {
        Ok(Json(state.context.config.moonlight_download_url_macos.clone()))
    } else {
        Ok(Json(state.context.config.moonlight_download_url_linux.clone()))
    }
}

pub async fn launch_moonlight() -> Result<Json<bool>, ApiError> {
    let moonlight = MoonlightService;
    moonlight
        .launch_native_client()
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn configure_moonlight(
    Json(payload): Json<ConfigureMoonlightRequest>,
) -> Result<Json<MoonlightConfigureResult>, ApiError> {
    let moonlight = MoonlightService;
    let resolution_override = payload
        .resolution
        .as_deref()
        .and_then(|value| value.split_once('x'))
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)));
    let network = match payload.network.as_deref() {
        Some("lan") => MoonlightNetworkPreference::Lan,
        Some("wifi") => MoonlightNetworkPreference::Wifi,
        Some("remote") => MoonlightNetworkPreference::Remote,
        _ => MoonlightNetworkPreference::Auto,
    };
    let prefer_codec = match payload.prefer_codec.as_deref() {
        Some("h264") => MoonlightCodecPreference::H264,
        Some("hevc") => MoonlightCodecPreference::Hevc,
        Some("av1") => MoonlightCodecPreference::Av1,
        _ => MoonlightCodecPreference::Auto,
    };
    let result = moonlight
        .configure_client(MoonlightConfigureOptions {
            apply: payload.apply,
            force_close: payload.force_close,
            native: payload.native,
            network,
            prefer_codec,
            max_bitrate: payload.max_bitrate,
            fps_override: payload.fps,
            resolution_override,
            set_overrides: BTreeMap::new(),
        })
        .await;
    Ok(Json(result))
}

pub async fn restore_moonlight_backup(
    Json(payload): Json<RestoreBackupRequest>,
) -> Result<Json<String>, ApiError> {
    let moonlight = MoonlightService;
    let result = moonlight
        .restore_backup(&payload.backup_file)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}
