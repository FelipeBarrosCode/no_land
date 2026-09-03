use std::collections::HashMap;

use tauri::State;
use tracing::info;

use crate::errors::{AppError, FrontendError};
use crate::models::application_bundle::{
    ProfileReference, ProviderDefinition, SharedStorageProfile, SharedStorageStatus,
    SharedStorageTestResult, StorageProvider,
};
use crate::services::app_context::AppContext;
use crate::services::remote_exec::RemoteExec;
use crate::services::shared_storage::object_storage::StorageCredential;
use crate::services::shared_storage::provider_profiles::{
    shared_profile_manager, SharedStorageProfileManager,
};
use crate::services::shared_storage::shared_storage_manager::SharedStorageManager;
use crate::services::vast_api::VastApiClient;

fn get_profile_manager() -> std::sync::Arc<SharedStorageProfileManager> {
    shared_profile_manager()
}

// ─── Commands ──────────────────────────────────────────────

#[tauri::command]
pub async fn list_storage_providers() -> Result<Vec<ProviderDefinition>, FrontendError> {
    let manager = get_profile_manager();
    Ok(manager.list_providers())
}

#[tauri::command]
pub async fn save_static_provider_credentials(
    context: State<'_, AppContext>,
    provider: String,
    credentials_json: String,
    bucket: Option<String>,
    prefix: Option<String>,
    display_name: String,
) -> Result<SharedStorageProfile, FrontendError> {
    clear_pending_oauth_sessions(context.inner()).await?;
    let provider: StorageProvider = serde_json::from_str(&format!(r#"\"{}\""#, provider))
        .map_err(|e| AppError::InvalidInput(format!("Unknown provider: {e}")))?;

    let raw_fields: HashMap<String, String> = serde_json::from_str(&credentials_json)
        .map_err(|e| AppError::InvalidInput(format!("Invalid credentials payload: {e}")))?;
    let credentials = build_storage_credentials(&provider, &raw_fields)?;

    let manager = get_profile_manager();
    let profile = manager
        .save_static_credentials(
            context.inner(),
            &provider,
            &credentials,
            &raw_fields,
            bucket.as_deref(),
            prefix.as_deref(),
            &display_name,
        )
        .await?;

    // Save profile reference to app state
    let profile_ref = ProfileReference {
        id: profile.id.clone(),
        display_name: profile.display_name.clone(),
        provider_label: profile.provider_label.clone(),
        provider: Some(provider.clone()),
        bucket: bucket.clone(),
        prefix: prefix.clone(),
        active: true,
    };

    context
        .update_state(|state| {
            for existing in &mut state.shared_storage_profiles {
                existing.active = false;
            }
            state
                .shared_storage_profiles
                .retain(|existing| existing.id != profile_ref.id);
            state.shared_storage_profiles.push(profile_ref.clone());
        })
        .await?;

    info!("Shared storage profile saved: {}", profile.id);
    Ok(profile)
}

#[tauri::command]
pub async fn test_shared_storage_connection(
    context: State<'_, AppContext>,
    profile_id: String,
) -> Result<SharedStorageTestResult, FrontendError> {
    info!("Testing shared storage connection for profile: {profile_id}");

    let remote = match build_remote_exec_from_state(context.inner()).await {
        Ok(remote) => remote,
        Err(error) => {
            return Ok(failed_test_result(format!(
                "Select or start a server before testing shared storage: {error}"
            )));
        }
    };
    let target_user = context.config.audio_target_user.clone();

    SharedStorageManager::test_profile_connection(
        context.inner(),
        &remote,
        &target_user,
        Some(&profile_id),
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn get_shared_storage_profiles(
    context: State<'_, AppContext>,
) -> Result<Vec<ProfileReference>, FrontendError> {
    ensure_active_profile_reference(context.inner()).await?;
    let state = context.load_state().await;
    Ok(state.shared_storage_profiles.clone())
}

#[tauri::command]
pub async fn set_active_shared_storage_profile(
    context: State<'_, AppContext>,
    profile_id: String,
) -> Result<(), FrontendError> {
    let found = {
        let state = context.load_state().await;
        state
            .shared_storage_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
    };
    if !found {
        return Err(
            AppError::NotFound(format!("Shared storage profile {} not found", profile_id)).into(),
        );
    }

    context
        .update_state(|state| {
            for profile in &mut state.shared_storage_profiles {
                profile.active = profile.id == profile_id;
            }
        })
        .await?;

    Ok(())
}

#[tauri::command]
pub async fn disconnect_shared_storage_profile(
    context: State<'_, AppContext>,
    profile_id: String,
) -> Result<(), FrontendError> {
    let manager = get_profile_manager();

    // Find the profile reference first
    let state = context.load_state().await;
    let profile_ref = state
        .shared_storage_profiles
        .iter()
        .find(|p| p.id == profile_id)
        .cloned();

    if let Some(pref) = profile_ref {
        let profile = SharedStorageProfile {
            id: profile_id.clone(),
            display_name: pref.display_name,
            provider: pref.provider.unwrap_or(StorageProvider::BackblazeB2),
            provider_label: pref.provider_label,
            bucket: pref.bucket.clone(),
            prefix: pref.prefix.clone(),
            credential_vault_reference: format!(
                "state.json:sharedStorageCredentials.profiles.{}",
                profile_id
            ),
            repository_id: String::new(),
            status: SharedStorageStatus::NotConfigured,
            last_verified_at: None,
            protected_bundles_count: 0,
            total_stored_bytes: 0,
        };
        manager
            .delete_profile_credentials(context.inner(), &profile)
            .await?;
    }

    context
        .update_state(|state| {
            let was_active = state
                .shared_storage_profiles
                .iter()
                .any(|profile| profile.id == profile_id && profile.active);

            // Delete the profile completely: its UI reference and any
            // remaining credential/session artifacts.
            state
                .shared_storage_credentials
                .profiles
                .remove(&profile_id);
            state
                .shared_storage_profiles
                .retain(|profile| profile.id != profile_id);

            if was_active {
                if let Some(first) = state.shared_storage_profiles.first_mut() {
                    first.active = true;
                }
            }
        })
        .await?;

    info!("Shared storage profile disconnected and deleted: {profile_id}");
    Ok(())
}

fn required_field<'a>(
    raw_fields: &'a HashMap<String, String>,
    key: &str,
) -> Result<&'a str, FrontendError> {
    raw_fields
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::InvalidInput(format!("Missing required field: {key}")).into())
}

fn optional_field(raw_fields: &HashMap<String, String>, key: &str) -> Option<String> {
    raw_fields
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn build_storage_credentials(
    provider: &StorageProvider,
    raw_fields: &HashMap<String, String>,
) -> Result<StorageCredential, FrontendError> {
    match provider {
        StorageProvider::BackblazeB2 => Ok(StorageCredential::BackblazeB2 {
            key_id: required_field(raw_fields, "key_id")?.to_string(),
            application_key: required_field(raw_fields, "application_key")?.to_string(),
        }),
        StorageProvider::AmazonS3
        | StorageProvider::CloudflareR2
        | StorageProvider::Wasabi
        | StorageProvider::DigitalOceanSpaces
        | StorageProvider::GenericS3 => Ok(StorageCredential::S3 {
            access_key_id: required_field(raw_fields, "access_key_id")?.to_string(),
            secret_access_key: required_field(raw_fields, "secret_access_key")?.to_string(),
            session_token: optional_field(raw_fields, "session_token"),
        }),
        StorageProvider::GoogleCloudStorage => Ok(StorageCredential::ServiceAccount {
            json: required_field(raw_fields, "service_account_json")?.to_string(),
        }),
        StorageProvider::AzureBlob => Ok(StorageCredential::UsernamePassword {
            username: required_field(raw_fields, "account_name")?.to_string(),
            password: required_field(raw_fields, "account_key")?.to_string(),
        }),
        StorageProvider::Sftp | StorageProvider::Webdav => {
            Ok(StorageCredential::UsernamePassword {
                username: required_field(raw_fields, "username")?.to_string(),
                password: required_field(raw_fields, "password")?.to_string(),
            })
        }
        StorageProvider::GoogleDrive
        | StorageProvider::MicrosoftOneDrive
        | StorageProvider::Dropbox
        | StorageProvider::Box => Err(AppError::InvalidInput(format!(
            "{} must be connected with OAuth in Noland instead of static credentials.",
            provider.label()
        ))
        .into()),
    }
}

fn failed_test_result(error: String) -> SharedStorageTestResult {
    SharedStorageTestResult {
        authenticated: false,
        can_list: false,
        can_write: false,
        can_read: false,
        can_delete_test_object: false,
        repository_accessible: false,
        latency_ms: None,
        error: Some(error),
    }
}

async fn clear_oauth_session_artifacts(
    context: &AppContext,
    session_id: &str,
) -> Result<(), FrontendError> {
    get_profile_manager()
        .delete_oauth_session_credentials(context, session_id)
        .await?;

    let mut sessions = get_oauth_sessions().write().await;
    sessions.remove(session_id);
    Ok(())
}

async fn clear_pending_oauth_sessions(context: &AppContext) -> Result<(), FrontendError> {
    let session_ids = {
        let sessions = get_oauth_sessions().read().await;
        sessions.keys().cloned().collect::<Vec<_>>()
    };

    if session_ids.is_empty() {
        return Ok(());
    }

    for session_id in &session_ids {
        clear_oauth_session_artifacts(context, session_id).await?;
    }

    Ok(())
}

async fn ensure_active_profile_reference(context: &AppContext) -> Result<(), FrontendError> {
    let needs_update = {
        let state = context.load_state().await;
        !state.shared_storage_profiles.is_empty()
            && !state
                .shared_storage_profiles
                .iter()
                .any(|profile| profile.active)
    };

    if needs_update {
        context
            .update_state(|state| {
                if let Some(first) = state.shared_storage_profiles.first_mut() {
                    first.active = true;
                }
            })
            .await?;
    }

    Ok(())
}

async fn build_remote_exec_from_state(context: &AppContext) -> Result<RemoteExec, AppError> {
    let state = context.load_state().await;
    let private_key_path = state.ssh.private_key_path.clone();
    if private_key_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "SSH private key path is empty. Run provisioning first.".to_string(),
        ));
    }

    let instance_id = state.instance.instance_id.ok_or_else(|| {
        AppError::InvalidInput(
            "No active instance selected. Start or select a server first.".to_string(),
        )
    })?;

    let ssh_user = if state.ssh.ssh_username.trim().is_empty() {
        context.config.ssh_user.clone()
    } else {
        state.ssh.ssh_username.clone()
    };
    let ssh_password = state.ssh.ssh_password.clone();

    let api_key = state.credentials.vast_api_key.clone();
    if api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Vast API key is missing. Add it in Settings first.".to_string(),
        ));
    }
    drop(state);

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );
    let instance = vast.get_instance(instance_id).await?;
    let ssh_host = if instance.public_ip.trim().is_empty() {
        instance.ssh_host.clone()
    } else {
        instance.public_ip.clone()
    };
    if ssh_host.trim().is_empty() || instance.ssh_port == 0 {
        return Err(AppError::InvalidInput(format!(
            "Instance {} SSH details are unavailable.",
            instance_id
        )));
    }

    Ok(RemoteExec {
        ssh_user,
        ssh_host,
        ssh_port: instance.ssh_port,
        private_key_path,
        ssh_password,
    })
}

