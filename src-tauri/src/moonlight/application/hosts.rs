use std::time::Duration;

use chrono::Utc;

use crate::moonlight::{
    domain::{
        select_active_address, AddressType, HostAddresses, HostPorts, MoonlightError,
        PersistedHost, ServerInfoCache,
    },
    infrastructure::{
        gamestream::{
            parse_server_info_response, GameStreamHttpClient, GameStreamRequest, GameStreamScheme,
            PairStatus, ReqwestGameStreamHttpClient,
        },
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
    },
};

#[derive(Debug, Clone)]
pub struct RegisterHostRequest {
    pub host_id: String,
    pub display_name: String,
    pub addresses: HostAddresses,
    pub ports: HostPorts,
    pub explicit_address_type: Option<AddressType>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub host: PersistedHost,
    pub reachable: bool,
    pub pair_status: PairStatus,
}

pub async fn register_host(
    repository: &JsonMoonlightStateRepository,
    request: RegisterHostRequest,
) -> Result<PersistedHost, MoonlightError> {
    validate_host_registration(&request)?;

    repository.update(|configuration| {
        if configuration.hosts.contains_key(&request.host_id) {
            return Err(MoonlightError::Validation(format!(
                "host {} already exists",
                request.host_id
            )));
        }

        let active_address_type = request
            .explicit_address_type
            .clone()
            .unwrap_or_else(|| default_address_type(&request.addresses));

        let host = PersistedHost {
            host_id: request.host_id.clone(),
            display_name: request.display_name.clone(),
            addresses: request.addresses.clone(),
            active_address_type,
            ports: request.ports.clone(),
            pairing: None,
            server_info_cache: None,
            apps_cache: None,
            preferences_override: None,
            last_selected_app_id: None,
        };

        configuration
            .hosts
            .insert(request.host_id.clone(), host.clone());
        Ok(host)
    })
}

pub async fn refresh_host(
    repository: &JsonMoonlightStateRepository,
    client: &impl GameStreamHttpClient,
    host_id: &str,
) -> Result<HostStatus, MoonlightError> {
    let existing = repository.get_host(host_id)?;
    let selected = select_active_address(&existing, Some(existing.active_address_type.clone()))?;

    let response = client
        .execute(GameStreamRequest {
            address: selected.address,
            port: existing.ports.http,
            scheme: GameStreamScheme::Http,
            endpoint: "/serverinfo".to_string(),
            query: vec![],
            identity: None,
            pinned_certificate: None,
            timeout: Duration::from_secs(10),
        })
        .await?;

    let server_info = parse_server_info_response(&response.body)?;
    let pair_status = server_info.pair_status.clone();
    let https_port = server_info.https_port;
    let refreshed = repository.update(|configuration| {
        let host = configuration
            .hosts
            .get_mut(host_id)
            .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} not found")))?;

        host.active_address_type = selected.address_type.clone();
        host.ports.https = https_port;
        host.server_info_cache = Some(ServerInfoCache {
            app_version: server_info.app_version.clone(),
            gfe_version: server_info.gfe_version.clone(),
            server_codec_mode_support: server_info.server_codec_mode_support,
            current_game_id: server_info.current_game_id.unwrap_or(0),
            last_seen_at: Utc::now().to_rfc3339(),
        });
        Ok(host.clone())
    })?;

    Ok(HostStatus {
        host: refreshed,
        reachable: true,
        pair_status,
    })
}

fn validate_host_registration(request: &RegisterHostRequest) -> Result<(), MoonlightError> {
    if request.host_id.trim().is_empty() {
        return Err(MoonlightError::Validation(
            "host_id is required".to_string(),
        ));
    }
    if request.display_name.trim().is_empty() {
        return Err(MoonlightError::Validation(
            "display_name is required".to_string(),
        ));
    }
    if request.ports.http == 0 {
        return Err(MoonlightError::Validation(
            "http port must be non-zero".to_string(),
        ));
    }
    let has_any_address = request
        .addresses
        .overlay
        .as_deref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        || request
            .addresses
            .lan
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        || request
            .addresses
            .external
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    if !has_any_address {
        return Err(MoonlightError::Validation(
            "at least one host address is required".to_string(),
        ));
    }
    Ok(())
}

fn default_address_type(addresses: &HostAddresses) -> AddressType {
    if addresses
        .overlay
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        AddressType::Overlay
    } else if addresses
        .lan
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        AddressType::Lan
    } else {
        AddressType::External
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use async_trait::async_trait;

    use super::{refresh_host, register_host, RegisterHostRequest};
    use crate::moonlight::{
        domain::{AddressType, HostAddresses, HostPorts, MoonlightError},
        infrastructure::{
            gamestream::{GameStreamHttpClient, GameStreamResponse},
            persistence::JsonMoonlightStateRepository,
        },
    };

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-moonlight-hosts-{name}-{}",
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
            _request: crate::moonlight::infrastructure::gamestream::GameStreamRequest,
        ) -> Result<GameStreamResponse, MoonlightError> {
            Ok(GameStreamResponse {
                status: 200,
                body: self.body.clone(),
                content_type: Some("application/xml".to_string()),
            })
        }
    }

    #[tokio::test]
    async fn registers_host() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("register"));
        let host = register_host(
            &repo,
            RegisterHostRequest {
                host_id: "host-1".to_string(),
                display_name: "Quebec RTX 3090".to_string(),
                addresses: HostAddresses {
                    overlay: Some("10.77.0.1".to_string()),
                    lan: None,
                    external: None,
                },
                ports: HostPorts {
                    http: 47989,
                    https: None,
                },
                explicit_address_type: Some(AddressType::Overlay),
            },
        )
        .await
        .unwrap();
        assert_eq!(host.host_id, "host-1");
    }

    #[tokio::test]
    async fn refreshes_host_and_caches_server_info() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("refresh"));
        register_host(
            &repo,
            RegisterHostRequest {
                host_id: "host-1".to_string(),
                display_name: "Quebec RTX 3090".to_string(),
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

        let status = refresh_host(
            &repo,
            &FakeClient {
                body: r#"<root><status_code>200</status_code><status_message>OK</status_message><appversion>7.1.431.-1</appversion><HttpsPort>47984</HttpsPort><PairStatus>1</PairStatus><currentgame>0</currentgame><ServerCodecModeSupport>197889</ServerCodecModeSupport></root>"#.to_string(),
            },
            "host-1",
        )
        .await
        .unwrap();

        assert!(status.reachable);
        assert_eq!(status.host.ports.https, Some(47984));
        assert!(status.host.server_info_cache.is_some());
    }
}

pub async fn refresh_host_with_default_client(
    repository: &JsonMoonlightStateRepository,
    host_id: &str,
) -> Result<HostStatus, MoonlightError> {
    let client = ReqwestGameStreamHttpClient::new()?;
    refresh_host(repository, &client, host_id).await
}
