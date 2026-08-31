use std::time::Duration;

use chrono::Utc;

use crate::moonlight::{
    application::bootstrap::bootstrap_client_identity,
    domain::{CachedRemoteApp, MoonlightError, RemoteApp},
    infrastructure::{
        gamestream::{
            parse_app_list_response, GameStreamHttpClient, GameStreamRequest, GameStreamScheme,
            PinnedCertificate,
        },
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
        secrets::SecretStore,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicy {
    UseCacheIfFresh,
    ForceRefresh,
}

pub async fn list_remote_apps(
    repository: &JsonMoonlightStateRepository,
    secret_store: &dyn SecretStore,
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
    let pairing = host
        .pairing
        .clone()
        .ok_or_else(|| MoonlightError::Validation(format!("host {host_id} is not paired")))?;
    let identity = bootstrap_client_identity(repository, secret_store)
        .await?
        .identity
        .persisted();
    let response = client
        .execute(GameStreamRequest {
            address,
            port: host.ports.https.unwrap_or(host.ports.http),
            scheme: GameStreamScheme::Https,
            endpoint: "/applist".to_string(),
            query: vec![],
            identity: Some(
                crate::moonlight::infrastructure::gamestream::ClientIdentityReference {
                    certificate_pem: identity.certificate_pem.clone(),
                    private_key_ref: identity.private_key_ref.clone(),
                },
            ),
            pinned_certificate: Some(PinnedCertificate {
                sha256_hex: pairing.server_certificate_sha256.clone(),
                certificate_pem: pairing.server_certificate_pem.clone(),
            }),
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::Mutex};

    use async_trait::async_trait;

    use super::{list_remote_apps, RefreshPolicy};
    use crate::moonlight::{
        application::hosts::{register_host, RegisterHostRequest},
        domain::{HostAddresses, HostPorts, MoonlightError},
        infrastructure::{
            gamestream::{GameStreamHttpClient, GameStreamRequest, GameStreamResponse},
            persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
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
        requests: Mutex<Vec<GameStreamRequest>>,
    }

    #[async_trait]
    impl GameStreamHttpClient for FakeClient {
        async fn execute(
            &self,
            request: GameStreamRequest,
        ) -> Result<GameStreamResponse, MoonlightError> {
            self.requests.lock().unwrap().push(request);
            Ok(GameStreamResponse {
                body: self.body.clone(),
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

        let secrets =
            crate::moonlight::infrastructure::secrets::testsupport::InMemorySecretStore::default();
        let _ =
            crate::moonlight::application::bootstrap::bootstrap_client_identity(&repo, &secrets)
                .await
                .unwrap();
        repo.update(|configuration| {
            let host = configuration.hosts.get_mut("host-1").unwrap();
            host.pairing = Some(crate::moonlight::domain::PersistedPairing {
                status: crate::moonlight::domain::PairingStatus::Paired,
                server_certificate_pem: "server-cert".to_string(),
                server_certificate_sha256: "deadbeef".to_string(),
                paired_at: "now".to_string(),
            });
            Ok(())
        })
        .unwrap();
        let client = FakeClient {
            body: r#"<root><status_code>200</status_code><status_message>OK</status_message><App><ID>1</ID><AppTitle>Steam</AppTitle><IsHdrSupported>1</IsHdrSupported></App></root>"#.to_string(),
            requests: Mutex::new(Vec::new()),
        };
        let apps = list_remote_apps(
            &repo,
            &secrets,
            &client,
            "host-1",
            RefreshPolicy::ForceRefresh,
        )
        .await
        .unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Steam");

        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(matches!(
            request.scheme,
            crate::moonlight::infrastructure::gamestream::GameStreamScheme::Https
        ));
        assert_eq!(request.endpoint, "/applist");
        assert!(request.identity.is_some());
        assert!(request.pinned_certificate.is_some());
    }
}
