use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use tauri::AppHandle;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::{
    errors::{AppError, AppResult},
    models::{
        app_state::{
            OrchestrationState, PairingContext, ProvisionedServerState, ProvisionedServerSteps,
        },
        events::ProvisioningEvent,
    },
};

use super::{
    app_context::{AppContext, OrchestrationStartRequest},
    audio_latency::AudioLatencyService,
    instance_manager::InstanceManager,
    moonlight::MoonlightService,
    nvidia_headless::NvidiaHeadlessService,
    post_provision::PostProvisionService,
    remote_exec::RemoteExec,
    shared_storage::shared_storage_manager::SharedStorageManager,
    ssh_keys::SshKeyService,
    sunshine::SunshineService,
    vast_api::VastApiClient,
    wireguard::{
        reconnect_local_wireguard_client, WireGuardProvisionMode, WireGuardProvisionResult,
        WireGuardService,
    },
};

#[derive(Debug, Clone)]
pub struct OrchestrationService;

impl OrchestrationService {
    pub async fn start_play_flow(app: AppHandle, context: AppContext) -> AppResult<()> {
        Self::request_start(app, context, OrchestrationStartRequest::SelectedOffer).await
    }

    pub async fn start_play_for_existing_instance(
        app: AppHandle,
        context: AppContext,
        instance_id: u64,
    ) -> AppResult<()> {
        Self::request_start(
            app,
            context,
            OrchestrationStartRequest::ExistingInstance(instance_id),
        )
        .await
    }

    async fn request_start(
        app: AppHandle,
        context: AppContext,
        request: OrchestrationStartRequest,
    ) -> AppResult<()> {
        let mut guard = context.orchestration_guard.lock().await;
        if *guard {
            {
                let mut pending = context.pending_start.lock().await;
                *pending = Some(request);
            }
            context.cancel_requested.store(true, Ordering::SeqCst);
            drop(guard);

            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringRemote,
                "Stopping current setup and switching to the new server...",
                None,
                false,
            )
            .await;

            return Ok(());
        }

        *guard = true;
        drop(guard);

        context.cancel_requested.store(false, Ordering::SeqCst);
        Self::spawn_run(app, context, request);
        Ok(())
    }

    fn spawn_run(app: AppHandle, context: AppContext, request: OrchestrationStartRequest) {
        tauri::async_runtime::spawn(async move {
            let result = match request {
                OrchestrationStartRequest::SelectedOffer => {
                    run_orchestration(app.clone(), context.clone()).await
                }
                OrchestrationStartRequest::ExistingInstance(instance_id) => {
                    run_existing_instance_orchestration(app.clone(), context.clone(), instance_id)
                        .await
                }
            };

            match result {
                Ok(()) => {}
                Err(AppError::Cancelled) => {
                    emit_transition(
                        &app,
                        &context,
                        OrchestrationState::Idle,
                        "Current setup cancelled.",
                        None,
                        false,
                    )
                    .await;
                }
                Err(error) => {
                    error!("orchestration failed: {error}");
                    let instance_id_for_error = { context.state.read().await.instance.instance_id };
                    if let Err(track_error) =
                        mark_server_error(&context, instance_id_for_error, &error.to_string()).await
                    {
                        warn!("failed to mark server error in state: {track_error}");
                    }
                    let details = Some(error.to_string());
                    emit_transition(
                        &app,
                        &context,
                        OrchestrationState::Error,
                        "Provisioning failed",
                        details,
                        true,
                    )
                    .await;
                }
            }

            {
                let mut guard = context.orchestration_guard.lock().await;
                *guard = false;
            }

            let pending = {
                let mut pending = context.pending_start.lock().await;
                pending.take()
            };

            if let Some(next_request) = pending {
                if let Err(error) =
                    Self::request_start(app.clone(), context.clone(), next_request).await
                {
                    error!("failed to start queued orchestration request: {error}");
                    emit_transition(
                        &app,
                        &context,
                        OrchestrationState::Error,
                        "Failed starting queued server setup",
                        Some(error.to_string()),
                        true,
                    )
                    .await;
                }
            }
        });
    }

    pub async fn submit_pairing_pin(
        app: &AppHandle,
        context: &AppContext,
        pin: String,
    ) -> AppResult<()> {
        let pin = pin.trim().to_string();
        if !is_valid_pairing_pin(&pin) {
            return Err(AppError::InvalidInput(
                "PIN should have at least 4 digits".to_string(),
            ));
        }

        let pairing_context = context
            .pairing_context
            .read()
            .await
            .clone()
            .ok_or_else(|| AppError::State("No active pairing session".to_string()))?;

        emit_transition(
            app,
            context,
            OrchestrationState::Pairing,
            "Submitting pairing PIN to Sunshine",
            None,
            false,
        )
        .await;

        let private_key_path = {
            let state = context.state.read().await;
            state.ssh.private_key_path.clone()
        };
        ensure_private_key_path_exists(Path::new(&private_key_path))?;

        let passphrase = {
            let state = context.state.read().await;
            state.credentials.app_password.clone()
        };

        if passphrase.is_empty() {
            return Err(AppError::InvalidInput(
                "Platform password is required to unlock SSH key".to_string(),
            ));
        }

        let ssh_service = SshKeyService::new("nolandConnectSSH");
        ssh_service
            .load_key_into_agent(Path::new(&private_key_path), &passphrase)
            .await?;

        let sunshine_user = sanitize_ssh_user(&context.config.audio_target_user);
        let ssh_username = {
            let state = context.state.read().await;
            if state.ssh.ssh_username.is_empty() {
                context.config.ssh_user.clone()
            } else {
                state.ssh.ssh_username.clone()
            }
        };
        let remote = RemoteExec {
            ssh_user: sanitize_ssh_user(&ssh_username),
            ssh_host: pairing_context.host.clone(),
            ssh_port: pairing_context.port,
            private_key_path,
        };

        let pairing_mode = detect_sunshine_pairing_mode(&remote).await?;
        match pairing_mode {
            SunshinePairingMode::SunshineCli => {
                let command = format!("sudo -u {sunshine_user} bash -lc 'printf \"%s\\n\" \"{pin}\" | sunshine-cli pair'");
                let remote_for_pair = remote.clone();
                let pair_result =
                    tokio::task::spawn_blocking(move || remote_for_pair.ssh(&command, Duration::from_secs(45)))
                        .await
                        .map_err(|error| {
                            AppError::Command(format!("Failed to join pairing task: {error}"))
                        })??;

                if pair_result.status_code != 0 {
                    return Err(AppError::Provisioning(format!(
                        "Pairing failed (exit {}): stdout: {} | stderr: {}",
                        pair_result.status_code,
                        pair_result.stdout.trim(),
                        pair_result.stderr.trim()
                    )));
                }
            }
            SunshinePairingMode::SunshinePairPin => {
                let command = format!("sudo -u {sunshine_user} bash -lc 'sunshine --pair-pin \"{pin}\"'");
                let remote_for_pair = remote.clone();
                let pair_result =
                    tokio::task::spawn_blocking(move || remote_for_pair.ssh(&command, Duration::from_secs(45)))
                        .await
                        .map_err(|error| {
                            AppError::Command(format!("Failed to join pairing task: {error}"))
                        })??;

                if pair_result.status_code != 0 {
                    return Err(AppError::Provisioning(format!(
                        "Pairing failed (exit {}): stdout: {} | stderr: {}",
                        pair_result.status_code,
                        pair_result.stdout.trim(),
                        pair_result.stderr.trim()
                    )));
                }
            }
            SunshinePairingMode::ManualWebUi => {
                info!(
                    "Sunshine build requires manual Web UI pairing; skipping version-specific pairing state verification"
                );
            }
        }

        {
            let mut pin_memory = context.pairing_pin_in_memory.write().await;
            *pin_memory = Some(pin.clone());
        }

        context
            .update_state(|state| {
                state.orchestration_state = OrchestrationState::Ready;
                state.last_error = None;
            })
            .await?;

        let active_instance = { context.state.read().await.instance.instance_id };
        if let Some(instance_id) = active_instance {
            let snapshot = context.state.read().await.clone();
            mark_server_step_completed(
                context,
                instance_id,
                ProvisionStepMarker::PairingCompleted,
                OrchestrationState::Ready,
                &snapshot.instance.status,
                &snapshot.instance.ssh_host,
                snapshot.instance.ssh_port,
                snapshot.instance.offer_id,
            )
            .await?;

            if let Err(error) = run_post_provision_step(
                app,
                context,
                &remote,
                instance_id,
                snapshot.instance.offer_id,
            )
            .await
            {
                warn!("Post-provision setup failed (non-fatal): {error}");
            }
        }

        emit_transition(
            app,
            context,
            OrchestrationState::Ready,
            "Pairing complete. Server is ready to stream.",
            None,
            false,
        )
        .await;

        Ok(())
    }

    pub async fn skip_pairing_and_continue(app: &AppHandle, context: &AppContext) -> AppResult<()> {
        let pairing_context = context.pairing_context.read().await.clone();
        if pairing_context.is_none() {
            return Err(AppError::State(
                "No active pairing session to skip".to_string(),
            ));
        }
        let pairing_context = pairing_context.expect("checked above");

        emit_transition(
            app,
            context,
            OrchestrationState::Pairing,
            "Skipping pairing PIN entry by user request",
            Some("Continuing provisioning and marking session as ready".to_string()),
            false,
        )
        .await;

        context
            .update_state(|state| {
                state.orchestration_state = OrchestrationState::Ready;
                state.last_error = None;
            })
            .await?;

        let active_instance = { context.state.read().await.instance.instance_id };
        if let Some(instance_id) = active_instance {
            let snapshot = context.state.read().await.clone();
            mark_server_step_completed(
                context,
                instance_id,
                ProvisionStepMarker::PairingCompleted,
                OrchestrationState::Ready,
                &snapshot.instance.status,
                &snapshot.instance.ssh_host,
                snapshot.instance.ssh_port,
                snapshot.instance.offer_id,
            )
            .await?;

            if !snapshot.ssh.private_key_path.trim().is_empty() {
                let remote = RemoteExec {
                    ssh_user: sanitize_ssh_user(&pairing_context.user),
                    ssh_host: pairing_context.host.clone(),
                    ssh_port: pairing_context.port,
                    private_key_path: snapshot.ssh.private_key_path.clone(),
                };
                if let Err(error) = run_post_provision_step(
                    app,
                    context,
                    &remote,
                    instance_id,
                    snapshot.instance.offer_id,
                )
                .await
                {
                    warn!("Post-provision setup failed (non-fatal): {error}");
                }
            } else {
                warn!("Skipping post-provision setup because SSH private key path is missing");
            }
        }

        emit_transition(
            app,
            context,
            OrchestrationState::Ready,
            "Provisioning complete. Server is ready to stream.",
            Some(
                "Pairing modal skipped. Complete Moonlight pairing manually if still needed."
                    .to_string(),
            ),
            false,
        )
        .await;

        Ok(())
    }

    pub async fn resume_if_needed(app: &AppHandle, context: &AppContext) {
        let state = context.state.read().await.clone();
        if matches!(
            state.orchestration_state,
            OrchestrationState::AwaitingPairPin | OrchestrationState::Pairing
        ) {
            emit_transition(
                app,
                context,
                state.orchestration_state,
                "Resumed pairing state from disk",
                None,
                false,
            )
            .await;
        }
    }
}

