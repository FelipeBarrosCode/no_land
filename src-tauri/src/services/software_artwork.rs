use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    models::launch_library::{software_artwork_key, SoftwareArtworkResult},
    services::app_config::IgdbConfig,
};

const TWITCH_TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const IGDB_GAMES_URL: &str = "https://api.igdb.com/v4/games";

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    refresh_at: Instant,
}

#[derive(Debug, Deserialize)]
struct TwitchTokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct IgdbGame {
    #[serde(default)]
    artworks: Vec<IgdbImage>,
    #[serde(default)]
    screenshots: Vec<IgdbImage>,
    cover: Option<IgdbImage>,
}

#[derive(Debug, Deserialize)]
struct IgdbImage {
    image_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedArtworkCache {
    entries: HashMap<String, SoftwareArtworkResult>,
}

pub struct SoftwareArtworkService {
    config: IgdbConfig,
    client: reqwest::Client,
    cache_path: PathBuf,
    cache: RwLock<HashMap<String, SoftwareArtworkResult>>,
    token: Mutex<Option<CachedToken>>,
}

impl SoftwareArtworkService {
    pub fn new(config: IgdbConfig, client: reqwest::Client, cache_path: PathBuf) -> Self {
        let entries = std::fs::read(&cache_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PersistedArtworkCache>(&bytes).ok())
            .map(|cache| cache.entries)
            .unwrap_or_default();
        Self {
            config,
            client,
            cache_path,
            cache: RwLock::new(entries),
            token: Mutex::new(None),
        }
    }

    pub async fn get(&self, name: &str) -> SoftwareArtworkResult {
        let key = software_artwork_key(name);
        if let Some(cached) = self.cache.read().await.get(&key).cloned() {
            return cached;
        }

        if self.config.twitch_client_id.is_none() || self.config.twitch_client_secret.is_none() {
            return placeholder(key);
        }

        match self.lookup_igdb(name).await {
            Ok(image_url) => {
                let source = if image_url.is_some() {
                    "igdb".to_string()
                } else {
                    "placeholder".to_string()
                };
                let result = SoftwareArtworkResult {
                    key: key.clone(),
                    image_url,
                    source,
                };
                self.cache_result(key, result.clone()).await;
                result
            }
            Err(error) => {
                warn!(%error, "IGDB artwork lookup failed; returning placeholder artwork");
                placeholder(key)
            }
        }
    }

    async fn lookup_igdb(&self, name: &str) -> AppResult<Option<String>> {
        let client_id = self
            .config
            .twitch_client_id
            .as_deref()
            .ok_or_else(|| AppError::State("IGDB client ID is not configured".to_string()))?;
        let access_token = self.access_token().await?;
        let escaped_name = name.trim().replace('\\', "\\\\").replace('"', "\\\"");
        let query = format!(
            "search \"{escaped_name}\"; fields artworks.image_id,screenshots.image_id,cover.image_id; limit 1;"
        );
        let response = self
            .client
            .post(IGDB_GAMES_URL)
            .header("Client-ID", client_id)
            .bearer_auth(access_token)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(query)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AppError::Api(format!(
                "IGDB games request returned HTTP {}",
                response.status()
            )));
        }
        let games = response.json::<Vec<IgdbGame>>().await?;
        Ok(games
            .into_iter()
            .next()
            .and_then(|game| {
                game.artworks
                    .into_iter()
                    .next()
                    .or_else(|| game.screenshots.into_iter().next())
                    .or(game.cover)
            })
            .map(|image| {
                format!(
                    "https://images.igdb.com/igdb/image/upload/t_1080p/{}.jpg",
                    image.image_id
                )
            }))
    }

    async fn access_token(&self) -> AppResult<String> {
        let mut token = self.token.lock().await;
        if let Some(cached) = token.as_ref() {
            if Instant::now() < cached.refresh_at {
                return Ok(cached.access_token.clone());
            }
        }

        let client_id = self
            .config
            .twitch_client_id
            .as_deref()
            .ok_or_else(|| AppError::State("IGDB client ID is not configured".to_string()))?;
        let client_secret =
            self.config.twitch_client_secret.as_deref().ok_or_else(|| {
                AppError::State("IGDB client secret is not configured".to_string())
            })?;
        let response = self
            .client
            .post(TWITCH_TOKEN_URL)
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AppError::Api(format!(
                "Twitch token request returned HTTP {}",
                response.status()
            )));
        }
        let response = response.json::<TwitchTokenResponse>().await?;
        let refresh_in = response.expires_in.saturating_sub(60).max(1);
        *token = Some(CachedToken {
            access_token: response.access_token.clone(),
            refresh_at: Instant::now() + Duration::from_secs(refresh_in),
        });
        Ok(response.access_token)
    }

    async fn cache_result(&self, key: String, result: SoftwareArtworkResult) {
        let snapshot = {
            let mut cache = self.cache.write().await;
            cache.insert(key, result);
            cache.clone()
        };
        let persisted = PersistedArtworkCache { entries: snapshot };
        let Ok(bytes) = serde_json::to_vec_pretty(&persisted) else {
            return;
        };
        if let Some(parent) = self.cache_path.parent() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                warn!(%error, "Failed creating software artwork cache directory");
                return;
            }
        }
        if let Err(error) = tokio::fs::write(&self.cache_path, bytes).await {
            warn!(%error, "Failed writing software artwork cache");
        }
    }
}

fn placeholder(key: String) -> SoftwareArtworkResult {
    SoftwareArtworkResult {
        key,
        image_url: None,
        source: "placeholder".to_string(),
    }
}
