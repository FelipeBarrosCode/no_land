use std::{
    net::IpAddr,
    sync::{Mutex, OnceLock},
};

use chrono::{Duration, Utc};
use keyring::Entry;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

const KEYRING_SERVICE: &str = "com.noland.connect.cloudflare-turn";
const KEYRING_ACCOUNT: &str = "default";
const CREDENTIAL_REF: &str = "secure-store://cloudflare-turn/default";
static CACHED_SECRET: OnceLock<Mutex<Option<Option<(String, String)>>>> = OnceLock::new();
const CLOUDFLARE_TURN_API_BASE: &str = "https://rtc.live.cloudflare.com";
const MAX_TTL_SECONDS: u32 = 48 * 60 * 60;
const VALIDATION_TTL_SECONDS: u32 = 60 * 60;
pub const RUNTIME_CREDENTIAL_TTL_SECONDS: u32 = 24 * 60 * 60;

#[derive(Clone, Serialize, Deserialize)]
struct StoredTurnSecret {
    key_id: String,
    api_token: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareTurnSettingsUpdate {
    pub enabled: bool,
    pub key_id: String,
    pub api_token: String,
}

impl std::fmt::Debug for CloudflareTurnSettingsUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudflareTurnSettingsUpdate")
            .field("enabled", &self.enabled)
            .field("key_id", &key_id_hint(&self.key_id))
            .field("api_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareTurnSettingsResponse {
    pub enabled: bool,
    pub status: String,
    pub key_id_hint: Option<String>,
    pub token_set: bool,
    pub credential_ref: String,
    pub last_validated_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareTurnTestResult {
    pub valid: bool,
    pub udp_urls: Vec<String>,
    pub credential_expires_at: String,
}

/// Short-lived credential material. This value must remain memory-only.
pub struct GeneratedTurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateCredentialsResponse {
    ice_servers: Vec<IceServer>,
}

#[derive(Debug, Deserialize)]
struct IceServer {
    #[serde(default)]
    urls: IceServerUrls,
    username: Option<String>,
    credential: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum IceServerUrls {
    One(String),
    Many(Vec<String>),
    #[default]
    Missing,
}

impl IceServerUrls {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(url) => vec![url],
            Self::Many(urls) => urls,
            Self::Missing => Vec::new(),
        }
    }
}

pub fn credential_ref() -> &'static str {
    CREDENTIAL_REF
}

pub fn load_secret() -> AppResult<Option<(String, String)>> {
    let cache = CACHED_SECRET.get_or_init(|| Mutex::new(None));
    if let Some(cached) = cache
        .lock()
        .map_err(|_| AppError::State("Cloudflare TURN credential cache is poisoned".to_string()))?
        .clone()
    {
        return Ok(cached);
    }
    let entry = keyring_entry()?;
    let result = match entry.get_password() {
        Ok(serialized) => {
            let secret: StoredTurnSecret = serde_json::from_str(&serialized).map_err(|error| {
                AppError::State(format!(
                    "Stored Cloudflare TURN credentials are invalid: {error}"
                ))
            })?;
            Ok(Some((secret.key_id, secret.api_token)))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(AppError::State(format!(
            "Could not read Cloudflare TURN credentials from secure storage: {error}"
        ))),
    };
    match result {
        Ok(value) => {
            if let Ok(mut guard) = cache.lock() {
                *guard = Some(value.clone());
            }
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

pub fn store_secret(key_id: &str, api_token: &str) -> AppResult<()> {
    let secret = StoredTurnSecret {
        key_id: key_id.to_string(),
        api_token: api_token.to_string(),
    };
    let serialized = serde_json::to_string(&secret)?;
    keyring_entry()?
        .set_password(&serialized)
        .map_err(|error| {
            AppError::State(format!(
                "Could not save Cloudflare TURN credentials in secure storage: {error}"
            ))
        })?;

    // Force the read-back to consult the keychain after replacing credentials.
    if let Ok(mut guard) = CACHED_SECRET.get_or_init(|| Mutex::new(None)).lock() {
        *guard = None;
    }

    // Do not report success based only on the write call. In development
    // builds macOS Keychain permissions can allow the provider validation but
    // reject or redirect the persistence operation. Read the value back using
    // the exact same keyring entry before the command reports success.
    let stored = load_secret()?.ok_or_else(|| {
        AppError::State(
            "Cloudflare TURN credentials were accepted but could not be read back from secure storage"
                .to_string(),
        )
    })?;
    if stored.0 != key_id || stored.1 != api_token {
        return Err(AppError::State(
            "Cloudflare TURN credentials did not match the value read back from secure storage"
                .to_string(),
        ));
    }
    if let Ok(mut guard) = CACHED_SECRET.get_or_init(|| Mutex::new(None)).lock() {
        *guard = Some(Some((key_id.to_string(), api_token.to_string())));
    }
    Ok(())
}

pub fn delete_secret() -> AppResult<()> {
    let entry = keyring_entry()?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(AppError::State(format!(
            "Could not remove Cloudflare TURN credentials from secure storage: {error}"
        ))),
    }?;
    if let Ok(mut guard) = CACHED_SECRET.get_or_init(|| Mutex::new(None)).lock() {
        *guard = Some(None);
    }
    Ok(())
}

fn keyring_entry() -> AppResult<Entry> {
    Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| {
        AppError::State(format!(
            "Could not access secure storage for Cloudflare TURN credentials: {error}"
        ))
    })
}

pub async fn generate_credentials(
    client: &reqwest::Client,
    key_id: &str,
    api_token: &str,
    ttl_seconds: u32,
) -> AppResult<GeneratedTurnCredentials> {
    let key_id = key_id.trim();
    let api_token = api_token.trim();
    if key_id.is_empty() || api_token.is_empty() {
        return Err(AppError::InvalidInput(
            "Cloudflare TURN Key ID and API token are required".to_string(),
        ));
    }
    if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
        return Err(AppError::InvalidInput(format!(
            "Cloudflare TURN credential TTL must be between 1 and {MAX_TTL_SECONDS} seconds"
        )));
    }

    let url = format!(
        "{CLOUDFLARE_TURN_API_BASE}/v1/turn/keys/{}/credentials/generate-ice-servers",
        urlencoding::encode(key_id)
    );
    let response = client
        .post(url)
        .bearer_auth(api_token)
        .json(&serde_json::json!({ "ttl": ttl_seconds }))
        .send()
        .await?;
    if response.status() != StatusCode::CREATED {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let detail = redact_provider_error(&body);
        return Err(
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                AppError::InvalidInput(
                    "Cloudflare rejected the TURN Key ID or API token".to_string(),
                )
            } else {
                AppError::Api(format!(
                    "Cloudflare TURN credential generation returned {status}: {detail}"
                ))
            },
        );
    }

    let body: GenerateCredentialsResponse = response.json().await?;
    parse_generated_credentials(body, ttl_seconds)
}