async fn run_post_provision_step(
    app: &AppHandle,
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    offer_id: Option<u64>,
) -> AppResult<()> {
    if server_step_is_completed(context, instance_id, ProvisionStepMarker::PostProvisionCompleted).await {
        emit_step_skipped(
            app,
            context,
            OrchestrationState::Ready,
            "Skipping post-provision setup",
            instance_id,
        )
        .await;
        return Ok(());
    }

    emit_transition(
        app,
        context,
        OrchestrationState::Ready,
        "Running post-provision setup",
        Some("Installing Chrome, Heroic, Wine and launcher shortcuts".to_string()),
        false,
    )
    .await;

    let output = PostProvisionService::run(remote, &context.config.audio_target_user).await?;

    if let Err(error) = SharedStorageManager::auto_restore_instance(
        context,
        remote,
        instance_id,
        &context.config.audio_target_user,
    )
    .await
    {
        warn!(
            instance_id = instance_id,
            error = %error,
            "Auto-restore after post-provision failed (non-blocking)"
        );
    }

    let snapshot = context.state.read().await.clone();
    mark_server_step_completed(
        context,
        instance_id,
        ProvisionStepMarker::PostProvisionCompleted,
        OrchestrationState::Ready,
        &snapshot.instance.status,
        &snapshot.instance.ssh_host,
        snapshot.instance.ssh_port,
        offer_id,
    )
    .await?;

    emit_transition(
        app,
        context,
        OrchestrationState::Ready,
        "Post-provision setup complete",
        Some(summarize_verification_output(&output)),
        false,
    )
    .await;

    Ok(())
}

