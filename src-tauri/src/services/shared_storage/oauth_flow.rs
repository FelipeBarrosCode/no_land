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
            scopes: "Files.ReadWrite.AppFolder User.Read offline_access",
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
        tracing::warn!("OAuth token exchange HTTP {status}: {body}");
        // Try to extract error from JSON body
        if let Ok(err_resp) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(err) = err_resp.get("error").and_then(|v| v.as_str()) {
                let desc = err_resp
                    .get("error_description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                return Err(format!("Token exchange failed: {err} — {desc}"));
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
        return Err(format!(
            "Token exchange returned error: {} — {}",
            token.error.unwrap_or_default(),
            token.error_description.unwrap_or_default()
        ));
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
        let body = response.text().await.unwrap_or_default();
        tracing::warn!("OAuth token refresh HTTP {status}: {body}");
        return Err(format!("Token refresh failed: HTTP {status}"));
    }

    let raw_body = response
        .text()
        .await
        .map_err(|e| format!("Failed reading response: {e}"))?;
    let token: OAuthTokenResponse = serde_json::from_str(&raw_body)
        .map_err(|e| format!("Failed to parse token refresh response: {e}"))?;

    if token.error.is_some() {
        return Err(format!(
            "Token refresh returned error: {}",
            token.error.unwrap_or_default()
        ));
    }

    Ok(token)
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
}
