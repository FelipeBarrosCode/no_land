use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;
use rand_core::{OsRng, RngCore};

use crate::moonlight::{
    application::bootstrap::load_existing_identity,
    domain::{MoonlightError, PairingStatus, PersistedPairing},
    infrastructure::{
        gamestream::{pair_host, PairHostRequest},
        persistence::{JsonMoonlightStateRepository, MoonlightStateRepository},
        secrets::SecretStore,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PairingSessionId(pub String);

#[derive(Debug, Clone)]
pub struct PairingSession {
    pub id: PairingSessionId,
    pub host_id: String,
    pub pin: String,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct PairingSessionStore {
    sessions: Arc<Mutex<HashMap<String, PairingSession>>>,
}

impl Default for PairingSessionStore {
    fn default() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingResult {
    pub host_id: String,
    pub persisted: bool,
}

pub async fn begin_pairing(
    repository: &JsonMoonlightStateRepository,
    sessions: &PairingSessionStore,
    host_id: &str,
) -> Result<PairingSession, MoonlightError> {
    repository.get_host(host_id)?;

    let session = PairingSession {
        id: PairingSessionId(random_hex_16()),
        host_id: host_id.to_string(),
        pin: random_pin(),
        expires_at: Instant::now() + Duration::from_secs(300),
    };

    let mut guard = sessions
        .sessions
        .lock()
        .map_err(|_| MoonlightError::Persistence("pairing session mutex poisoned".to_string()))?;
    guard.retain(|_, value| value.expires_at > Instant::now());
    guard.insert(session.id.0.clone(), session.clone());
    Ok(session)
}

pub async fn complete_pairing(
    repository: &JsonMoonlightStateRepository,
    secret_store: &dyn SecretStore,
    sessions: &PairingSessionStore,
    session_id: &PairingSessionId,
) -> Result<PairingResult, MoonlightError> {
    let session = {
        let mut guard = sessions.sessions.lock().map_err(|_| {
            MoonlightError::Persistence("pairing session mutex poisoned".to_string())
        })?;
        let session = guard
            .remove(&session_id.0)
            .ok_or_else(|| MoonlightError::Validation("pairing session not found".to_string()))?;
        if session.expires_at <= Instant::now() {
            return Err(MoonlightError::Validation(
                "pairing session has expired".to_string(),
            ));
        }
        session
    };

    let snapshot = repository.snapshot()?;
    let host = snapshot
        .hosts
        .get(&session.host_id)
        .cloned()
        .ok_or_else(|| MoonlightError::Validation(format!("host {} not found", session.host_id)))?;
    let persisted_identity = snapshot.identity.ok_or_else(|| {
        MoonlightError::IdentityInvalid("Moonlight identity is missing".to_string())
    })?;
    let identity = load_existing_identity(persisted_identity, secret_store).await?;
    let address = host
        .addresses
        .overlay
        .clone()
        .or(host.addresses.lan.clone())
        .or(host.addresses.external.clone())
        .ok_or_else(|| {
            MoonlightError::Validation(format!("host {} has no usable address", session.host_id))
        })?;
    let result = pair_host(PairHostRequest {
        address,
        http_port: host.ports.http,
        https_port: host.ports.https,
        unique_id: identity.unique_id.clone(),
        pin: session.pin.clone(),
        client_certificate_pem: identity.certificate_pem.clone(),
        client_private_key_pem: identity.private_key_pem.clone(),
        server_app_version: host
            .server_info_cache
            .as_ref()
            .map(|cache| cache.app_version.clone()),
    })
    .await?;

    repository.update(|configuration| {
        let host = configuration
            .hosts
            .get_mut(&session.host_id)
            .ok_or_else(|| {
                MoonlightError::Validation(format!("host {} not found", session.host_id))
            })?;
        host.pairing = Some(PersistedPairing {
            status: PairingStatus::Paired,
            server_certificate_pem: result.server_certificate_pem.clone(),
            server_certificate_sha256: result.server_certificate_sha256.clone(),
            paired_at: Utc::now().to_rfc3339(),
        });
        Ok(())
    })?;

    Ok(PairingResult {
        host_id: session.host_id,
        persisted: true,
    })
}

fn random_pin() -> String {
    let mut bytes = [0u8; 4];
    OsRng.fill_bytes(&mut bytes);
    let number = u32::from_be_bytes(bytes) % 10_000;
    format!("{number:04}")
}

fn random_hex_16() -> String {
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    let mut out = String::with_capacity(16);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{begin_pairing, PairingSessionStore};
    use crate::moonlight::{
        application::hosts::{register_host, RegisterHostRequest},
        domain::{HostAddresses, HostPorts},
        infrastructure::persistence::JsonMoonlightStateRepository,
    };

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-moonlight-pairing-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&base).unwrap();
        base.join("state.json")
    }

    #[tokio::test]
    async fn creates_pairing_session() {
        let repo = JsonMoonlightStateRepository::new(temp_state_path("pair"));
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

        let store = PairingSessionStore::default();
        let session = begin_pairing(&repo, &store, "host-1").await.unwrap();
        assert_eq!(session.pin.len(), 4);
        assert_eq!(session.host_id, "host-1");
    }
}