// ─── OAuth flow state ────────────────────────────────────

use tokio::sync::RwLock;

static OAUTH_SESSIONS: std::sync::OnceLock<RwLock<HashMap<String, OAuthPendingSession>>> =
    std::sync::OnceLock::new();

fn get_oauth_sessions() -> &'static RwLock<HashMap<String, OAuthPendingSession>> {
    OAUTH_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone)]
struct OAuthPendingSession {
    pub provider: StorageProvider,
    pub display_name: String,
    pub code_verifier: String,
    pub state: String,

    pub oauth_fields: HashMap<String, String>,
    pub status: OAuthSessionStatus,
}

#[derive(Debug, Clone, PartialEq)]
enum OAuthSessionStatus {
    Pending,
    Completed,
    Failed(String),
}

// ─── OAuth Commands ──────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthBeginResponse {
    pub session_id: String,
    pub authorization_url: String,
    pub provider_label: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCompleteResponse {
    pub profile: SharedStorageProfile,
    pub account_email: Option<String>,
}

#[tauri::command]
pub async fn begin_oauth_authorization(
    context: State<'_, AppContext>,
    provider: String,
    display_name: String,
    client_id: String,
    client_secret: Option<String>,
    provider_fields_json: Option<String>,
) -> Result<OAuthBeginResponse, FrontendError> {
    clear_pending_oauth_sessions(context.inner()).await?;
    let provider: StorageProvider = serde_json::from_str(&format!("\"{}\"", provider))
        .map_err(|e| AppError::InvalidInput(format!("Unknown provider: {e}")))?;

    let config = crate::services::shared_storage::oauth_flow::get_oauth_config(
        &provider,
        &client_id,
        client_secret.as_deref(),
    )
    .ok_or_else(|| {
        AppError::InvalidInput(format!("{} does not support OAuth", provider.label()))
    })?;

    let code_verifier = crate::services::shared_storage::oauth_flow::generate_code_verifier();
    let code_challenge =
        crate::services::shared_storage::oauth_flow::derive_code_challenge(&code_verifier);
    let state = crate::services::shared_storage::oauth_flow::generate_state();

    // Pick a random available port between 17700-17900
    let port = 17770u16;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let session_id = uuid::Uuid::new_v4().to_string();

    let auth_url = config.build_authorization_url(&redirect_uri, &state, &code_challenge);

    {
        let mut sessions = get_oauth_sessions().write().await;
        sessions.insert(
            session_id.clone(),
            OAuthPendingSession {
                provider: provider.clone(),
                display_name: display_name.clone(),
                code_verifier,
                state,

                oauth_fields: {
                    let mut fields = provider_fields_json
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| serde_json::from_str::<HashMap<String, String>>(value))
                        .transpose()
                        .map_err(|error| {
                            AppError::InvalidInput(format!(
                                "Invalid OAuth provider fields payload: {error}"
                            ))
                        })?
                        .unwrap_or_default();
                    fields.insert("client_id".to_string(), client_id.clone());
                    if let Some(secret) = client_secret
                        .clone()
                        .filter(|value| !value.trim().is_empty())
                    {
                        fields.insert("client_secret".to_string(), secret);
                    }
                    fields
                },
                status: OAuthSessionStatus::Pending,
            },
        );
    }

    // Spawn a local HTTP server to receive the callback
    let session_id_clone = session_id.clone();
    let provider_clone = provider.clone();
    let display_name_clone = display_name.clone();
    let config_clone = config.clone();
    let app_context = context.inner().clone();
    tokio::spawn(async move {
        if let Err(e) = run_loopback_server(
            app_context,
            port,
            session_id_clone,
            provider_clone,
            display_name_clone,
            config_clone,
        )
        .await
        {
            tracing::warn!("OAuth loopback server error: {e}");
        }
    });

    info!("OAuth flow started for {}: {}", provider.label(), auth_url);

    Ok(OAuthBeginResponse {
        session_id,
        authorization_url: auth_url,
        provider_label: provider.label().to_string(),
    })
}