async fn run_orchestration(app: AppHandle, context: AppContext) -> AppResult<()> {
    ensure_not_cancelled(&context)?;

    let initial_state = context.state.read().await.clone();

    let offer = initial_state.selected_offer.clone().ok_or_else(|| {
        AppError::InvalidInput("Select a server before clicking Play".to_string())
    })?;

    let api_key = initial_state.credentials.vast_api_key.clone();
    if api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Missing Vast.ai API key. Complete onboarding first.".to_string(),
        ));
    }

    let app_data_dir = context.state_store.path().parent().ok_or_else(|| {
        AppError::State("Could not resolve app data directory from state file path".to_string())
    })?;

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );

    ensure_not_cancelled(&context)?;

    match vast.list_instances().await {
        Ok(existing_instances) => {
            let existing_count = existing_instances
                .iter()
                .filter(|instance| {
                    let status = instance.status.to_ascii_lowercase();
                    !status.contains("destroy")
                        && !status.contains("stopped")
                        && !status.contains("exited")
                        && !instance.ssh_host.is_empty()
                })
                .count();
            if existing_count > 0 {
                info!(
                    "Found {} active rented instance(s) in account, but selected-offer flow will still request a new instance",
                    existing_count
                );
            }
        }
        Err(error) => {
            warn!(
                "Failed to list existing instances before create flow; continuing anyway: {}",
                error
            );
        }
    }

    emit_transition(
        &app,
        &context,
        OrchestrationState::CreatingInstance,
        "No active rented instance found. Creating a new reservation.",
        Some(format!("Offer {} selected", offer.id)),
        false,
    )
    .await;

    emit_transition(
        &app,
        &context,
        OrchestrationState::GeneratingSshKey,
        "Ensuring SSH keypair exists",
        None,
        false,
    )
    .await;

    let ssh_service = SshKeyService::new(initial_state.ssh.key_name.clone());
    let key_paths = ssh_service.ensure_keypair(app_data_dir).await?;
    ensure_private_key_path_exists(key_paths.private_key_path.as_path())?;
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::UploadingSshKeyToVast,
        "Syncing SSH key with Vast.ai",
        None,
        false,
    )
    .await;

    let uploaded = ssh_service
        .upload_public_key_if_missing(&vast, &key_paths.public_key_path)
        .await?;

    context
        .update_state(|state| {
            state.ssh.private_key_path = key_paths.private_key_path.display().to_string();
            state.ssh.public_key_path = key_paths.public_key_path.display().to_string();
            state.ssh.uploaded_to_vast = uploaded || state.ssh.uploaded_to_vast;
            state.last_error = None;
        })
        .await?;
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::CreatingInstance,
        "Creating Vast.ai instance",
        Some(format!(
            "Offer {} using template {}",
            offer.id, initial_state.server_preferences.template_hash
        )),
        false,
    )
    .await;

    let instance_manager = InstanceManager {
        poll_interval: context.config.poll_interval,
        max_attempts: context.config.poll_max_attempts,
    };

    info!(
        "Provisioning create_instance start offer_id={} template_hash={} storage_gb={}",
        offer.id,
        initial_state.server_preferences.template_hash,
        initial_state.server_preferences.storage_gb
    );

    let mut instance = match instance_manager
        .create_instance(
            &vast,
            offer.id,
            &initial_state.server_preferences.template_hash,
            initial_state.server_preferences.storage_gb,
        )
        .await
    {
        Ok(instance) => {
            info!(
                "Provisioning create_instance success offer_id={} instance_id={} status={} ssh={}:{}",
                offer.id, instance.id, instance.status, instance.ssh_host, instance.ssh_port
            );
            instance
        }
        Err(error) => {
            warn!(
                "Provisioning create_instance failed offer_id={} error={}",
                offer.id, error
            );
            if is_no_such_ask_error(&error) {
                let existing = match vast.list_instances().await {
                    Ok(instances) => find_active_rented_instance(instances),
                    Err(error) => {
                        warn!(
                            "Create-instance fallback could not list instances; skipping fallback reuse: {}",
                            error
                        );
                        None
                    }
                };
                if let Some(existing) = existing {
                    emit_transition(
                        &app,
                        &context,
                        OrchestrationState::WaitingForInstance,
                        "Selected offer became unavailable. Reusing your existing rented server.",
                        Some(format!(
                            "Instance {} status {}",
                            existing.id, existing.status
                        )),
                        false,
                    )
                    .await;

                    context
                        .update_state(|state| {
                            state.instance.instance_id = Some(existing.id);
                            state.instance.status = existing.status.clone();
                            state.instance.ssh_host = existing.ssh_host.clone();
                            state.instance.ssh_port = existing.ssh_port;
                            state.instance.ssh_user = context.config.ssh_user.clone();
                            state.instance.ssh_command = existing.ssh_command.clone();
                        })
                        .await?;

                    let _ = hydrate_state_from_server_record(&context, existing.id, false).await?;

                    if try_launch_existing_moonlight_session(&app, &context, Some(existing.id))
                        .await?
                    {
                        return Ok(());
                    }

                    return run_existing_instance_orchestration(app, context, existing.id).await;
                }

                return Err(AppError::Provisioning(format!(
                    "Create-instance failed because the selected offer is no longer available, and no active fallback instance was found in your account. Root error: {}",
                    error
                )));
            }

            return Err(error);
        }
    };
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::CreatingInstance,
        "Create-instance request accepted by Vast",
        Some(format!(
            "Instance {} status {} ssh {}:{}",
            instance.id, instance.status, instance.ssh_host, instance.ssh_port
        )),
        false,
    )
    .await;

    context
        .update_state(|state| {
            state.instance.instance_id = Some(instance.id);
            state.instance.offer_id = Some(offer.id);
            state.instance.status = instance.status.clone();
            state.instance.ssh_host = instance.ssh_host.clone();
            state.instance.ssh_port = instance.ssh_port;
            state.instance.ssh_user = context.config.ssh_user.clone();
            state.instance.ssh_command = instance.ssh_command.clone();
        })
        .await?;

    ensure_server_record(
        &context,
        instance.id,
        Some(offer.id),
        &instance.ssh_host,
        instance.ssh_port,
        &instance.status,
        OrchestrationState::CreatingInstance,
    )
    .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::SshKeyReady,
        OrchestrationState::GeneratingSshKey,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        Some(offer.id),
    )
    .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::SshKeyUploadedToVast,
        OrchestrationState::UploadingSshKeyToVast,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        Some(offer.id),
    )
    .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::InstanceCreated,
        OrchestrationState::CreatingInstance,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        Some(offer.id),
    )
    .await?;

    if server_step_is_completed(&context, instance.id, ProvisionStepMarker::InstanceReady).await {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::WaitingForInstance,
            "Skipping instance readiness wait",
            instance.id,
        )
        .await;
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::WaitingForInstance,
            "Waiting for instance readiness",
            Some("Polling every 60 seconds".to_string()),
            false,
        )
        .await;

        instance = instance_manager
            .wait_until_ssh_ready(
                &vast,
                instance.id,
                |attempt, current| {
                    info!(
                        "poll attempt {attempt} instance {} status {}",
                        current.id, current.status
                    );
                },
                || context.cancel_requested.load(Ordering::SeqCst),
            )
            .await?;

        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::InstanceReady,
            OrchestrationState::WaitingForInstance,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    context
        .update_state(|state| {
            state.instance.status = instance.status.clone();
            state.instance.ssh_host = instance.ssh_host.clone();
            state.instance.ssh_port = instance.ssh_port;
            state.instance.ssh_command = instance.ssh_command.clone();
        })
        .await?;

    instance =
        verify_instance_reserved_in_account(&app, &context, &vast, instance.id, Some(offer.id))
            .await?;
    ensure_instance_is_vm_runtime(&instance)?;
    ensure_not_cancelled(&context)?;

    // ssh_user: who we authenticate as over SSH (typically root on cloud VMs)
    // target_user: who Sunshine/Xorg run as (unprivileged user)
    let ssh_user = sanitize_ssh_user(&{
        let state = context.state.read().await;
        if state.ssh.ssh_username.is_empty() {
            context.config.ssh_user.clone()
        } else {
            state.ssh.ssh_username.clone()
        }
    });
    let target_user = sanitize_ssh_user(&context.config.audio_target_user);
    let mut remote = RemoteExec {
        ssh_user,
        ssh_host: instance.public_ip.clone(),
        ssh_port: instance.ssh_port,
        private_key_path: key_paths.private_key_path.display().to_string(),
    };

    if server_step_is_completed(&context, instance.id, ProvisionStepMarker::SshConnected).await {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::ConnectingSsh,
            "Skipping SSH connectivity check",
            instance.id,
        )
        .await;
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConnectingSsh,
            "Checking SSH connectivity",
            Some(format!("{}:{}", instance.public_ip, instance.ssh_port)),
            false,
        )
        .await;

        wait_for_ssh_acceptance(&app, &context, &remote, &vast, instance.id).await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::SshConnected,
            OrchestrationState::ConnectingSsh,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::ConfiguringNvidiaHeadless,
        "Configuring NVIDIA headless streaming",
        None,
        false,
    )
    .await;

    let nvidia = NvidiaHeadlessService;
    if server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::NvidiaHeadlessConfigured,
    )
    .await
    {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::ConfiguringNvidiaHeadless,
            "Skipping NVIDIA headless setup",
            instance.id,
        )
        .await;
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringNvidiaHeadless,
            "Configuring NVIDIA headless streaming",
            None,
            false,
        )
        .await;

        match nvidia.setup_and_validate(&remote).await {
            Ok(()) => {}
            Err(AppError::DriverMismatch(_)) => {
                warn!("NVIDIA driver mismatch detected — triggering reboot and retry");
                ensure_post_nvidia_reboot(
                    &app,
                    &context,
                    &vast,
                    &mut instance,
                    &mut remote,
                    Some(offer.id),
                )
                .await?;
                ensure_not_cancelled(&context)?;
                // Retry NVIDIA setup after reboot
                if let Err(error) = nvidia.setup_and_validate(&remote).await {
                    let diagnostics = nvidia.collect_diagnostics(&remote).await.ok();
                    let diag_summary = diagnostics
                        .map(|diag| {
                            diag.commands
                                .into_iter()
                                .map(|(command, output)| {
                                    format!("{command} -> {}", output.status_code)
                                })
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .unwrap_or_else(|| "no diagnostics collected".to_string());
                    return Err(AppError::Provisioning(format!(
                        "{error}. Diagnostics: {diag_summary}"
                    )));
                }
            }
            Err(error) => {
                let diagnostics = nvidia.collect_diagnostics(&remote).await.ok();
                let diag_summary = diagnostics
                    .map(|diag| {
                        diag.commands
                            .into_iter()
                            .map(|(command, output)| format!("{command} -> {}", output.status_code))
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_else(|| "no diagnostics collected".to_string());
                return Err(AppError::Provisioning(format!(
                    "{error}. Diagnostics: {diag_summary}"
                )));
            }
        }

        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::NvidiaHeadlessConfigured,
            OrchestrationState::ConfiguringNvidiaHeadless,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    ensure_post_nvidia_reboot(
        &app,
        &context,
        &vast,
        &mut instance,
        &mut remote,
        Some(offer.id),
    )
    .await?;
    ensure_not_cancelled(&context)?;

    let sunshine = SunshineService {
        defaults: context.config.sunshine.clone(),
    };
    let sunshine_step_completed = server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::SunshineConfigured,
    )
    .await;
    let mut should_install_sunshine = !sunshine_step_completed;
    if sunshine_step_completed {
        match sunshine.verify_resume_health(&remote, &target_user).await {
            Ok(()) => {
                emit_step_skipped(
                    &app,
                    &context,
                    OrchestrationState::ConfiguringSunshine,
                    "Skipping Sunshine install/config",
                    instance.id,
                )
                .await;
            }
            Err(error) => {
                warn!(
                    "Saved Sunshine state drifted for instance {}. Forcing full reconfiguration. {}",
                    instance.id, error
                );
                emit_transition(
                    &app,
                    &context,
                    OrchestrationState::ConfiguringSunshine,
                    "Saved Sunshine state is stale. Reconfiguring Sunshine.",
                    Some("Remote Sunshine preflight failed; rerunning full setup".to_string()),
                    false,
                )
                .await;
                should_install_sunshine = true;
            }
        }
    }
    if should_install_sunshine {
        let moonlight_preferences = { context.state.read().await.moonlight_preferences.clone() };
        let display_profile = crate::services::sunshine::DisplayProfile::from_moonlight_prefs(
            moonlight_preferences.width,
            moonlight_preferences.height,
            moonlight_preferences.fps,
        );
        info!(
            "Sunshine display profile: {}x{} @ {}Hz ({} FPS x2)",
            display_profile.width,
            display_profile.height,
            display_profile.virtual_hz(),
            display_profile.fps
        );
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Installing and configuring Sunshine",
            Some(format!(
                "Display: {}x{} @ {}Hz",
                display_profile.width,
                display_profile.height,
                display_profile.virtual_hz()
            )),
            false,
        )
        .await;
        sunshine
            .install_and_configure(&remote, &target_user, display_profile)
            .await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::SunshineConfigured,
            OrchestrationState::ConfiguringSunshine,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    let audio_latency = AudioLatencyService::from_config(&context.config);
    if server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::LowLatencyAudioConfigured,
    )
    .await
    {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Skipping low-latency audio setup",
            instance.id,
        )
        .await;
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Applying low-latency PipeWire/WirePlumber audio profile",
            Some(format!(
                "target_user={} profile={}",
                context.config.audio_target_user, context.config.audio_profile
            )),
            false,
        )
        .await;

        let audio_result = audio_latency.configure(&remote).await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::LowLatencyAudioConfigured,
            OrchestrationState::ConfiguringSunshine,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;

        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Low-latency audio profile configured",
            Some(summarize_verification_output(
                &audio_result.verification_output,
            )),
            false,
        )
        .await;
    }
    ensure_not_cancelled(&context)?;

    match vast.get_instance(instance.id).await {
        Ok(refreshed_instance) => {
            instance.public_ip = refreshed_instance.public_ip;
            instance.ssh_host = refreshed_instance.ssh_host;
            instance.wireguard_port = refreshed_instance.wireguard_port;
            info!(
                "Refreshed instance networking before WireGuard: public_ip={} ssh_host={} wireguard_port={}",
                instance.public_ip, instance.ssh_host, instance.wireguard_port
            );
        }
        Err(error) => {
            warn!(
                "Failed to refresh instance networking before WireGuard; using cached values: {}",
                error
            );
        }
    }

    let wireguard = WireGuardService {
        defaults: context.config.wireguard.clone(),
    };
    let endpoint_host = instance.wireguard_endpoint_host();
    let endpoint_port = instance.wireguard_port;
    if endpoint_port == 0 {
        return Err(AppError::Provisioning(format!(
            "Instance {} does not expose 51820/udp on Vast. Pick a VM-enabled offer with direct UDP ports.",
            instance.id
        )));
    }

    let wireguard_step_completed = server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::WireguardConfigured,
    )
    .await;

    let wireguard_result: WireGuardProvisionResult = if wireguard_step_completed {
        if let Some(cached) = load_wireguard_result_from_server_record(&context, instance.id).await
        {
            emit_step_skipped(
                &app,
                &context,
                OrchestrationState::ConfiguringWireGuard,
                "Skipping WireGuard setup",
                instance.id,
            )
            .await;

            context
                .update_state(|state| {
                    state.wireguard.server_ip = cached.server_ip.clone();
                    state.wireguard.client_ip = cached.client_ip.clone();
                    state.wireguard.server_public_key = cached.server_public_key.clone();
                    state.wireguard.client_public_key = cached.client_public_key.clone();
                    state.wireguard.config_path = cached.client_config_path.display().to_string();
                    state.sunshine.configured = true;
                })
                .await?;

            cached
        } else {
            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringWireGuard,
                "WireGuard checkpoint is stale. Reconfiguring tunnel.",
                Some("No saved WireGuard artifacts were found for this instance".to_string()),
                false,
            )
            .await;

            clear_server_steps(
                &context,
                instance.id,
                &[
                    ProvisionStepMarker::WireguardConfigured,
                    ProvisionStepMarker::MoonlightConfigured,
                    ProvisionStepMarker::AwaitingPairPin,
                    ProvisionStepMarker::PairingCompleted,
                ],
            )
            .await?;

            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringWireGuard,
                "Setting up WireGuard tunnel",
                None,
                false,
            )
            .await;

            let result = wireguard
                .configure(
                    &remote,
                    app_data_dir,
                    &endpoint_host,
                    endpoint_port,
                    WireGuardProvisionMode::FreshProvision,
                )
                .await?;
            mark_server_step_completed(
                &context,
                instance.id,
                ProvisionStepMarker::WireguardConfigured,
                OrchestrationState::ConfiguringWireGuard,
                &instance.status,
                &instance.ssh_host,
                instance.ssh_port,
                Some(offer.id),
            )
            .await?;
            persist_wireguard_result_for_server(&context, instance.id, &result).await?;

            context
                .update_state(|state| {
                    state.wireguard.server_ip = result.server_ip.clone();
                    state.wireguard.client_ip = result.client_ip.clone();
                    state.wireguard.server_public_key = result.server_public_key.clone();
                    state.wireguard.client_public_key = result.client_public_key.clone();
                    state.wireguard.config_path = result.client_config_path.display().to_string();
                    state.sunshine.configured = true;
                })
                .await?;

            result
        }
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringWireGuard,
            "Setting up WireGuard tunnel",
            None,
            false,
        )
        .await;

        let result = wireguard
            .configure(
                &remote,
                app_data_dir,
                &endpoint_host,
                endpoint_port,
                WireGuardProvisionMode::FreshProvision,
            )
            .await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::WireguardConfigured,
            OrchestrationState::ConfiguringWireGuard,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;
        persist_wireguard_result_for_server(&context, instance.id, &result).await?;

        context
            .update_state(|state| {
                state.wireguard.server_ip = result.server_ip.clone();
                state.wireguard.client_ip = result.client_ip.clone();
                state.wireguard.server_public_key = result.server_public_key.clone();
                state.wireguard.client_public_key = result.client_public_key.clone();
                state.wireguard.config_path = result.client_config_path.display().to_string();
                state.sunshine.configured = true;
            })
            .await?;

        result
    };
    ensure_not_cancelled(&context)?;

    if !wireguard_step_completed {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringWireGuard,
            "Applying local WireGuard tunnel",
            Some(
                "Replacing any previous local tunnel interface with the newly provisioned config"
                    .to_string(),
            ),
            false,
        )
        .await;

        reconnect_local_wireguard_client(&wireguard_result.client_config_path)?;
        ensure_not_cancelled(&context)?;
    }

    let moonlight = MoonlightService;
    let moonlight_preferences = { context.state.read().await.moonlight_preferences.clone() };
    let moonlight_step_completed = server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::MoonlightConfigured,
    )
    .await;
    let (moonlight_host_address, moonlight_path_label) = if moonlight_step_completed {
        if let Some(cached_host) =
            load_moonlight_host_from_server_record(&context, instance.id).await
        {
            emit_step_skipped(
                &app,
                &context,
                OrchestrationState::ConfiguringMoonlight,
                "Skipping Moonlight local config patch",
                instance.id,
            )
            .await;

            let host_address = if cached_host.trim().is_empty() {
                wireguard_result.server_ip.clone()
            } else {
                cached_host
            };

            persist_moonlight_host_for_server(&context, instance.id, &host_address).await?;

            (host_address, "already-configured".to_string())
        } else {
            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringMoonlight,
                "Moonlight checkpoint is stale. Re-patching local config.",
                Some("No saved Moonlight host was found for this instance".to_string()),
                false,
            )
            .await;

            clear_server_steps(
                &context,
                instance.id,
                &[
                    ProvisionStepMarker::MoonlightConfigured,
                    ProvisionStepMarker::AwaitingPairPin,
                    ProvisionStepMarker::PairingCompleted,
                    ProvisionStepMarker::PostProvisionCompleted,
                ],
            )
            .await?;

            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringMoonlight,
                "Patching local Moonlight config",
                None,
                false,
            )
            .await;

            let path = moonlight
                .patch_local_config(
                    &wireguard_result.server_ip,
                    context.config.sunshine.port,
                    &moonlight_preferences,
                )
                .await?;
            mark_server_step_completed(
                &context,
                instance.id,
                ProvisionStepMarker::MoonlightConfigured,
                OrchestrationState::ConfiguringMoonlight,
                &instance.status,
                &instance.ssh_host,
                instance.ssh_port,
                Some(offer.id),
            )
            .await?;
            persist_moonlight_host_for_server(&context, instance.id, &wireguard_result.server_ip)
                .await?;

            (
                wireguard_result.server_ip.clone(),
                path.display().to_string(),
            )
        }
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringMoonlight,
            "Patching local Moonlight config",
            None,
            false,
        )
        .await;

        let path = moonlight
            .patch_local_config(
                &wireguard_result.server_ip,
                context.config.sunshine.port,
                &moonlight_preferences,
            )
            .await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::MoonlightConfigured,
            OrchestrationState::ConfiguringMoonlight,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            Some(offer.id),
        )
        .await?;
        persist_moonlight_host_for_server(&context, instance.id, &wireguard_result.server_ip)
            .await?;

        (
            wireguard_result.server_ip.clone(),
            path.display().to_string(),
        )
    };

    context
        .update_state(|state| {
            state.wireguard.server_ip = wireguard_result.server_ip.clone();
            state.wireguard.client_ip = wireguard_result.client_ip.clone();
            state.wireguard.server_public_key = wireguard_result.server_public_key.clone();
            state.wireguard.client_public_key = wireguard_result.client_public_key.clone();
            state.wireguard.config_path = wireguard_result.client_config_path.display().to_string();
            state.sunshine.configured = true;
            state.moonlight.configured = true;
            state.moonlight.host_address = moonlight_host_address.clone();
            state.orchestration_state = OrchestrationState::AwaitingPairPin;
            state.last_error = None;
        })
        .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::AwaitingPairPin,
        OrchestrationState::AwaitingPairPin,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        Some(offer.id),
    )
    .await?;

    {
        let mut pairing = context.pairing_context.write().await;
        *pairing = Some(PairingContext {
            host: instance.public_ip.clone(),
            port: instance.ssh_port,
            user: context.config.ssh_user.clone(),
        });
    }

    emit_transition(
        &app,
        &context,
        OrchestrationState::AwaitingPairPin,
        "Waiting for pairing PIN",
        Some(format!(
            "Type this IP in Moonlight (WireGuard): {} | WireGuard client config: {} | Moonlight config: {}",
            wireguard_result.server_ip,
            wireguard_result.client_config_path.display(),
            moonlight_path_label
        )),
        false,
    )
    .await;

    Ok(())
}

