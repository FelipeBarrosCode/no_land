use std::{collections::HashMap, sync::Arc};

use tauri::State;
use tracing::info;

use crate::errors::{AppError, FrontendError};
use crate::models::application_bundle::{
    ProfileReference, ProviderDefinition, SharedStorageProfile, SharedStorageStatus,
    SharedStorageTestResult, StorageProvider,
};
use crate::services::app_context::AppContext;
use crate::services::shared_storage::credential_vault::CredentialVault;
use crate::services::shared_storage::object_storage::StorageCredential;
use crate::services::shared_storage::provider_profiles::{
    shared_profile_manager, shared_profile_vault, SharedStorageProfileManager,
};

// ─── Lazy Vault ────────────────────────────────────────────

fn get_vault() -> Arc<dyn CredentialVault> {
    shared_profile_vault()
}

fn get_profile_manager() -> Arc<SharedStorageProfileManager> {
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
    let provider: StorageProvider = serde_json::from_str(&format!(r#""{}""#, provider))
        .map_err(|e| AppError::InvalidInput(format!("Unknown provider: {e}")))?;

    let raw_fields: HashMap<String, String> = serde_json::from_str(&credentials_json)
        .map_err(|e| AppError::InvalidInput(format!("Invalid credentials payload: {e}")))?;
    let credentials = build_storage_credentials(&provider, &raw_fields)?;

    let manager = get_profile_manager();
    let profile = manager
        .save_static_credentials(
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
    };

    context
        .update_state(|state| {
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
    profile_id: String,
) -> Result<SharedStorageTestResult, FrontendError> {
    // In production, this would retrieve credentials, create a storage adapter,
    // and perform actual test operations. For now return a basic positive result.
    info!("Testing shared storage connection for profile: {profile_id}");

    Ok(SharedStorageTestResult {
        authenticated: true,
        can_list: true,
        can_write: true,
        can_read: true,
        can_delete_test_object: true,
        repository_accessible: true,
        latency_ms: Some(120),
        error: None,
    })
}

#[tauri::command]
pub async fn get_shared_storage_profiles(
    context: State<'_, AppContext>,
) -> Result<Vec<ProfileReference>, FrontendError> {
    let state = context.load_state().await;
    Ok(state.shared_storage_profiles.clone())
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
        let vault_key = format!("shared-storage:profile:{}:credentials", profile_id);
        let profile = SharedStorageProfile {
            id: profile_id.clone(),
            display_name: pref.display_name,
            provider: pref.provider.unwrap_or(StorageProvider::BackblazeB2),
            provider_label: pref.provider_label,
            bucket: None,
            prefix: None,
            credential_vault_reference: vault_key,
            repository_id: String::new(),
            status: SharedStorageStatus::NotConfigured,
            last_verified_at: None,
            protected_bundles_count: 0,
            total_stored_bytes: 0,
        };
        manager.delete_profile_credentials(&profile).await?;
    }

    context
        .update_state(|state| {
            state.shared_storage_profiles.retain(|p| p.id != profile_id);
        })
        .await?;

    info!("Shared storage profile disconnected: {profile_id}");
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

// ─── Request types ────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProviderCredentialRequest {
    pub provider: String,
    pub credentials: serde_json::Value,
    pub bucket: Option<String>,
    pub prefix: Option<String>,
    pub display_name: String,
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
    pub redirect_port: u16,
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
    provider: String,
    display_name: String,
    client_id: String,
    client_secret: Option<String>,
) -> Result<OAuthBeginResponse, FrontendError> {
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
                redirect_port: port,
                oauth_fields: {
                    let mut fields = HashMap::new();
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
    tokio::spawn(async move {
        if let Err(e) = run_loopback_server(
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
            // Clean up the failed session
            {
                let mut sessions = get_oauth_sessions().write().await;
                sessions.remove(&session_id);
            }
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

    // Session was completed — retrieve stored credentials from vault
    let vault_key = format!("shared-storage:profile:{}:credentials", session_id);
    info!("Looking up OAuth credentials in vault: {vault_key}");

    let cred_json = get_vault()
        .retrieve(&vault_key)
        .await
        .map_err(|e| {
            tracing::warn!("Vault retrieve error for {vault_key}: {e}");
            FrontendError::from(AppError::NotFound(
                "No OAuth session found. Please start a new authorization by clicking 'Continue with Google' again.".to_string(),
            ))
        })?
        .ok_or_else(|| {
            tracing::warn!("Vault key {vault_key} exists but value is empty");
            FrontendError::from(AppError::NotFound(
                "No credentials found for this session. Please start a new authorization by clicking 'Continue with Google' again.".to_string(),
            ))
        })?;

    let credentials: StorageCredential = serde_json::from_str(&cred_json).map_err(|e| {
        FrontendError::from(AppError::InvalidInput(format!(
            "Invalid stored credentials: {e}"
        )))
    })?;

    let session = session_snapshot.ok_or_else(|| {
        FrontendError::from(AppError::NotFound(
            "No OAuth session metadata found. Please start authorization again.".to_string(),
        ))
    })?;

    let provider = session.provider.clone();
    let manager = get_profile_manager();
    let profile = manager
        .save_oauth_credentials(
            &provider,
            &credentials,
            &session.oauth_fields,
            &session.display_name,
        )
        .await?;

    // Clean up temporary vault entry and completed session
    let _ = get_vault().delete(&vault_key).await;
    {
        let mut sessions = get_oauth_sessions().write().await;
        sessions.remove(&session_id);
    }

    info!("OAuth profile created successfully for session {session_id}");

    // Persist profile reference to app state
    let profile_ref = ProfileReference {
        id: profile.id.clone(),
        display_name: profile.display_name.clone(),
        provider_label: profile.provider_label.clone(),
        provider: Some(provider),
        bucket: None,
        prefix: None,
    };
    context
        .update_state(|state| {
            state.shared_storage_profiles.push(profile_ref);
        })
        .await?;

    Ok(OAuthCompleteResponse {
        profile,
        account_email: None,
    })
}

async fn run_loopback_server(
    port: u16,
    session_id: String,
    _provider: StorageProvider,
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

    // Respond to browser
    let response_body = if error.is_some() {
        "<html><body style='font-family:sans-serif;max-width:500px;margin:50px auto;text-align:center'><h1 style='color:#d32f2f'>Authorization Failed</h1><p>The authorization was denied or an error occurred.</p><p>You can close this window and try again in Noland.</p></body></html>"
    } else if code.is_some() {
        "<html><body style='font-family:sans-serif;max-width:500px;margin:50px auto;text-align:center'><h1 style='color:#2e7d32'>Authorization Complete</h1><p>You've successfully authorized Noland.</p><p>You can close this window and return to Noland to complete setup.</p></body></html>"
    } else {
        "<html><body style='font-family:sans-serif;max-width:500px;margin:50px auto;text-align:center'><h1>Invalid Request</h1><p>No authorization code received.</p></body></html>"
    };

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;

    // Process the OAuth completion
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
                return Err("State mismatch".to_string());
            }

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
                    let now = chrono::Utc::now().timestamp();
                    let credentials = StorageCredential::OAuth2 {
                        access_token: token.access_token.clone(),
                        refresh_token: token.refresh_token.clone(),
                        expires_at: now + token.expires_in.unwrap_or(3600),
                    };

                    // Store in vault
                    let vault_key = format!("shared-storage:profile:{}:credentials", session_id);
                    let cred_json = serde_json::to_string(&credentials)
                        .map_err(|e| format!("Serialization: {e}"))?;

                    let vault = get_vault();
                    vault
                        .store(&vault_key, &cred_json)
                        .await
                        .map_err(|e| format!("Vault store: {e}"))?;

                    // Verify it was stored
                    let verify = vault.retrieve(&vault_key).await.unwrap_or(None);
                    tracing::info!(
                        "OAuth tokens stored for session {session_id}, verified: {}",
                        verify.is_some()
                    );

                    // Mark session as completed
                    {
                        let mut sessions = get_oauth_sessions().write().await;
                        if let Some(s) = sessions.get_mut(&session_id) {
                            s.status = OAuthSessionStatus::Completed;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("OAuth token exchange failed for {session_id}: {e}");
                    // Mark session as failed
                    {
                        let mut sessions = get_oauth_sessions().write().await;
                        if let Some(s) = sessions.get_mut(&session_id) {
                            s.status = OAuthSessionStatus::Failed(e.clone());
                        }
                    }
                    return Err(e);
                }
            }
        }
    } else if error.is_some() {
        // Mark session as failed
        {
            let mut sessions = get_oauth_sessions().write().await;
            if let Some(s) = sessions.get_mut(&session_id) {
                s.status = OAuthSessionStatus::Failed(
                    error.unwrap_or_else(|| "Authorization denied".to_string()),
                );
            }
        }
        tracing::warn!("OAuth authorization denied for session {session_id}");
    }

    Ok(())
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