#[tauri::command]
pub async fn complete_oauth_authorization(
    context: State<'_, AppContext>,
    session_id: String,
) -> Result<OAuthCompleteResponse, FrontendError> {
    // Check session status
    let session_snapshot = {
        let sessions = get_oauth_sessions().read().await;
        sessions.get(&session_id).cloned()
    };
    let session_status = session_snapshot.as_ref().map(|s| s.status.clone());

    match session_status {
        Some(OAuthSessionStatus::Pending) => {
            return Err(AppError::InvalidInput(
                "OAuth flow still in progress. Complete authorization in your browser.".to_string(),
            )
            .into());
        }
        Some(OAuthSessionStatus::Failed(reason)) => {
            clear_oauth_session_artifacts(context.inner(), &session_id).await?;
            return Err(AppError::Provisioning(format!(
                "Authorization failed: {}. Please try again.",
                reason
            ))
            .into());
        }
        Some(OAuthSessionStatus::Completed) | None => {
            // Session completed (or was never in our map — maybe from a previous run)
            // Try to retrieve stored credentials
        }
    }

    info!("Looking up OAuth credentials in persisted state for session {session_id}");

    let manager = get_profile_manager();
    let credentials = match manager
        .retrieve_oauth_session_credentials(context.inner(), &session_id)
        .await?
    {
        Some(value) => value,
        None => {
            tracing::warn!(
                "No OAuth credentials found in persisted state for session {session_id}"
            );
            clear_oauth_session_artifacts(context.inner(), &session_id).await?;
            return Err(FrontendError::from(AppError::NotFound(
                "No credentials found for this session. Stale authorization data was cleared; start a new authorization now.".to_string(),
            )));
        }
    };

    let session = match session_snapshot {
        Some(session) => session,
        None => {
            clear_oauth_session_artifacts(context.inner(), &session_id).await?;
            return Err(FrontendError::from(AppError::NotFound(
                "No OAuth session metadata found. Stale authorization data was cleared; start authorization again.".to_string(),
            )));
        }
    };

    let provider = session.provider.clone();
    let profile = manager
        .save_oauth_credentials(
            context.inner(),
            &provider,
            &credentials,
            &session.oauth_fields,
            &session.display_name,
        )
        .await?;

    clear_oauth_session_artifacts(context.inner(), &session_id).await?;

    info!("OAuth profile created successfully for session {session_id}");

    // Persist profile reference to app state
    let profile_ref = ProfileReference {
        id: profile.id.clone(),
        display_name: profile.display_name.clone(),
        provider_label: profile.provider_label.clone(),
        provider: Some(provider),
        bucket: None,
        prefix: session
            .oauth_fields
            .get("folder")
            .cloned()
            .or_else(|| session.oauth_fields.get("remote_path").cloned()),
        active: true,
    };
    context
        .update_state(|state| {
            for existing in &mut state.shared_storage_profiles {
                existing.active = false;
            }
            state
                .shared_storage_profiles
                .retain(|existing| existing.id != profile_ref.id);
            state.shared_storage_profiles.push(profile_ref.clone());
        })
        .await?;

    Ok(OAuthCompleteResponse {
        profile,
        account_email: None,
    })
}

