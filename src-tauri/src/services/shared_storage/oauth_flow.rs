use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand_core::RngCore;
use sha2::{Digest, Sha256};

/// Generate a cryptographically random code verifier for PKCE.
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Derive the S256 code challenge from a verifier.
pub fn derive_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate a random OAuth state parameter.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// OAuth provider configuration for the client-side flow.
#[derive(Debug, Clone)]
pub struct OAuthProviderConfig {
    pub authorization_endpoint: &'static str,
    pub token_endpoint: &'static str,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Space-separated scopes.
    pub scopes: &'static str,
    /// Extra query parameters to append to the authorization URL.
    pub extra_auth_params: &'static [(&'static str, &'static str)],
}

impl OAuthProviderConfig {
    pub fn build_authorization_url(
        &self,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
    ) -> String {
        let mut url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&scope={}",
            self.authorization_endpoint,
            urlencoding(&self.client_id),
            urlencoding(redirect_uri),
            urlencoding(state),
            urlencoding(code_challenge),
            urlencoding(self.scopes),
        );
        for (key, value) in self.extra_auth_params {
            url.push_str(&format!("&{}={}", key, urlencoding(value)));
        }
        url
    }
}

fn urlencoding(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// Build an OAuth config for a provider using user-supplied credentials.
pub fn get_oauth_config(
    provider: &crate::models::application_bundle::StorageProvider,
    client_id: &str,
    client_secret: Option<&str>,
) -> Option<OAuthProviderConfig> {
    match provider {
        crate::models::application_bundle::StorageProvider::GoogleDrive => Some(OAuthProviderConfig {
            authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth",
            token_endpoint: "https://oauth2.googleapis.com/token",
            client_id: client_id.to_string(),
            client_secret: client_secret.map(String::from),
            scopes: "https://www.googleapis.com/auth/drive.file https://www.googleapis.com/auth/userinfo.email",
            extra_auth_params: &[("access_type", "offline"), ("prompt", "consent")],
        }),
        crate::models::application_bundle::StorageProvider::MicrosoftOneDrive => Some(OAuthProviderConfig {
            authorization_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            client_id: client_id.to_string(),
            client_secret: client_secret.map(String::from),
            // rclone addresses the selected drive by ID and performs normal
            // filesystem operations, which AppFolder-only permission cannot do.
            scopes: "Files.ReadWrite User.Read offline_access",
            extra_auth_params: &[],
        }),
        crate::models::application_bundle::StorageProvider::Dropbox => Some(OAuthProviderConfig {
            authorization_endpoint: "https://www.dropbox.com/oauth2/authorize",
            token_endpoint: "https://api.dropboxapi.com/oauth2/token",
            client_id: client_id.to_string(),
            client_secret: client_secret.map(String::from),
            scopes: "",
            extra_auth_params: &[("token_access_type", "offline")],
        }),
        crate::models::application_bundle::StorageProvider::Box => Some(OAuthProviderConfig {
            authorization_endpoint: "https://account.box.com/api/oauth2/authorize",
            token_endpoint: "https://api.box.com/oauth2/token",
            client_id: client_id.to_string(),
            client_secret: client_secret.map(String::from),
            scopes: "",
            extra_auth_params: &[],
        }),
        _ => None,
    }
}

/// Result of a completed OAuth authorization code exchange.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub token_type: String,
    pub scope: Option<String>,
    /// Some providers nest the error in a JSON object.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    config: &OAuthProviderConfig,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthTokenResponse, String> {
    let client = reqwest::Client::new();
    let mut params: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", &config.client_id),
        ("code_verifier", code_verifier),
    ];
    add_token_scopes(config, &mut params);

    // Some providers require client_secret for confidential clients
    let secret_holder;
    if let Some(secret) = &config.client_secret {
        secret_holder = secret.clone();
        params.push(("client_secret", &secret_holder));
    }

    let response = client
        .post(config.token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::warn!("OAuth token exchange HTTP {status}");
        // Try to extract error from JSON body
        if let Ok(err_resp) = serde_json::from_str::<serde_json::Value>(&body) {
            if err_resp.get("error").and_then(|v| v.as_str()) == Some("invalid_grant") {
                return Err(
                    "Token exchange failed: authorization expired or was revoked; authorize again."
                        .into(),
                );
            }
        }
        return Err(format!("Token exchange failed: HTTP {status}"));
    }

    let raw_body = response
        .text()
        .await
        .map_err(|e| format!("Failed reading response: {e}"))?;

    let token: OAuthTokenResponse = serde_json::from_str(&raw_body)
        .map_err(|e| format!("Failed to parse OAuth token response: {e}"))?;

    // Check if the response contains an error
    if token.error.is_some() {
        return Err("Token exchange returned an OAuth error; authorize again.".into());
    }

    Ok(token)
}