async fn run_existing_instance_orchestration(
    app: AppHandle,
    context: AppContext,
    instance_id: u64,
) -> AppResult<()> {
    ensure_not_cancelled(&context)?;

    context
        .update_state(|state| {
            state.instance.instance_id = Some(instance_id);
            state.last_error = None;
        })
        .await?;

    let _ = hydrate_state_from_server_record(&context, instance_id, true).await?;

    // Check saved steps and determine where to resume from
    let saved_steps = {
        let snapshot = context.state.read().await;
        snapshot
            .provisioned_servers
            .iter()
            .find(|r| r.instance_id == instance_id)
            .map(|r| r.steps.clone())
    };

    if let Some(steps) = saved_steps {
        let (resume_state, resume_msg) = determine_resume_step(&steps);
        info!(
            "Resuming instance {} from step: {:?} - {}",
            instance_id, resume_state, resume_msg
        );
        emit_transition(
            &app,
            &context,
            resume_state,
            &resume_msg,
            Some(format!(
                "Progress: SSH={}, NVIDIA={}, Sunshine={}, WireGuard={}, Moonlight={}",
                steps.ssh_connected,
                steps.nvidia_headless_configured,
                steps.sunshine_configured,
                steps.wireguard_configured,
                steps.moonlight_configured
            )),
            false,
        )
        .await;
    }

    let initial_state = context.state.read().await.clone();

    if try_launch_existing_moonlight_session(&app, &context, Some(instance_id)).await? {
        return Ok(());
    }

    let api_key = initial_state.credentials.vast_api_key.clone();
    if api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Missing Vast.ai API key. Complete onboarding first.".to_string(),
        ));
    }

    let app_data_dir = context.state_store.path().parent().ok_or_else(|| {
        AppError::State("Could not resolve app data directory from state file path".to_string())
    })?;

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );

    let offer_id = initial_state
        .instance
        .offer_id
        .or(initial_state.selected_offer.as_ref().map(|offer| offer.id));

    emit_transition(
        &app,
        &context,
        OrchestrationState::GeneratingSshKey,
        "Ensuring SSH keypair exists",
        None,
        false,
    )
    .await;

    let ssh_service = SshKeyService::new(initial_state.ssh.key_name.clone());
    let key_paths = ssh_service.ensure_keypair(app_data_dir).await?;
    ensure_private_key_path_exists(key_paths.private_key_path.as_path())?;
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::UploadingSshKeyToVast,
        "Syncing SSH key with Vast.ai",
        None,
        false,
    )
    .await;

    let uploaded = ssh_service
        .upload_public_key_if_missing(&vast, &key_paths.public_key_path)
        .await?;

    context
        .update_state(|state| {
            state.ssh.private_key_path = key_paths.private_key_path.display().to_string();
            state.ssh.public_key_path = key_paths.public_key_path.display().to_string();
            state.ssh.uploaded_to_vast = uploaded || state.ssh.uploaded_to_vast;
            state.last_error = None;
        })
        .await?;
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::WaitingForInstance,
        "Loading rented instance",
        Some(format!("Checking instance {instance_id}")),
        false,
    )
    .await;

    let instance_manager = InstanceManager {
        poll_interval: context.config.poll_interval,
        max_attempts: context.config.poll_max_attempts,
    };

    let mut instance = vast.get_instance(instance_id).await?;
    ensure_not_cancelled(&context)?;

    ensure_server_record(
        &context,
        instance.id,
        offer_id,
        &instance.ssh_host,
        instance.ssh_port,
        &instance.status,
        OrchestrationState::WaitingForInstance,
    )
    .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::SshKeyReady,
        OrchestrationState::GeneratingSshKey,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        offer_id,
    )
    .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::SshKeyUploadedToVast,
        OrchestrationState::UploadingSshKeyToVast,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        offer_id,
    )
    .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::InstanceCreated,
        OrchestrationState::CreatingInstance,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        offer_id,
    )
    .await?;

    if server_step_is_completed(&context, instance_id, ProvisionStepMarker::InstanceReady).await {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::WaitingForInstance,
            "Skipping rented instance readiness wait",
            instance_id,
        )
        .await;
    } else if !instance.ssh_ready() || instance.ssh_host.is_empty() {
        instance = instance_manager
            .wait_until_ssh_ready(
                &vast,
                instance_id,
                |attempt, current| {
                    info!(
                        "existing instance poll attempt {attempt} instance {} status {}",
                        current.id, current.status
                    );
                },
                || context.cancel_requested.load(Ordering::SeqCst),
            )
            .await?;

        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::InstanceReady,
            OrchestrationState::WaitingForInstance,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
    } else {
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::InstanceReady,
            OrchestrationState::WaitingForInstance,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    context
        .update_state(|state| {
            state.instance.instance_id = Some(instance.id);
            state.instance.offer_id = state
                .instance
                .offer_id
                .or(state.selected_offer.as_ref().map(|offer| offer.id));
            state.instance.status = instance.status.clone();
            state.instance.ssh_host = instance.ssh_host.clone();
            state.instance.ssh_port = instance.ssh_port;
            state.instance.ssh_user = context.config.ssh_user.clone();
            state.instance.ssh_command = instance.ssh_command.clone();
        })
        .await?;

    ensure_server_record(
        &context,
        instance.id,
        offer_id,
        &instance.ssh_host,
        instance.ssh_port,
        &instance.status,
        OrchestrationState::ConnectingSsh,
    )
    .await?;

    instance =
        verify_instance_reserved_in_account(&app, &context, &vast, instance.id, offer_id).await?;
    ensure_instance_is_vm_runtime(&instance)?;
    ensure_not_cancelled(&context)?;

    // ssh_user: who we authenticate as over SSH (typically root on cloud VMs)
    // target_user: who Sunshine/Xorg run as (unprivileged user)
    let ssh_user = sanitize_ssh_user(&{
        let state = context.state.read().await;
        if state.ssh.ssh_username.is_empty() {
            context.config.ssh_user.clone()
        } else {
            state.ssh.ssh_username.clone()
        }
    });
    let target_user = sanitize_ssh_user(&context.config.audio_target_user);
    let mut remote = RemoteExec {
        ssh_user,
        ssh_host: instance.public_ip.clone(),
        ssh_port: instance.ssh_port,
        private_key_path: key_paths.private_key_path.display().to_string(),
    };

    if server_step_is_completed(&context, instance.id, ProvisionStepMarker::SshConnected).await {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::ConnectingSsh,
            "Skipping SSH connectivity check",
            instance.id,
        )
        .await;
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConnectingSsh,
            "Checking SSH connectivity",
            Some(format!("{}:{}", instance.public_ip, instance.ssh_port)),
            false,
        )
        .await;

        wait_for_ssh_acceptance(&app, &context, &remote, &vast, instance.id).await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::SshConnected,
            OrchestrationState::ConnectingSsh,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    emit_transition(
        &app,
        &context,
        OrchestrationState::ConfiguringNvidiaHeadless,
        "Configuring NVIDIA headless streaming",
        None,
        false,
    )
    .await;

    let nvidia = NvidiaHeadlessService;
    if server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::NvidiaHeadlessConfigured,
    )
    .await
    {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::ConfiguringNvidiaHeadless,
            "Skipping NVIDIA headless setup",
            instance.id,
        )
        .await;
    } else {
        if let Err(error) = nvidia.setup_and_validate(&remote).await {
            let diagnostics = nvidia.collect_diagnostics(&remote).await.ok();
            let diag_summary = diagnostics
                .map(|diag| {
                    diag.commands
                        .into_iter()
                        .map(|(command, output)| format!("{command} -> {}", output.status_code))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_else(|| "no diagnostics collected".to_string());

            return Err(AppError::Provisioning(format!(
                "{error}. Diagnostics: {diag_summary}"
            )));
        }

        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::NvidiaHeadlessConfigured,
            OrchestrationState::ConfiguringNvidiaHeadless,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
    }

    ensure_post_nvidia_reboot(&app, &context, &vast, &mut instance, &mut remote, offer_id).await?;
    ensure_not_cancelled(&context)?;

    let sunshine = SunshineService {
        defaults: context.config.sunshine.clone(),
    };
    let sunshine_step_completed = server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::SunshineConfigured,
    )
    .await;
    let mut should_install_sunshine = !sunshine_step_completed;
    if sunshine_step_completed {
        match sunshine.verify_resume_health(&remote, &target_user).await {
            Ok(()) => {
                emit_step_skipped(
                    &app,
                    &context,
                    OrchestrationState::ConfiguringSunshine,
                    "Skipping Sunshine install/config",
                    instance.id,
                )
                .await;
            }
            Err(error) => {
                warn!(
                    "Saved Sunshine state drifted for existing instance {}. Forcing full reconfiguration. {}",
                    instance.id, error
                );
                emit_transition(
                    &app,
                    &context,
                    OrchestrationState::ConfiguringSunshine,
                    "Saved Sunshine state is stale. Reconfiguring Sunshine.",
                    Some("Remote Sunshine preflight failed; rerunning full setup".to_string()),
                    false,
                )
                .await;
                should_install_sunshine = true;
            }
        }
    }
    if should_install_sunshine {
        let moonlight_preferences = { context.state.read().await.moonlight_preferences.clone() };
        let display_profile = crate::services::sunshine::DisplayProfile::from_moonlight_prefs(
            moonlight_preferences.width,
            moonlight_preferences.height,
            moonlight_preferences.fps,
        );
        info!(
            "Sunshine display profile (existing instance): {}x{} @ {}Hz ({} FPS x2)",
            display_profile.width,
            display_profile.height,
            display_profile.virtual_hz(),
            display_profile.fps
        );
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Installing and configuring Sunshine",
            Some(format!(
                "Display: {}x{} @ {}Hz",
                display_profile.width,
                display_profile.height,
                display_profile.virtual_hz()
            )),
            false,
        )
        .await;
        sunshine
            .install_and_configure(&remote, &target_user, display_profile)
            .await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::SunshineConfigured,
            OrchestrationState::ConfiguringSunshine,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
    }
    ensure_not_cancelled(&context)?;

    let audio_latency = AudioLatencyService::from_config(&context.config);
    if server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::LowLatencyAudioConfigured,
    )
    .await
    {
        emit_step_skipped(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Skipping low-latency audio setup",
            instance.id,
        )
        .await;
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Applying low-latency PipeWire/WirePlumber audio profile",
            Some(format!(
                "target_user={} profile={}",
                context.config.audio_target_user, context.config.audio_profile
            )),
            false,
        )
        .await;

        let audio_result = audio_latency.configure(&remote).await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::LowLatencyAudioConfigured,
            OrchestrationState::ConfiguringSunshine,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;

        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringSunshine,
            "Low-latency audio profile configured",
            Some(summarize_verification_output(
                &audio_result.verification_output,
            )),
            false,
        )
        .await;
    }
    ensure_not_cancelled(&context)?;

    match vast.get_instance(instance.id).await {
        Ok(refreshed_instance) => {
            instance.public_ip = refreshed_instance.public_ip;
            instance.ssh_host = refreshed_instance.ssh_host;
            instance.wireguard_port = refreshed_instance.wireguard_port;
            info!(
                "Refreshed existing-instance networking before WireGuard: public_ip={} ssh_host={} wireguard_port={}",
                instance.public_ip, instance.ssh_host, instance.wireguard_port
            );
        }
        Err(error) => {
            warn!(
                "Failed to refresh existing-instance networking before WireGuard; using cached values: {}",
                error
            );
        }
    }

    let wireguard = WireGuardService {
        defaults: context.config.wireguard.clone(),
    };
    let endpoint_host = instance.wireguard_endpoint_host();
    let endpoint_port = instance.wireguard_port;
    if endpoint_port == 0 {
        return Err(AppError::Provisioning(format!(
            "Instance {} does not expose 51820/udp on Vast. Pick a VM-enabled offer with direct UDP ports.",
            instance.id
        )));
    }

    let wireguard_step_completed = server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::WireguardConfigured,
    )
    .await;

    let wireguard_result: WireGuardProvisionResult = if wireguard_step_completed {
        if let Some(cached) = load_wireguard_result_from_server_record(&context, instance.id).await
        {
            emit_step_skipped(
                &app,
                &context,
                OrchestrationState::ConfiguringWireGuard,
                "Skipping WireGuard setup",
                instance.id,
            )
            .await;

            context
                .update_state(|state| {
                    state.wireguard.server_ip = cached.server_ip.clone();
                    state.wireguard.client_ip = cached.client_ip.clone();
                    state.wireguard.server_public_key = cached.server_public_key.clone();
                    state.wireguard.client_public_key = cached.client_public_key.clone();
                    state.wireguard.config_path = cached.client_config_path.display().to_string();
                    state.sunshine.configured = true;
                })
                .await?;

            cached
        } else {
            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringWireGuard,
                "WireGuard checkpoint is stale. Reconfiguring tunnel.",
                Some("No saved WireGuard artifacts were found for this instance".to_string()),
                false,
            )
            .await;

            clear_server_steps(
                &context,
                instance.id,
                &[
                    ProvisionStepMarker::WireguardConfigured,
                    ProvisionStepMarker::MoonlightConfigured,
                    ProvisionStepMarker::AwaitingPairPin,
                    ProvisionStepMarker::PairingCompleted,
                ],
            )
            .await?;

            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringWireGuard,
                "Setting up WireGuard tunnel",
                None,
                false,
            )
            .await;

            let result = wireguard
                .configure(
                    &remote,
                    app_data_dir,
                    &endpoint_host,
                    endpoint_port,
                    WireGuardProvisionMode::ReinitializeExisting,
                )
                .await?;
            mark_server_step_completed(
                &context,
                instance.id,
                ProvisionStepMarker::WireguardConfigured,
                OrchestrationState::ConfiguringWireGuard,
                &instance.status,
                &instance.ssh_host,
                instance.ssh_port,
                offer_id,
            )
            .await?;
            persist_wireguard_result_for_server(&context, instance.id, &result).await?;

            context
                .update_state(|state| {
                    state.wireguard.server_ip = result.server_ip.clone();
                    state.wireguard.client_ip = result.client_ip.clone();
                    state.wireguard.server_public_key = result.server_public_key.clone();
                    state.wireguard.client_public_key = result.client_public_key.clone();
                    state.wireguard.config_path = result.client_config_path.display().to_string();
                    state.sunshine.configured = true;
                })
                .await?;

            result
        }
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringWireGuard,
            "Setting up WireGuard tunnel",
            None,
            false,
        )
        .await;

        let result = wireguard
            .configure(
                &remote,
                app_data_dir,
                &endpoint_host,
                endpoint_port,
                WireGuardProvisionMode::ReinitializeExisting,
            )
            .await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::WireguardConfigured,
            OrchestrationState::ConfiguringWireGuard,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
        persist_wireguard_result_for_server(&context, instance.id, &result).await?;

        context
            .update_state(|state| {
                state.wireguard.server_ip = result.server_ip.clone();
                state.wireguard.client_ip = result.client_ip.clone();
                state.wireguard.server_public_key = result.server_public_key.clone();
                state.wireguard.client_public_key = result.client_public_key.clone();
                state.wireguard.config_path = result.client_config_path.display().to_string();
                state.sunshine.configured = true;
            })
            .await?;

        result
    };
    ensure_not_cancelled(&context)?;

    if !wireguard_step_completed {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringWireGuard,
            "Applying local WireGuard tunnel",
            Some(
                "Replacing any previous local tunnel interface with the newly provisioned config"
                    .to_string(),
            ),
            false,
        )
        .await;

        reconnect_local_wireguard_client(&wireguard_result.client_config_path)?;
        ensure_not_cancelled(&context)?;
    }

    let moonlight = MoonlightService;
    let moonlight_preferences = { context.state.read().await.moonlight_preferences.clone() };
    let moonlight_step_completed = server_step_is_completed(
        &context,
        instance.id,
        ProvisionStepMarker::MoonlightConfigured,
    )
    .await;
    let (moonlight_host_address, moonlight_path_label) = if moonlight_step_completed {
        if let Some(cached_host) =
            load_moonlight_host_from_server_record(&context, instance.id).await
        {
            emit_step_skipped(
                &app,
                &context,
                OrchestrationState::ConfiguringMoonlight,
                "Skipping Moonlight local config patch",
                instance.id,
            )
            .await;

            let host_address = if cached_host.trim().is_empty() {
                wireguard_result.server_ip.clone()
            } else {
                cached_host
            };

            persist_moonlight_host_for_server(&context, instance.id, &host_address).await?;

            (host_address, "already-configured".to_string())
        } else {
            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringMoonlight,
                "Moonlight checkpoint is stale. Re-patching local config.",
                Some("No saved Moonlight host was found for this instance".to_string()),
                false,
            )
            .await;

            clear_server_steps(
                &context,
                instance.id,
                &[
                    ProvisionStepMarker::MoonlightConfigured,
                    ProvisionStepMarker::AwaitingPairPin,
                    ProvisionStepMarker::PairingCompleted,
                    ProvisionStepMarker::PostProvisionCompleted,
                ],
            )
            .await?;

            emit_transition(
                &app,
                &context,
                OrchestrationState::ConfiguringMoonlight,
                "Patching local Moonlight config",
                None,
                false,
            )
            .await;

            let path = moonlight
                .patch_local_config(
                    &wireguard_result.server_ip,
                    context.config.sunshine.port,
                    &moonlight_preferences,
                )
                .await?;
            mark_server_step_completed(
                &context,
                instance.id,
                ProvisionStepMarker::MoonlightConfigured,
                OrchestrationState::ConfiguringMoonlight,
                &instance.status,
                &instance.ssh_host,
                instance.ssh_port,
                offer_id,
            )
            .await?;
            persist_moonlight_host_for_server(&context, instance.id, &wireguard_result.server_ip)
                .await?;

            (
                wireguard_result.server_ip.clone(),
                path.display().to_string(),
            )
        }
    } else {
        emit_transition(
            &app,
            &context,
            OrchestrationState::ConfiguringMoonlight,
            "Patching local Moonlight config",
            None,
            false,
        )
        .await;

        let path = moonlight
            .patch_local_config(
                &wireguard_result.server_ip,
                context.config.sunshine.port,
                &moonlight_preferences,
            )
            .await?;
        mark_server_step_completed(
            &context,
            instance.id,
            ProvisionStepMarker::MoonlightConfigured,
            OrchestrationState::ConfiguringMoonlight,
            &instance.status,
            &instance.ssh_host,
            instance.ssh_port,
            offer_id,
        )
        .await?;
        persist_moonlight_host_for_server(&context, instance.id, &wireguard_result.server_ip)
            .await?;

        (
            wireguard_result.server_ip.clone(),
            path.display().to_string(),
        )
    };

    context
        .update_state(|state| {
            state.wireguard.server_ip = wireguard_result.server_ip.clone();
            state.wireguard.client_ip = wireguard_result.client_ip.clone();
            state.wireguard.server_public_key = wireguard_result.server_public_key.clone();
            state.wireguard.client_public_key = wireguard_result.client_public_key.clone();
            state.wireguard.config_path = wireguard_result.client_config_path.display().to_string();
            state.sunshine.configured = true;
            state.moonlight.configured = true;
            state.moonlight.host_address = moonlight_host_address.clone();
            state.orchestration_state = OrchestrationState::AwaitingPairPin;
            state.last_error = None;
        })
        .await?;
    mark_server_step_completed(
        &context,
        instance.id,
        ProvisionStepMarker::AwaitingPairPin,
        OrchestrationState::AwaitingPairPin,
        &instance.status,
        &instance.ssh_host,
        instance.ssh_port,
        offer_id,
    )
    .await?;

    {
        let mut pairing = context.pairing_context.write().await;
        *pairing = Some(PairingContext {
            host: instance.public_ip.clone(),
            port: instance.ssh_port,
            user: context.config.ssh_user.clone(),
        });
    }

    emit_transition(
        &app,
        &context,
        OrchestrationState::AwaitingPairPin,
        "Waiting for pairing PIN",
        Some(format!(
            "Type this IP in Moonlight (WireGuard): {} | WireGuard client config: {} | Moonlight config: {}",
            wireguard_result.server_ip,
            wireguard_result.client_config_path.display(),
            moonlight_path_label
        )),
        false,
    )
    .await;

    Ok(())
}

