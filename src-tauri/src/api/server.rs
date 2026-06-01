use std::{net::SocketAddr, sync::Arc};

use axum::{routing::{delete, get, post, put}, Router};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;

use super::{routes, state::ApiState};

pub fn build_router(state: Arc<ApiState>) -> Router {
    let protected = Router::new()
        .route("/state", get(routes::app_state::get_app_state))
        .route("/onboarding/complete", post(routes::onboarding::complete_onboarding))
        .route("/location/ip/refresh", post(routes::location::refresh_ip_location))
        .route("/location/manual", put(routes::location::set_manual_location))
        .route("/location/os", put(routes::location::set_os_location))
        .route("/offers", get(routes::offers::search_offers))
        .route("/offers/selected", put(routes::offers::select_offer))
        .route("/orchestration/play/start", post(routes::orchestration::start_play))
        .route("/orchestration/play/start-existing", post(routes::orchestration::start_play_existing))
        .route("/orchestration/pairing/pin", post(routes::orchestration::submit_pairing_pin))
        .route("/orchestration/pairing/skip", post(routes::orchestration::skip_pairing))
        .route("/orchestration/logs", get(routes::orchestration::get_logs))
        .route("/orchestration/retry-stage", post(routes::orchestration::retry_stage))
        .route("/wireguard/local/setup", post(routes::wireguard::setup_wireguard_client))
        .route("/wireguard/local/reconnect", post(routes::wireguard::reconnect_local_wireguard_client))
        .route("/wireguard/handoff/start", post(routes::wireguard::setup_handoff))
        .route("/wireguard/verify", post(routes::wireguard::verify_wireguard))
        .route("/wireguard/app/open", post(routes::wireguard::open_wireguard_app_route))
        .route("/wireguard/config/download", get(routes::wireguard::download_wireguard_config_route))
        .route("/wireguard/setup-status", get(routes::wireguard::get_setup_status_route))
        .route("/wireguard/download-url", get(routes::wireguard::download_url))
        .route("/sunshine/verify", post(routes::moonlight::verify_sunshine))
        .route("/moonlight/detect", get(routes::moonlight::detect_moonlight))
        .route("/moonlight-sunshine/setup", post(routes::moonlight::setup_moonlight_sunshine_route))
        .route("/moonlight-sunshine/pin", post(routes::moonlight::submit_pin))
        .route("/moonlight/download-url", get(routes::moonlight::moonlight_download_url))
        .route("/moonlight/launch", post(routes::moonlight::launch_moonlight))
        .route("/moonlight/configure", post(routes::moonlight::configure_moonlight))
        .route("/moonlight/restore-backup", post(routes::moonlight::restore_moonlight_backup))
        .route("/instances/rented", get(routes::instances::get_rented_instances))
        .route("/instances/:instance_id/services/reboot", post(routes::instances::reboot_instance_services))
        .route("/instances/:instance_id/pause", post(routes::instances::pause_instance))
        .route("/instances/:instance_id", delete(routes::instances::destroy_instance))
        .route("/instances/:instance_id/wireguard/reconnect", post(routes::wireguard::reconnect_instance_wireguard))
        .route("/instances/:instance_id/sunshine-settings/get", post(routes::instances::get_instance_sunshine_settings))
        .route("/instances/:instance_id/sunshine-settings", put(routes::instances::update_instance_sunshine_settings))
        .route("/instances/:instance_id/sunshine-settings/reset", post(routes::instances::reset_instance_sunshine_settings))
        .route("/settings/vast-api-key", put(routes::settings::update_vast_api_key))
        .route("/settings/platform-credentials", put(routes::settings::update_platform_credentials))
        .route("/settings/server-preferences", put(routes::settings::update_server_preferences))
        .route("/settings/moonlight-preferences", put(routes::settings::update_moonlight_preferences))
        .route("/settings/edid/regenerate", post(routes::settings::regenerate_edid))
        .route("/settings/ssh-credentials", put(routes::settings::update_ssh_credentials))
        .route("/shared-storage/settings", get(routes::shared_storage::get_shared_storage_settings))
        .route("/shared-storage/settings", put(routes::shared_storage::save_shared_storage_settings))
        .route("/shared-storage/settings/test", post(routes::shared_storage::test_shared_storage_config))
        .route("/shared-storage/backup/trigger", post(routes::shared_storage::trigger_instance_backup))
        .route("/shared-storage/backup/trigger/:instance_id", post(routes::shared_storage::trigger_instance_backup_for))
        .route("/shared-storage/sync/:instance_id", post(routes::shared_storage::sync_instance_from_shared_storage))
        .route("/shared-storage/objects/:instance_id", get(routes::shared_storage::list_instance_shared_storage_objects))
        .route("/shared-storage/sync/:instance_id/selected", post(routes::shared_storage::sync_instance_from_shared_storage_selected))
        .route("/shared-storage/exportable-objects/:instance_id", get(routes::shared_storage::list_instance_exportable_storage_objects))
        .route("/shared-storage/save/:instance_id/selected", post(routes::shared_storage::save_instance_to_shared_storage_selected))
        .route("/shared-storage/backup/status", get(routes::shared_storage::get_instance_backup_status))
        .route("/shared-storage/backup/schedule/setup", post(routes::shared_storage::setup_instance_backup_schedule))
        .route("/shared-storage/backup/schedule/remove", post(routes::shared_storage::remove_instance_backup_schedule))
        .route("/restore/bundles/index/generate", post(routes::restore::generate_bundle_index))
        .route("/restore/bundles/:instance_id", get(routes::restore::get_instance_restore_bundles))
        .route("/restore/:instance_id/dry-run", post(routes::restore::dry_run_restore))
        .route("/restore/:instance_id/run", post(routes::restore::restore_bundle))
        .route("/restore/jobs/:job_id", get(routes::restore::get_restore_job));

    Router::new()
        .route("/health", get(routes::health::health))
        .nest("/api/v1", protected)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

pub async fn run(state: Arc<ApiState>, bind_addr: SocketAddr) -> Result<(), String> {
    let app = build_router(state);
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|error| format!("Failed binding API server: {error}"))?;
    info!("HTTP API listening on {}", bind_addr);
    axum::serve(listener, app)
        .await
        .map_err(|error| format!("HTTP API server error: {error}"))
}