/// Refresh an OAuth access token using a refresh token.
pub async fn refresh_token(
    config: &OAuthProviderConfig,
    refresh_token: &str,
) -> Result<OAuthTokenResponse, String> {
    let client = reqwest::Client::new();
    let mut params: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &config.client_id),
    ];
    add_token_scopes(config, &mut params);

    let secret_holder;
    if let Some(secret) = &config.client_secret {
        secret_holder = secret.clone();
        params.push(("client_secret", &secret_holder));
    }

    let response = client
        .post(config.token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token refresh failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        tracing::warn!("OAuth token refresh HTTP {status}");
        return Err(format!("Token refresh failed: HTTP {status}"));
    }

    let raw_body = response
        .text()
        .await
        .map_err(|e| format!("Failed reading response: {e}"))?;
    let token: OAuthTokenResponse = serde_json::from_str(&raw_body)
        .map_err(|e| format!("Failed to parse token refresh response: {e}"))?;

    if token.error.is_some() {
        return Err("Token refresh returned an OAuth error; authorize again.".into());
    }

    Ok(token)
}

fn add_token_scopes<'a>(config: &'a OAuthProviderConfig, params: &mut Vec<(&'a str, &'a str)>) {
    if !config.scopes.is_empty() {
        params.push(("scope", config.scopes));
    }
}

/// Resolve the concrete drive selected by Microsoft for the authorized user.
/// Rclone cannot construct a OneDrive filesystem from an OAuth token alone.
pub async fn discover_onedrive_drive(access_token: &str) -> Result<(String, String), String> {
    let response = reqwest::Client::new()
        .get("https://graph.microsoft.com/v1.0/me/drive")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| "OneDrive metadata request failed".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("OneDrive metadata request failed: HTTP {status}"));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Invalid OneDrive metadata response".to_string())?;
    parse_onedrive_drive(&value)
}

fn parse_onedrive_drive(value: &serde_json::Value) -> Result<(String, String), String> {
    let drive_id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
        .ok_or_else(|| "OneDrive metadata did not contain a valid drive ID.".to_string())?;
    let drive_type = value
        .get("driveType")
        .and_then(serde_json::Value::as_str)
        .filter(|value| matches!(*value, "personal" | "business" | "documentLibrary"))
        .ok_or_else(|| "OneDrive metadata contained an unsupported drive type.".to_string())?;
    Ok((drive_id.to_owned(), drive_type.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_verifier_generation() {
        let verifier = generate_code_verifier();
        assert!(verifier.len() >= 43); // PKCE minimum
        let challenge = derive_code_challenge(&verifier);
        assert!(challenge.len() == 43);
    }

    #[test]
    fn test_state_generation() {
        let state = generate_state();
        assert!(state.len() >= 32);
    }

    #[test]
    fn test_google_config() {
        use crate::models::application_bundle::StorageProvider;
        let config =
            get_oauth_config(&StorageProvider::GoogleDrive, "test-client-id", None).unwrap();
        assert!(config.authorization_endpoint.contains("google"));
        assert!(config.token_endpoint.contains("google"));
        assert!(config.scopes.contains("drive"));
        assert_eq!(config.client_id, "test-client-id");
    }

    #[test]
    fn test_none_for_non_oauth() {
        use crate::models::application_bundle::StorageProvider;
        assert!(get_oauth_config(&StorageProvider::BackblazeB2, "", None).is_none());
        assert!(get_oauth_config(&StorageProvider::AmazonS3, "", None).is_none());
        assert!(get_oauth_config(&StorageProvider::GoogleDrive, "id", None).is_some());
    }

    #[test]
    fn onedrive_scope_is_consistent_for_authorize_exchange_and_refresh() {
        use crate::models::application_bundle::StorageProvider;
        let config = get_oauth_config(
            &StorageProvider::MicrosoftOneDrive,
            "synthetic-client",
            None,
        )
        .unwrap();
        for redirect in ["http://127.0.0.1:17770/callback", "noland://oauth/callback"] {
            let url =
                config.build_authorization_url(redirect, "synthetic-state", "synthetic-challenge");
            assert!(url.contains(&format!("redirect_uri={}", urlencoding(redirect))));
            assert!(url.contains(&format!("scope={}", urlencoding(config.scopes))));
            assert!(url.contains("code_challenge_method=S256"));
        }
        assert!(config
            .scopes
            .split_whitespace()
            .any(|scope| scope == "Files.ReadWrite"));
        assert!(config.scopes.contains("offline_access"));
        assert!(!config.scopes.contains("AppFolder"));
        for grant in ["authorization_code", "refresh_token"] {
            let mut params = vec![("grant_type", grant)];
            add_token_scopes(&config, &mut params);
            assert_eq!(params.last(), Some(&("scope", config.scopes)));
        }
    }

    #[test]
    fn onedrive_metadata_requires_supported_concrete_drive_identity() {
        for drive_type in ["personal", "business", "documentLibrary"] {
            assert_eq!(
                parse_onedrive_drive(
                    &serde_json::json!({"id":"synthetic-drive", "driveType":drive_type})
                )
                .unwrap(),
                ("synthetic-drive".into(), drive_type.into())
            );
        }
        for value in [
            serde_json::json!({"driveType":"personal"}),
            serde_json::json!({"id":" ", "driveType":"business"}),
            serde_json::json!({"id":"drive\nconfiguration", "driveType":"personal"}),
            serde_json::json!({"id":"drive", "driveType":"sharepoint"}),
            serde_json::json!({"id":"drive", "driveType":7}),
        ] {
            assert!(parse_onedrive_drive(&value).is_err());
        }
    }
}