async fn wait_for_ssh_acceptance(
    app: &AppHandle,
    context: &AppContext,
    remote: &RemoteExec,
    vast: &VastApiClient,
    instance_id: u64,
) -> AppResult<()> {
    ensure_private_key_path_exists(Path::new(&remote.private_key_path))?;

    let passphrase = {
        let state = context.state.read().await;
        state.credentials.app_password.clone()
    };

    if passphrase.is_empty() {
        return Err(AppError::InvalidInput(
            "Platform password is required to unlock SSH key".to_string(),
        ));
    }

    let ssh_service = SshKeyService::new("nolandConnectSSH");
    ssh_service
        .load_key_into_agent(Path::new(&remote.private_key_path), &passphrase)
        .await?;

    for attempt in 1..=context.config.ssh_connect_probe_attempts {
        if context.cancel_requested.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled);
        }

        let probe = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("echo connected", Duration::from_secs(20))
            })
            .await
            .map_err(|error| AppError::Command(format!("ssh check join failure: {error}")))??
        };

        if probe.status_code == 0 {
            return Ok(());
        }

        let mut reservation_suffix = String::new();
        if looks_like_ssh_connectivity_refusal(&probe.stderr) {
            match reservation_snapshot_from_list(vast, instance_id).await {
                Ok(Some(instance)) => {
                    if is_inactive_instance_status(&instance.status) {
                        return Err(AppError::Provisioning(format!(
                            "SSH is still refusing connections and instance {instance_id} is inactive in your account (status: {})",
                            instance.status
                        )));
                    }

                    reservation_suffix = format!(
                        " | reservation check: instance {} still listed as {}",
                        instance.id, instance.status
                    );
                }
                Ok(None) => {
                    return Err(AppError::Provisioning(format!(
                        "SSH is refusing connections and instance {instance_id} is no longer listed under your Vast account reservations"
                    )));
                }
                Err(error) => {
                    reservation_suffix =
                        format!(" | warning: reservation re-check failed ({error})");
                }
            }
        }

        let details = if probe.stderr.trim().is_empty() {
            format!(
                "Attempt {attempt}/{}; ssh -p {} {}@{}; retrying in {}s{}",
                context.config.ssh_connect_probe_attempts,
                remote.ssh_port,
                remote.ssh_user,
                remote.ssh_host,
                context.config.ssh_connect_probe_interval.as_secs(),
                reservation_suffix
            )
        } else {
            format!(
                "Attempt {attempt}/{}; ssh -p {} {}@{}; error: {}; retrying in {}s{}",
                context.config.ssh_connect_probe_attempts,
                remote.ssh_port,
                remote.ssh_user,
                remote.ssh_host,
                probe.stderr.trim(),
                context.config.ssh_connect_probe_interval.as_secs(),
                reservation_suffix
            )
        };

        emit_transition(
            app,
            context,
            OrchestrationState::ConnectingSsh,
            "VM is not yet accepting SSH connections",
            Some(details),
            false,
        )
        .await;

        if attempt < context.config.ssh_connect_probe_attempts {
            sleep(context.config.ssh_connect_probe_interval).await;
        }
    }

    match reservation_snapshot_from_list(vast, instance_id).await {
        Ok(Some(instance)) => Err(AppError::Timeout(format!(
            "Instance never became SSH-connectable after readiness polling (instance status: {})",
            instance.status
        ))),
        Ok(None) => Err(AppError::Provisioning(format!(
            "Instance {instance_id} never became SSH-connectable and is no longer listed under your Vast account reservations"
        ))),
        Err(error) => Err(AppError::Timeout(format!(
            "Instance never became SSH-connectable after readiness polling; final reservation re-check failed: {error}"
        ))),
    }
}

