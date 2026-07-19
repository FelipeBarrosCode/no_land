use std::{fs, path::PathBuf, time::Duration};

use chrono::Utc;

use crate::moonlight::{
    domain::{AppArtwork, CachedRemoteApp, MoonlightError, RemoteApp},
    infrastructure::{
        gamestream::{
            parse_app_list_response, GameStreamHttpClient, GameStreamRequest, GameStreamScheme,
            RemoteAppAssetEndpoint,
        },
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicy {
    UseCacheIfFresh,
    ForceRefresh,
}

pub async fn list_remote_apps(
    repository: &JsonMoonlightStateRepository,
    client: &impl GameStreamHttpClient,
    host_id: &str,
    refresh: RefreshPolicy,
) -> Result<Vec<RemoteApp>, MoonlightError> {
    let host = repository.get_host(host_id)?;

    if matches!(refresh, RefreshPolicy::UseCacheIfFresh) {
        if let Some(cache) = host.apps_cache {
            if !cache.items.is_empty() {
                return Ok(cache
                    .items
                    .into_iter()
                    .map(|item| RemoteApp {
                        id: item.id,
                        name: item.name,
                        hdr_supported: item.hdr_supported,
                    })
                    .collect());
            }
        }
    }

    let address = host
        .addresses
        .overlay
        .or(host.addresses.lan)
        .or(host.addresses.external)
        .ok_or_else(|| {
            MoonlightError::Validation(format!("host {host_id} has no usable address"))
        })?;
    let response = client
        .execute(GameStreamRequest {
            address,
            port: host.ports.http,
            scheme: GameStreamScheme::Http,
            endpoint: "/applist".to_string(),
            query: vec![],
            identity: None,
            pinned_certificate: None,
            timeout: Duration::from_secs(10),
        })
        .await?;

    let apps = parse_app_list_response(&response.body)?;
    repository.update(|configuration| {
        let host = configuration
            .hosts
            .get_mut(host_id)
            .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} not found")))?;
        host.apps_cache = Some(crate::moonlight::domain::AppsCache {
            fetched_at: Utc::now().to_rfc3339(),
            items: apps
                .iter()
                .map(|app| CachedRemoteApp {
                    id: app.id,
                    name: app.name.clone(),
                    hdr_supported: app.hdr_supported,
                })
                .collect(),
        });
        Ok(())
    })?;

    Ok(apps)
}

pub async fn get_remote_app_artwork(
    repository: &JsonMoonlightStateRepository,
    client: &impl GameStreamHttpClient,
    app_data_dir: &std::path::Path,
    host_id: &str,
    app_id: u32,
) -> Result<AppArtwork, MoonlightError> {
    let host = repository.get_host(host_id)?;
    let address = host
        .addresses
        .overlay
        .or(host.addresses.lan)
        .or(host.addresses.external)
        .ok_or_else(|| {
            MoonlightError::Validation(format!("host {host_id} has no usable address"))
        })?;

    let response = client
        .execute(GameStreamRequest {
            address,
            port: host.ports.http,
            scheme: GameStreamScheme::Http,
            endpoint: RemoteAppAssetEndpoint::default().path,
            query: vec![("appid".to_string(), app_id.to_string())],
            identity: None,
            pinned_certificate: None,
            timeout: Duration::from_secs(10),
        })
        .await?;

    let content_type = response
        .content_type
        .unwrap_or_else(|| "image/png".to_string());
    let bytes = response.body.into_bytes();

    let artwork_path = app_data_dir
        .join("moonlight")
        .join("artwork")
        .join(host_id)
        .join(format!("{app_id}.png"));
    if let Some(parent) = artwork_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&artwork_path, &bytes)?;

    Ok(AppArtwork {
        app_id,
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use async_trait::async_trait;

    use super::{list_remote_apps, RefreshPolicy};
    use crate::moonlight::{
        application::hosts::{register_host, RegisterHostRequest},
        domain::{HostAddresses, HostPorts, MoonlightError},
        infrastructure::{
            gamestream::{GameStreamHttpClient, GameStreamRequest, GameStreamResponse},
            persistence::JsonMoonlightStateRepository,
        },
    };

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-moonlight-apps-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&base).unwrap();
        base.join("state.json")
    }

    struct FakeClient {
        body: String,
    }

    #[async_trait]
    impl GameStreamHttpClient for FakeClient {
        async fn execute(
            &self,
            _request: GameStreamRequest,
        ) -> Result<GameStreamResponse, MoonlightError> {
            Ok(GameStreamResponse {
                status: 200,
                body: self.body.clone(),
                content_type: Some("application/xml".to_string()),
            })
        }
    }

    #[tokio::test]
    async fn fetches_and_caches_remote_apps() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("cache"));
        register_host(
            &repo,
            RegisterHostRequest {
                host_id: "host-1".to_string(),
                display_name: "Host".to_string(),
                addresses: HostAddresses {
                    overlay: Some("10.77.0.1".to_string()),
                    lan: None,
                    external: None,
                },
                ports: HostPorts {
                    http: 47989,
                    https: None,
                },
                explicit_address_type: None,
            },
        )
        .await
        .unwrap();

        let apps = list_remote_apps(&repo, &FakeClient { body: r#"<root><status_code>200</status_code><status_message>OK</status_message><App><ID>1</ID><AppTitle>Steam</AppTitle><IsHdrSupported>1</IsHdrSupported></App></root>"#.to_string() }, "host-1", RefreshPolicy::ForceRefresh).await.unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Steam");
    }
}