async fn run_loopback_server(
    context: AppContext,
    port: u16,
    session_id: String,
    provider: StorageProvider,
    _display_name: String,
    oauth_config: crate::services::shared_storage::oauth_flow::OAuthProviderConfig,
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Bind failed: {e}"))?;

    tracing::info!("OAuth loopback server listening on {addr}");

    // Accept one connection
    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|e| format!("Accept failed: {e}"))?;

    let mut buf = [0u8; 4096];
    let n = socket
        .read(&mut buf)
        .await
        .map_err(|e| format!("Read failed: {e}"))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Extract the code and state from the callback URL
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("");

    let code = extract_query_param(path, "code");
    let returned_state = extract_query_param(path, "state");
    let error = extract_query_param(path, "error");
    let had_callback_error = error.is_some();

    // Process the OAuth completion FIRST, then respond to the browser with the
    // real outcome.  Previously the browser was told "Authorization Complete"
    // before the token exchange finished, so a failed exchange still showed a
    // success page and the user clicked "Complete Authorization" only to be
    // bounced back to the start.
    let mut outcome_ok = false;
    let mut outcome_message = String::new();

    if let Some(auth_code) = code {
        let stored_state = {
            let sessions = get_oauth_sessions().read().await;
            sessions.get(&session_id).map(|s| {
                (
                    s.state.clone(),
                    s.code_verifier.clone(),
                    s.display_name.clone(),
                )
            })
        };

        if let Some((expected_state, code_verifier, _display_name)) = stored_state {
            if returned_state.as_deref() != Some(&expected_state) {
                tracing::warn!("OAuth state mismatch for session {session_id}");
                outcome_message = "State mismatch — please try again.".to_string();
                {
                    let mut sessions = get_oauth_sessions().write().await;
                    if let Some(s) = sessions.get_mut(&session_id) {
                        s.status = OAuthSessionStatus::Failed(outcome_message.clone());
                    }
                }
            } else {
                // Exchange code for tokens
                let redirect_uri = format!("http://127.0.0.1:{port}/callback");
                match crate::services::shared_storage::oauth_flow::exchange_code(
                    &oauth_config,
                    &auth_code,
                    &code_verifier,
                    &redirect_uri,
                )
                .await
                {
                    Ok(token) => {
                        let refresh_token = token
                            .refresh_token
                            .clone()
                            .filter(|value| !value.trim().is_empty());
                        if refresh_token.is_none() {
                            outcome_message = format!(
                                "{} did not provide a refresh token. Revoke Noland's existing authorization and reconnect so background transfers remain authorized.",
                                provider.label()
                            );
                            let mut sessions = get_oauth_sessions().write().await;
                            if let Some(s) = sessions.get_mut(&session_id) {
                                s.status = OAuthSessionStatus::Failed(outcome_message.clone());
                            }
                        } else {
                            let now = chrono::Utc::now().timestamp();
                            let credentials = StorageCredential::OAuth2 {
                                access_token: token.access_token.clone(),
                                refresh_token,
                                expires_at: now + token.expires_in.unwrap_or(3600),
                            };

                            get_profile_manager()
                                .store_oauth_session_credentials(
                                    &context,
                                    &session_id,
                                    &credentials,
                                )
                                .await
                                .map_err(|e| format!("Persist OAuth session credentials: {e}"))?;

                            let verify: Option<StorageCredential> = get_profile_manager()
                                .retrieve_oauth_session_credentials(&context, &session_id)
                                .await
                                .map_err(|e| {
                                    format!("Verify persisted OAuth session credentials: {e}")
                                })?;
                            tracing::info!(
                                "OAuth tokens stored for session {session_id}, verified: {}",
                                verify.is_some()
                            );

                            let mut sessions = get_oauth_sessions().write().await;
                            if let Some(s) = sessions.get_mut(&session_id) {
                                s.status = OAuthSessionStatus::Completed;
                            }
                            outcome_ok = true;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("OAuth token exchange failed for {session_id}: {e}");
                        outcome_message = format!("Token exchange failed: {e}");
                        // Mark session as failed
                        {
                            let mut sessions = get_oauth_sessions().write().await;
                            if let Some(s) = sessions.get_mut(&session_id) {
                                s.status = OAuthSessionStatus::Failed(e.clone());
                            }
                        }
                    }
                }
            }
        } else {
            outcome_message = "Session not found — please try again.".to_string();
        }
    } else if let Some(err) = error {
        outcome_message = format!("Authorization denied: {err}");
        // Mark session as failed
        {
            let mut sessions = get_oauth_sessions().write().await;
            if let Some(s) = sessions.get_mut(&session_id) {
                s.status = OAuthSessionStatus::Failed(outcome_message.clone());
            }
        }
        tracing::warn!("OAuth authorization denied for session {session_id}");
    } else {
        outcome_message = "No authorization code received.".to_string();
    }

    // NOW respond to the browser with the accurate outcome
    let response_body: String = if outcome_ok {
        "<html><body style='font-family:sans-serif;max-width:500px;margin:50px auto;text-align:center'><h1 style='color:#2e7d32'>Authorization Complete</h1><p>You've successfully authorized Noland.</p><p>You can close this window and return to Noland to complete setup.</p></body></html>".to_string()
    } else if had_callback_error || outcome_message.contains("denied") {
        format!("<html><body style='font-family:sans-serif;max-width:500px;margin:50px auto;text-align:center'><h1 style='color:#d32f2f'>Authorization Failed</h1><p>{}</p><p>You can close this window and try again in Noland.</p></body></html>", html_escape(&outcome_message))
    } else {
        format!("<html><body style='font-family:sans-serif;max-width:500px;margin:50px auto;text-align:center'><h1 style='color:#d32f2f'>Authorization Incomplete</h1><p>{}</p><p>You can close this window and try again in Noland.</p></body></html>", html_escape(&outcome_message))
    };

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;

    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn extract_query_param(path: &str, param: &str) -> Option<String> {
    let query_start = path.find('?')?;
    let query = &path[query_start + 1..];
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let value = parts.next().unwrap_or("");
        if key == param {
            return Some(urlencoding::decode(value).ok()?.into_owned());
        }
    }
    None
}