async fn ensure_post_nvidia_reboot(
    app: &AppHandle,
    context: &AppContext,
    vast: &VastApiClient,
    instance: &mut crate::models::vast::VastInstance,
    remote: &mut RemoteExec,
    offer_id: Option<u64>,
) -> AppResult<()> {
    if server_step_is_completed(
        context,
        instance.id,
        ProvisionStepMarker::PostNvidiaRebootCompleted,
    )
    .await
    {
        emit_step_skipped(
            app,
            context,
            OrchestrationState::ConnectingSsh,
            "Skipping post-NVIDIA reboot",
            instance.id,
        )
        .await;
        return Ok(());
    }

    emit_transition(
        app,
        context,
        OrchestrationState::ConnectingSsh,
        "Rebooting instance to finalize NVIDIA/Xorg setup",
        Some("Instance will disconnect briefly, then auto-reconnect".to_string()),
        false,
    )
    .await;

    let reboot_output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || {
            remote.ssh(
                "sudo bash -lc 'nohup sh -c \"sleep 2; reboot\" >/dev/null 2>&1 &'",
                Duration::from_secs(20),
            )
        })
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))??
    };

    if reboot_output.status_code != 0 {
        warn!(
            "Reboot command returned non-zero (continuing): stdout: {} | stderr: {}",
            reboot_output.stdout.trim(),
            reboot_output.stderr.trim()
        );
    }

    sleep(Duration::from_secs(8)).await;

    const REBOOT_RECONNECT_ATTEMPTS: usize = 36;
    const REBOOT_RECONNECT_INTERVAL: Duration = Duration::from_secs(10);

    for attempt in 1..=REBOOT_RECONNECT_ATTEMPTS {
        if context.cancel_requested.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled);
        }

        match vast.get_instance(instance.id).await {
            Ok(refreshed) => {
                if !refreshed.public_ip.trim().is_empty() {
                    instance.public_ip = refreshed.public_ip.clone();
                }
                if !refreshed.ssh_host.trim().is_empty() {
                    instance.ssh_host = refreshed.ssh_host.clone();
                }
                if refreshed.ssh_port > 0 {
                    instance.ssh_port = refreshed.ssh_port;
                }
                if !refreshed.status.trim().is_empty() {
                    instance.status = refreshed.status.clone();
                }

                remote.ssh_host = if !instance.public_ip.trim().is_empty() {
                    instance.public_ip.clone()
                } else {
                    instance.ssh_host.clone()
                };
                remote.ssh_port = instance.ssh_port;

                let probe = {
                    let remote = remote.clone();
                    tokio::task::spawn_blocking(move || {
                        remote.ssh("echo reboot-online", Duration::from_secs(15))
                    })
                    .await
                    .map_err(|error| {
                        AppError::Command(format!("reboot probe join failure: {error}"))
                    })??
                };

                if probe.status_code == 0 {
                    // Wait for systemd to finish booting before continuing
                    // Prevents race conditions where Xorg service start fails because systemd is mid-boot
                    const SYSTEM_STATE_ATTEMPTS: usize = 30;
                    const SYSTEM_STATE_INTERVAL: Duration = Duration::from_secs(2);
                    let mut system_ready = false;
                    for sys_attempt in 1..=SYSTEM_STATE_ATTEMPTS {
                        let system_state = {
                            let remote = remote.clone();
                            tokio::task::spawn_blocking(move || {
                                remote.ssh(
                                    "systemctl is-system-running 2>/dev/null",
                                    Duration::from_secs(10),
                                )
                            })
                            .await
                            .map_err(|error| {
                                AppError::Command(format!(
                                    "system-state probe join failure: {error}"
                                ))
                            })??
                        };
                        let state = system_state.stdout.trim();
                        if state == "running" || state == "degraded" {
                            system_ready = true;
                            break;
                        }
                        emit_transition(
                            app,
                            context,
                            OrchestrationState::ConnectingSsh,
                            "Waiting for system to finish booting",
                            Some(format!(
                                "system state: {} (attempt {}/{})",
                                state, sys_attempt, SYSTEM_STATE_ATTEMPTS
                            )),
                            false,
                        )
                        .await;
                        sleep(SYSTEM_STATE_INTERVAL).await;
                    }
                    if !system_ready {
                        warn!(
                            "System did not reach 'running' state after reboot, continuing anyway"
                        );
                    }

                    mark_server_step_completed(
                        context,
                        instance.id,
                        ProvisionStepMarker::PostNvidiaRebootCompleted,
                        OrchestrationState::ConnectingSsh,
                        &instance.status,
                        &instance.ssh_host,
                        instance.ssh_port,
                        offer_id,
                    )
                    .await?;

                    emit_transition(
                        app,
                        context,
                        OrchestrationState::ConnectingSsh,
                        "Instance reboot completed and SSH is back online",
                        Some(format!(
                            "{}:{} (attempt {}/{})",
                            remote.ssh_host, remote.ssh_port, attempt, REBOOT_RECONNECT_ATTEMPTS
                        )),
                        false,
                    )
                    .await;

                    return Ok(());
                }

                emit_transition(
                    app,
                    context,
                    OrchestrationState::ConnectingSsh,
                    "Waiting for SSH after reboot",
                    Some(format!(
                        "Attempt {}/{} | status={} | ssh={}:{} | error={} ",
                        attempt,
                        REBOOT_RECONNECT_ATTEMPTS,
                        instance.status,
                        remote.ssh_host,
                        remote.ssh_port,
                        probe.stderr.trim()
                    )),
                    false,
                )
                .await;
            }
            Err(error) => {
                emit_transition(
                    app,
                    context,
                    OrchestrationState::WaitingForInstance,
                    "Waiting for Vast instance metadata after reboot",
                    Some(format!(
                        "Attempt {}/{} failed to refresh instance {}: {}",
                        attempt, REBOOT_RECONNECT_ATTEMPTS, instance.id, error
                    )),
                    false,
                )
                .await;
            }
        }

        if attempt < REBOOT_RECONNECT_ATTEMPTS {
            sleep(REBOOT_RECONNECT_INTERVAL).await;
        }
    }

    Err(AppError::Timeout(format!(
        "Timed out waiting for instance {} to reconnect after reboot",
        instance.id
    )))
}

fn is_no_such_ask_error(error: &AppError) -> bool {
    match error {
        AppError::Api(message) | AppError::NotFound(message) => {
            let normalized = message.to_ascii_lowercase();
            normalized.contains("no_such_ask")
                || normalized.contains("instance type by id")
                || normalized.contains("not available")
        }
        _ => false,
    }
}

fn looks_like_ssh_connectivity_refusal(stderr: &str) -> bool {
    let normalized = stderr.to_ascii_lowercase();
    normalized.contains("connection refused")
        || normalized.contains("connection reset")
        || normalized.contains("connection timed out")
        || normalized.contains("no route to host")
}

fn is_inactive_instance_status(status: &str) -> bool {
    let normalized = status.to_ascii_lowercase();
    normalized.contains("destroy")
        || normalized.contains("stopped")
        || normalized.contains("exited")
}

async fn reservation_snapshot_from_list(
    vast: &VastApiClient,
    instance_id: u64,
) -> AppResult<Option<crate::models::vast::VastInstance>> {
    Ok(vast
        .list_instances()
        .await?
        .into_iter()
        .find(|candidate| candidate.id == instance_id))
}

fn find_active_rented_instance(
    instances: Vec<crate::models::vast::VastInstance>,
) -> Option<crate::models::vast::VastInstance> {
    instances.into_iter().find(|instance| {
        let status = instance.status.to_ascii_lowercase();
        !status.contains("destroy")
            && !status.contains("stopped")
            && !status.contains("exited")
            && !instance.ssh_host.is_empty()
    })
}

async fn verify_instance_reserved_in_account(
    app: &AppHandle,
    context: &AppContext,
    vast: &VastApiClient,
    instance_id: u64,
    offer_id: Option<u64>,
) -> AppResult<crate::models::vast::VastInstance> {
    const VERIFY_ATTEMPTS: usize = 6;
    const VERIFY_RETRY_DELAY: Duration = Duration::from_secs(5);

    emit_transition(
        app,
        context,
        OrchestrationState::VerifyingReservation,
        "Verifying reservation ownership in Vast account",
        Some(format!("Checking instance {instance_id}")),
        false,
    )
    .await;

    let mut last_get_instance_snapshot: Option<crate::models::vast::VastInstance> = None;
    let mut last_lookup_error: Option<String> = None;
    for attempt in 1..=VERIFY_ATTEMPTS {
        ensure_not_cancelled(context)?;

        match vast.get_instance(instance_id).await {
            Ok(snapshot) => {
                last_get_instance_snapshot = Some(snapshot);
                last_lookup_error = None;
            }
            Err(AppError::NotFound(_)) => {
                last_get_instance_snapshot = None;
                last_lookup_error = Some(format!(
                    "get_instance did not find instance {} on attempt {}/{}",
                    instance_id, attempt, VERIFY_ATTEMPTS
                ));
            }
            Err(error) => {
                last_lookup_error = Some(format!(
                    "get_instance lookup failed on attempt {}/{}: {}",
                    attempt, VERIFY_ATTEMPTS, error
                ));
            }
        }

        let listed = reservation_snapshot_from_list(vast, instance_id).await?;
        if let Some(mut instance) = listed {
            if instance.image_runtype.trim().is_empty() {
                if let Some(snapshot) = &last_get_instance_snapshot {
                    if snapshot.id == instance_id {
                        instance.image_runtype = snapshot.image_runtype.clone();
                        instance.hosting_type = snapshot.hosting_type.clone();
                    }
                }
            }

            if is_inactive_instance_status(&instance.status) {
                return Err(AppError::Provisioning(format!(
                    "Instance {instance_id} exists in your account but is not active (status: {})",
                    instance.status
                )));
            }

            let status = instance.status.clone();
            let ssh_host = instance.ssh_host.clone();
            let ssh_port = instance.ssh_port;
            let ssh_command = instance.ssh_command.clone();
            context
                .update_state(move |state| {
                    state.instance.instance_id = Some(instance_id);
                    state.instance.offer_id = offer_id.or(state.instance.offer_id);
                    state.instance.status = status.clone();
                    state.instance.ssh_host = ssh_host.clone();
                    state.instance.ssh_port = ssh_port;
                    state.instance.ssh_user = context.config.ssh_user.clone();
                    state.instance.ssh_command = ssh_command.clone();
                    state.last_error = None;
                })
                .await?;

            ensure_server_record(
                context,
                instance_id,
                offer_id,
                &instance.ssh_host,
                instance.ssh_port,
                &instance.status,
                OrchestrationState::VerifyingReservation,
            )
            .await?;

            emit_transition(
                app,
                context,
                OrchestrationState::VerifyingReservation,
                "Reservation confirmed in your Vast account",
                Some(format!(
                    "Instance {} status {} ssh {}:{}",
                    instance.id, instance.status, instance.ssh_host, instance.ssh_port
                )),
                false,
            )
            .await;

            return Ok(instance);
        }

        if attempt < VERIFY_ATTEMPTS {
            let mut details = format!(
                "Attempt {attempt}/{VERIFY_ATTEMPTS}; retrying in {}s",
                VERIFY_RETRY_DELAY.as_secs()
            );
            if let Some(lookup_error) = &last_lookup_error {
                details.push_str(" | ");
                details.push_str(lookup_error);
            }

            emit_transition(
                app,
                context,
                OrchestrationState::VerifyingReservation,
                "Instance not yet visible in your reserved instances list",
                Some(details),
                false,
            )
            .await;
            sleep(VERIFY_RETRY_DELAY).await;
        }
    }

    let base_diagnostic = if let Some(snapshot) = last_get_instance_snapshot {
        format!(
            "Direct lookup saw status '{}' at {}:{} but it never appeared in the reserved instances list",
            snapshot.status, snapshot.ssh_host, snapshot.ssh_port
        )
    } else {
        "Direct lookup by id also did not return this instance".to_string()
    };

    let diagnostic = if let Some(lookup_error) = last_lookup_error {
        format!("{} | Last lookup detail: {}", base_diagnostic, lookup_error)
    } else {
        base_diagnostic
    };

    Err(AppError::Provisioning(format!(
        "Instance {instance_id} was not confirmed under your Vast account reservations after {VERIFY_ATTEMPTS} checks. {diagnostic}"
    )))
}