pub async fn generate_runtime_credentials(
    client: &reqwest::Client,
) -> AppResult<GeneratedTurnCredentials> {
    let (key_id, api_token) = tokio::task::spawn_blocking(load_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??
        .ok_or_else(|| {
            AppError::InvalidInput(
                "Cloudflare TURN credentials are missing from secure storage".to_string(),
            )
        })?;
    generate_credentials(client, &key_id, &api_token, RUNTIME_CREDENTIAL_TTL_SECONDS).await
}

pub async fn discover_client_public_ip(_client: &reqwest::Client) -> AppResult<IpAddr> {
    // The host TURN client currently allocates over IPv4. Force this lookup
    // over IPv4 as well so the permission matches the source address of the
    // desktop's UDP packets to the relayed endpoint on dual-stack networks.
    let ipv4_client = reqwest::Client::builder()
        .local_address(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .build()?;
    let body = ipv4_client
        .get("https://www.cloudflare.com/cdn-cgi/trace")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    body.lines()
        .find_map(|line| line.strip_prefix("ip="))
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .filter(IpAddr::is_ipv4)
        .ok_or_else(|| {
            AppError::Api(
                "Cloudflare did not report a valid client public IP for TURN permission setup"
                    .to_string(),
            )
        })
}

pub async fn test_credentials(
    client: &reqwest::Client,
    key_id: &str,
    api_token: &str,
) -> AppResult<CloudflareTurnTestResult> {
    let generated = generate_credentials(client, key_id, api_token, VALIDATION_TTL_SECONDS).await?;
    Ok(CloudflareTurnTestResult {
        valid: true,
        udp_urls: generated.urls,
        credential_expires_at: generated.expires_at,
    })
}

fn parse_generated_credentials(
    body: GenerateCredentialsResponse,
    ttl_seconds: u32,
) -> AppResult<GeneratedTurnCredentials> {
    for server in body.ice_servers {
        let urls = server.urls.into_vec();
        let udp_urls = urls
            .into_iter()
            .filter(|url| url.starts_with("turn:") && url.contains("transport=udp"))
            .collect::<Vec<_>>();
        if udp_urls.is_empty() {
            continue;
        }
        let Some(username) = server.username.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let Some(credential) = server.credential.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        return Ok(GeneratedTurnCredentials {
            urls: udp_urls,
            username,
            credential,
            expires_at: (Utc::now() + Duration::seconds(ttl_seconds as i64)).to_rfc3339(),
        });
    }
    Err(AppError::Api(
        "Cloudflare TURN response did not contain a usable UDP TURN credential".to_string(),
    ))
}

pub fn key_id_hint(key_id: &str) -> String {
    let chars = key_id.chars().collect::<Vec<_>>();
    if chars.len() <= 8 {
        return "••••".to_string();
    }
    format!(
        "{}…{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

fn redact_provider_error(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "empty response".to_string()
    } else {
        "provider returned an error response".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_udp_turn_urls() {
        let generated = parse_generated_credentials(
            GenerateCredentialsResponse {
                ice_servers: vec![
                    IceServer {
                        urls: IceServerUrls::Many(vec!["stun:stun.cloudflare.com:3478".into()]),
                        username: None,
                        credential: None,
                    },
                    IceServer {
                        urls: IceServerUrls::Many(vec![
                            "turn:turn.cloudflare.com:3478?transport=udp".into(),
                            "turn:turn.cloudflare.com:3478?transport=tcp".into(),
                        ]),
                        username: Some("temporary-user".into()),
                        credential: Some("temporary-password".into()),
                    },
                ],
            },
            3600,
        )
        .unwrap();
        assert_eq!(
            generated.urls,
            vec!["turn:turn.cloudflare.com:3478?transport=udp"]
        );
        assert_eq!(generated.username, "temporary-user");
    }

    #[test]
    fn key_hint_does_not_reveal_the_full_identifier() {
        assert_eq!(key_id_hint("1234567890abcdef"), "1234…cdef");
        assert_eq!(key_id_hint("short"), "••••");
    }
}