fn ensure_instance_is_vm_runtime(instance: &crate::models::vast::VastInstance) -> AppResult<()> {
    if instance.is_vm_runtime() {
        return Ok(());
    }

    let runtime = if instance.image_runtype.trim().is_empty() {
        "unknown"
    } else {
        instance.image_runtype.trim()
    };

    let hosting_type = if instance.hosting_type.trim().is_empty() {
        "unknown"
    } else {
        instance.hosting_type.trim()
    };

    Err(AppError::Provisioning(format!(
        "Instance {} is not running as a VM (runtime='{}', hosting_type='{}'). Noland now requires VM runtime for streaming reliability. Please choose a VM-backed offer and recreate the instance.",
        instance.id, runtime, hosting_type
    )))
}

async fn try_launch_existing_moonlight_session(
    app: &AppHandle,
    context: &AppContext,
    instance_id: Option<u64>,
) -> AppResult<bool> {
    let snapshot = context.state.read().await.clone();
    let target_instance_id = instance_id.or(snapshot.instance.instance_id);
    let (pairing_completed, post_provision_completed) = target_instance_id
        .and_then(|id| {
            snapshot
                .provisioned_servers
                .iter()
                .find(|record| record.instance_id == id)
                .map(|record| {
                    (
                        record.steps.pairing_completed,
                        record.steps.post_provision_completed,
                    )
                })
        })
        .unwrap_or((false, false));

    let has_pin_in_memory = {
        let pin_memory = context.pairing_pin_in_memory.read().await;
        pin_memory
            .as_deref()
            .map(is_valid_pairing_pin)
            .unwrap_or(false)
    };

    let ready = snapshot.moonlight.configured
        && !snapshot.moonlight.host_address.trim().is_empty()
        && snapshot.sunshine.configured
        && !snapshot.wireguard.server_ip.trim().is_empty()
        && pairing_completed
        && post_provision_completed
        && has_pin_in_memory;

    if !ready {
        return Ok(false);
    }

    if let Some(id) = instance_id {
        if let Some(current_id) = snapshot.instance.instance_id {
            if current_id != id {
                return Ok(false);
            }
        }
    }

    let moonlight = MoonlightService;
    moonlight.launch_native_client()?;

    context
        .update_state(|state| {
            if let Some(id) = instance_id {
                state.instance.instance_id = Some(id);
            }
            state.orchestration_state = OrchestrationState::Ready;
            state.last_error = None;
        })
        .await?;

    if let Some(id) = instance_id {
        ensure_server_record(
            context,
            id,
            snapshot.instance.offer_id,
            &snapshot.instance.ssh_host,
            snapshot.instance.ssh_port,
            &snapshot.instance.status,
            OrchestrationState::Ready,
        )
        .await?;

        if snapshot.sunshine.configured {
            mark_server_step_completed(
                context,
                id,
                ProvisionStepMarker::SunshineConfigured,
                OrchestrationState::ConfiguringSunshine,
                &snapshot.instance.status,
                &snapshot.instance.ssh_host,
                snapshot.instance.ssh_port,
                snapshot.instance.offer_id,
            )
            .await?;
        }
        if !snapshot.wireguard.server_ip.trim().is_empty() {
            mark_server_step_completed(
                context,
                id,
                ProvisionStepMarker::WireguardConfigured,
                OrchestrationState::ConfiguringWireGuard,
                &snapshot.instance.status,
                &snapshot.instance.ssh_host,
                snapshot.instance.ssh_port,
                snapshot.instance.offer_id,
            )
            .await?;

            persist_wireguard_result_for_server(
                context,
                id,
                &WireGuardProvisionResult {
                    server_ip: snapshot.wireguard.server_ip.clone(),
                    client_ip: snapshot.wireguard.client_ip.clone(),
                    server_public_key: snapshot.wireguard.server_public_key.clone(),
                    client_public_key: snapshot.wireguard.client_public_key.clone(),
                    client_config_path: PathBuf::from(snapshot.wireguard.config_path.clone()),
                },
            )
            .await?;
        }
        if snapshot.moonlight.configured {
            mark_server_step_completed(
                context,
                id,
                ProvisionStepMarker::MoonlightConfigured,
                OrchestrationState::ConfiguringMoonlight,
                &snapshot.instance.status,
                &snapshot.instance.ssh_host,
                snapshot.instance.ssh_port,
                snapshot.instance.offer_id,
            )
            .await?;

            let host = if snapshot.moonlight.host_address.trim().is_empty() {
                snapshot.wireguard.server_ip.clone()
            } else {
                snapshot.moonlight.host_address.clone()
            };
            if !host.trim().is_empty() {
                persist_moonlight_host_for_server(context, id, &host).await?;
            }
        }
    }

    emit_transition(
        app,
        context,
        OrchestrationState::Ready,
        "Server already configured. Opening native Moonlight client.",
        Some(format!(
            "Host {} via WireGuard {}",
            snapshot.moonlight.host_address, snapshot.wireguard.server_ip
        )),
        false,
    )
    .await;

    Ok(true)
}

fn summarize_verification_output(output: &str) -> String {
    const MAX_LINES: usize = 18;
    const MAX_CHARS: usize = 2200;

    let mut collected = String::new();
    let mut lines = 0usize;

    for line in output.lines() {
        if lines >= MAX_LINES || collected.len() >= MAX_CHARS {
            break;
        }

        collected.push_str(line);
        collected.push('\n');
        lines += 1;
    }

    if output.lines().count() > MAX_LINES || output.len() > MAX_CHARS {
        collected.push_str("... (verification output truncated)");
    }

    if collected.trim().is_empty() {
        "Audio profile applied. Verification output was empty.".to_string()
    } else {
        collected
    }
}

fn is_valid_pairing_pin(pin: &str) -> bool {
    let trimmed = pin.trim();
    trimmed.len() >= 4 && trimmed.chars().all(|character| character.is_ascii_digit())
}

#[derive(Clone, Copy)]
enum SunshinePairingMode {
    SunshineCli,
    SunshinePairPin,
    ManualWebUi,
}

async fn detect_sunshine_pairing_mode(remote: &RemoteExec) -> AppResult<SunshinePairingMode> {
    let detect_command = "if command -v sunshine-cli >/dev/null 2>&1 && sunshine-cli --help 2>/dev/null | grep -qi pair; then echo CLI; elif command -v sunshine >/dev/null 2>&1 && sunshine --help 2>/dev/null | grep -q -- '--pair-pin'; then echo PAIR_PIN; else echo MANUAL; fi";

    let output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh(detect_command, Duration::from_secs(20)))
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
    };

    let mode = output.stdout.trim();
    Ok(match mode {
        "CLI" => SunshinePairingMode::SunshineCli,
        "PAIR_PIN" => SunshinePairingMode::SunshinePairPin,
        _ => SunshinePairingMode::ManualWebUi,
    })
}

#[derive(Clone, Copy)]
enum ProvisionStepMarker {
    SshKeyReady,
    SshKeyUploadedToVast,
    InstanceCreated,
    InstanceReady,
    SshConnected,
    NvidiaHeadlessConfigured,
    PostNvidiaRebootCompleted,
    SunshineConfigured,
    LowLatencyAudioConfigured,
    WireguardConfigured,
    MoonlightConfigured,
    AwaitingPairPin,
    PairingCompleted,
    PostProvisionCompleted,
}

fn step_completed(steps: &ProvisionedServerSteps, step: ProvisionStepMarker) -> bool {
    match step {
        ProvisionStepMarker::SshKeyReady => steps.ssh_key_ready,
        ProvisionStepMarker::SshKeyUploadedToVast => steps.ssh_key_uploaded_to_vast,
        ProvisionStepMarker::InstanceCreated => steps.instance_created,
        ProvisionStepMarker::InstanceReady => steps.instance_ready,
        ProvisionStepMarker::SshConnected => steps.ssh_connected,
        ProvisionStepMarker::NvidiaHeadlessConfigured => steps.nvidia_headless_configured,
        ProvisionStepMarker::PostNvidiaRebootCompleted => steps.post_nvidia_reboot_completed,
        ProvisionStepMarker::SunshineConfigured => steps.sunshine_configured,
        ProvisionStepMarker::LowLatencyAudioConfigured => steps.low_latency_audio_configured,
        ProvisionStepMarker::WireguardConfigured => steps.wireguard_configured,
        ProvisionStepMarker::MoonlightConfigured => steps.moonlight_configured,
        ProvisionStepMarker::AwaitingPairPin => steps.awaiting_pair_pin,
        ProvisionStepMarker::PairingCompleted => steps.pairing_completed,
        ProvisionStepMarker::PostProvisionCompleted => steps.post_provision_completed,
    }
}

fn set_step_completed(steps: &mut ProvisionedServerSteps, step: ProvisionStepMarker, value: bool) {
    match step {
        ProvisionStepMarker::SshKeyReady => steps.ssh_key_ready = value,
        ProvisionStepMarker::SshKeyUploadedToVast => steps.ssh_key_uploaded_to_vast = value,
        ProvisionStepMarker::InstanceCreated => steps.instance_created = value,
        ProvisionStepMarker::InstanceReady => steps.instance_ready = value,
        ProvisionStepMarker::SshConnected => steps.ssh_connected = value,
        ProvisionStepMarker::NvidiaHeadlessConfigured => steps.nvidia_headless_configured = value,
        ProvisionStepMarker::PostNvidiaRebootCompleted => {
            steps.post_nvidia_reboot_completed = value
        }
        ProvisionStepMarker::SunshineConfigured => steps.sunshine_configured = value,
        ProvisionStepMarker::LowLatencyAudioConfigured => {
            steps.low_latency_audio_configured = value
        }
        ProvisionStepMarker::WireguardConfigured => steps.wireguard_configured = value,
        ProvisionStepMarker::MoonlightConfigured => steps.moonlight_configured = value,
        ProvisionStepMarker::AwaitingPairPin => steps.awaiting_pair_pin = value,
        ProvisionStepMarker::PairingCompleted => steps.pairing_completed = value,
        ProvisionStepMarker::PostProvisionCompleted => steps.post_provision_completed = value,
    }
}

/// Determines the resume step for an existing instance based on saved progress
fn determine_resume_step(steps: &ProvisionedServerSteps) -> (OrchestrationState, String) {
    if steps.post_provision_completed {
        (
            OrchestrationState::Ready,
            "Resuming: Post-provision setup already completed".to_string(),
        )
    } else if steps.pairing_completed {
        (
            OrchestrationState::Ready,
            "Resuming: Pairing done, pending post-provision setup".to_string(),
        )
    } else if steps.awaiting_pair_pin {
        (
            OrchestrationState::AwaitingPairPin,
            "Resuming: Awaiting pairing PIN".to_string(),
        )
    } else if steps.moonlight_configured {
        (
            OrchestrationState::ConfiguringMoonlight,
            "Resuming: Moonlight configured, need pairing".to_string(),
        )
    } else if steps.wireguard_configured {
        (
            OrchestrationState::ConfiguringWireGuard,
            "Resuming: Starting from WireGuard config".to_string(),
        )
    } else if steps.low_latency_audio_configured {
        (
            OrchestrationState::ConfiguringSunshine,
            "Resuming: Starting from WireGuard setup".to_string(),
        )
    } else if steps.sunshine_configured {
        (
            OrchestrationState::ConfiguringSunshine,
            "Resuming: Sunshine configured, continue setup".to_string(),
        )
    } else if steps.nvidia_headless_configured {
        (
            OrchestrationState::ConfiguringNvidiaHeadless,
            "Resuming: Starting from NVIDIA setup".to_string(),
        )
    } else if steps.ssh_connected {
        (
            OrchestrationState::ConnectingSsh,
            "Resuming: SSH connected, starting remote config".to_string(),
        )
    } else if steps.instance_ready {
        (
            OrchestrationState::WaitingForInstance,
            "Resuming: Instance ready, waiting for SSH".to_string(),
        )
    } else if steps.instance_created {
        (
            OrchestrationState::CreatingInstance,
            "Resuming: Instance created, waiting for ready state".to_string(),
        )
    } else {
        (
            OrchestrationState::CreatingInstance,
            "Starting provisioning from beginning".to_string(),
        )
    }
}

async fn hydrate_state_from_server_record(
    context: &AppContext,
    instance_id: u64,
    include_instance_metadata: bool,
) -> AppResult<bool> {
    let record = {
        let snapshot = context.state.read().await;
        snapshot
            .provisioned_servers
            .iter()
            .find(|record| record.instance_id == instance_id)
            .cloned()
    };

    let Some(record) = record else {
        return Ok(false);
    };

    let offer_id = record.offer_id;
    let status = record.status.clone();
    let ssh_host = record.ssh_host.clone();
    let ssh_port = record.ssh_port;
    let ssh_command = record.ssh_command.clone();
    let wireguard_server_ip = record.wireguard_server_ip.clone();
    let wireguard_client_ip = record.wireguard_client_ip.clone();
    let wireguard_server_public_key = record.wireguard_server_public_key.clone();
    let wireguard_client_public_key = record.wireguard_client_public_key.clone();
    let wireguard_config_path = record.wireguard_config_path.clone();
    let sunshine_configured = record.steps.sunshine_configured;
    let moonlight_configured = record.steps.moonlight_configured || record.steps.pairing_completed;
    let moonlight_host_address = if record.moonlight_host_address.trim().is_empty() {
        wireguard_server_ip.clone()
    } else {
        record.moonlight_host_address.clone()
    };

    context
        .update_state(move |state| {
            state.instance.instance_id = Some(instance_id);
            if include_instance_metadata {
                state.instance.offer_id = offer_id.or(state.instance.offer_id);
                if !status.is_empty() {
                    state.instance.status = status.clone();
                }
                if !ssh_host.is_empty() {
                    state.instance.ssh_host = ssh_host.clone();
                }
                if ssh_port > 0 {
                    state.instance.ssh_port = ssh_port;
                }
                if !ssh_command.is_empty() {
                    state.instance.ssh_command = ssh_command.clone();
                }
            }

            state.wireguard.server_ip = wireguard_server_ip.clone();
            state.wireguard.client_ip = wireguard_client_ip.clone();
            state.wireguard.server_public_key = wireguard_server_public_key.clone();
            state.wireguard.client_public_key = wireguard_client_public_key.clone();
            state.wireguard.config_path = wireguard_config_path.clone();
            state.sunshine.configured = sunshine_configured;
            state.moonlight.configured = moonlight_configured;
            state.moonlight.host_address = moonlight_host_address.clone();
        })
        .await?;

    Ok(true)
}

async fn load_wireguard_result_from_server_record(
    context: &AppContext,
    instance_id: u64,
) -> Option<WireGuardProvisionResult> {
    let snapshot = context.state.read().await;
    let record = snapshot
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)?;

    if record.wireguard_server_ip.trim().is_empty() {
        return None;
    }

    Some(WireGuardProvisionResult {
        server_ip: record.wireguard_server_ip.clone(),
        client_ip: record.wireguard_client_ip.clone(),
        server_public_key: record.wireguard_server_public_key.clone(),
        client_public_key: record.wireguard_client_public_key.clone(),
        client_config_path: PathBuf::from(record.wireguard_config_path.clone()),
    })
}

async fn load_moonlight_host_from_server_record(
    context: &AppContext,
    instance_id: u64,
) -> Option<String> {
    let snapshot = context.state.read().await;
    let record = snapshot
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)?;

    let host = if record.moonlight_host_address.trim().is_empty() {
        record.wireguard_server_ip.clone()
    } else {
        record.moonlight_host_address.clone()
    };

    if host.trim().is_empty() {
        None
    } else {
        Some(host)
    }
}

async fn persist_wireguard_result_for_server(
    context: &AppContext,
    instance_id: u64,
    result: &WireGuardProvisionResult,
) -> AppResult<()> {
    let server_ip = result.server_ip.clone();
    let client_ip = result.client_ip.clone();
    let server_public_key = result.server_public_key.clone();
    let client_public_key = result.client_public_key.clone();
    let config_path = result.client_config_path.display().to_string();

    context
        .update_state(|app_state| {
            let index = app_state
                .provisioned_servers
                .iter()
                .position(|record| record.instance_id == instance_id)
                .unwrap_or_else(|| {
                    app_state
                        .provisioned_servers
                        .push(ProvisionedServerState::new(instance_id));
                    app_state.provisioned_servers.len() - 1
                });
            let record = &mut app_state.provisioned_servers[index];

            record.wireguard_server_ip = server_ip.clone();
            record.wireguard_client_ip = client_ip.clone();
            record.wireguard_server_public_key = server_public_key.clone();
            record.wireguard_client_public_key = client_public_key.clone();
            record.wireguard_config_path = config_path.clone();
        })
        .await?;

    Ok(())
}

async fn persist_moonlight_host_for_server(
    context: &AppContext,
    instance_id: u64,
    host_address: &str,
) -> AppResult<()> {
    let host_address = host_address.to_string();

    context
        .update_state(|app_state| {
            let index = app_state
                .provisioned_servers
                .iter()
                .position(|record| record.instance_id == instance_id)
                .unwrap_or_else(|| {
                    app_state
                        .provisioned_servers
                        .push(ProvisionedServerState::new(instance_id));
                    app_state.provisioned_servers.len() - 1
                });
            let record = &mut app_state.provisioned_servers[index];
            record.moonlight_host_address = host_address.clone();
        })
        .await?;

    Ok(())
}

async fn clear_server_steps(
    context: &AppContext,
    instance_id: u64,
    steps: &[ProvisionStepMarker],
) -> AppResult<()> {
    let steps_to_clear = steps.to_vec();

    context
        .update_state(move |app_state| {
            if let Some(record) = app_state
                .provisioned_servers
                .iter_mut()
                .find(|record| record.instance_id == instance_id)
            {
                for step in &steps_to_clear {
                    set_step_completed(&mut record.steps, *step, false);
                }
            }
        })
        .await?;

    Ok(())
}

async fn ensure_server_record(
    context: &AppContext,
    instance_id: u64,
    offer_id: Option<u64>,
    ssh_host: &str,
    ssh_port: u16,
    status: &str,
    state: OrchestrationState,
) -> AppResult<()> {
    let ssh_host = ssh_host.to_string();
    let status = status.to_string();

    // Get ssh_command from current instance state
    let ssh_command = {
        let snapshot = context.state.read().await;
        snapshot.instance.ssh_command.clone()
    };

    context
        .update_state(|app_state| {
            let index = app_state
                .provisioned_servers
                .iter()
                .position(|record| record.instance_id == instance_id)
                .unwrap_or_else(|| {
                    app_state
                        .provisioned_servers
                        .push(ProvisionedServerState::new(instance_id));
                    app_state.provisioned_servers.len() - 1
                });
            let record = &mut app_state.provisioned_servers[index];

            record.offer_id = offer_id.or(record.offer_id);
            if !ssh_host.is_empty() {
                record.ssh_host = ssh_host.clone();
            }
            if ssh_port > 0 {
                record.ssh_port = ssh_port;
            }
            if !status.is_empty() {
                record.status = status.clone();
            }
            if !ssh_command.is_empty() {
                record.ssh_command = ssh_command.clone();
            }
            record.last_state = state;
            record.last_error = None;
        })
        .await?;

    Ok(())
}

async fn server_step_is_completed(
    context: &AppContext,
    instance_id: u64,
    step: ProvisionStepMarker,
) -> bool {
    let snapshot = context.state.read().await;
    snapshot
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)
        .map(|record| step_completed(&record.steps, step))
        .unwrap_or(false)
}

async fn mark_server_step_completed(
    context: &AppContext,
    instance_id: u64,
    step: ProvisionStepMarker,
    state: OrchestrationState,
    status: &str,
    ssh_host: &str,
    ssh_port: u16,
    offer_id: Option<u64>,
) -> AppResult<()> {
    let status = status.to_string();
    let ssh_host = ssh_host.to_string();

    context
        .update_state(|app_state| {
            let index = app_state
                .provisioned_servers
                .iter()
                .position(|record| record.instance_id == instance_id)
                .unwrap_or_else(|| {
                    app_state
                        .provisioned_servers
                        .push(ProvisionedServerState::new(instance_id));
                    app_state.provisioned_servers.len() - 1
                });
            let record = &mut app_state.provisioned_servers[index];

            record.offer_id = offer_id.or(record.offer_id);
            if !status.is_empty() {
                record.status = status.clone();
            }
            if !ssh_host.is_empty() {
                record.ssh_host = ssh_host.clone();
            }
            if ssh_port > 0 {
                record.ssh_port = ssh_port;
            }
            record.last_state = state;
            record.last_error = None;
            set_step_completed(&mut record.steps, step, true);
        })
        .await?;

    Ok(())
}

async fn mark_server_error(
    context: &AppContext,
    instance_id: Option<u64>,
    error_message: &str,
) -> AppResult<()> {
    let Some(instance_id) = instance_id else {
        return Ok(());
    };

    let message = error_message.to_string();
    context
        .update_state(|app_state| {
            if let Some(record) = app_state
                .provisioned_servers
                .iter_mut()
                .find(|record| record.instance_id == instance_id)
            {
                record.last_state = OrchestrationState::Error;
                record.last_error = Some(message.clone());
            }
        })
        .await?;

    Ok(())
}

async fn emit_step_skipped(
    app: &AppHandle,
    context: &AppContext,
    state: OrchestrationState,
    message: &str,
    instance_id: u64,
) {
    emit_transition(
        app,
        context,
        state,
        message,
        Some(format!(
            "Step already applied for instance {}. Skipping.",
            instance_id
        )),
        false,
    )
    .await;
}

fn ensure_not_cancelled(context: &AppContext) -> AppResult<()> {
    if context.cancel_requested.load(Ordering::SeqCst) {
        return Err(AppError::Cancelled);
    }

    Ok(())
}

fn ensure_private_key_path_exists(path: &Path) -> AppResult<()> {
    if path.exists() {
        return Ok(());
    }

    Err(AppError::State(format!(
        "SSH private key not found at {}",
        path.display()
    )))
}

fn sanitize_ssh_user(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

async fn emit_transition(
    app: &AppHandle,
    context: &AppContext,
    state: OrchestrationState,
    message: &str,
    details: Option<String>,
    is_error: bool,
) {
    let persisted_error_message = if is_error {
        Some(details.as_deref().unwrap_or(message).to_string())
    } else {
        None
    };

    if let Err(error) = context
        .update_state(|current| {
            current.orchestration_state = state;
            if is_error {
                current.last_error = persisted_error_message.clone();
            } else if current.last_error.as_deref() == Some(message) {
                current.last_error = None;
            }

            if let Some(instance_id) = current.instance.instance_id {
                if let Some(record) = current
                    .provisioned_servers
                    .iter_mut()
                    .find(|record| record.instance_id == instance_id)
                {
                    record.last_state = state;
                    if is_error {
                        record.last_error = persisted_error_message.clone();
                    } else if record.last_error.as_deref() == Some(message) {
                        record.last_error = None;
                    }
                }
            }
        })
        .await
    {
        warn!("could not persist orchestration transition: {error}");
    }

    let event = if is_error {
        ProvisioningEvent::error(state, message, details)
    } else {
        ProvisioningEvent::info(state, message, details)
    };

    context.emit_progress(app, event).await;
}
