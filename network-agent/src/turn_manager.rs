use std::{
    fs::{self, File},
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use noland_network_contracts::{
    control::{InstallProbeSessionRequest, PrepareTurnRequest, PrepareTurnResponse},
    state::{HostNetworkState, NetworkEndpoint, TransportKind, TurnRuntimeStatus},
    NETWORK_STATE_SCHEMA_VERSION,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    turn_bridge::{TurnBridgeConfig, TurnBridgeHandle, TurnBridgeStats},
    SharedState,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);
const REPLACEMENT_OVERLAP: std::time::Duration = std::time::Duration::from_secs(30);

pub struct TurnManager {
    inner: Mutex<TurnManagerInner>,
    probe_sessions: SharedState,
    state_path: PathBuf,
    kernel_wireguard_addr: SocketAddr,
}

struct TurnManagerInner {
    state: HostNetworkState,
    bridge: Option<TurnBridgeHandle>,
    last_prepare: Option<(Uuid, PrepareTurnResponse)>,
    retirement_tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl TurnManager {
    pub fn new(
        instance_id: String,
        agent_version: String,
        state_path: PathBuf,
        kernel_wireguard_addr: SocketAddr,
        probe_sessions: SharedState,
    ) -> Result<Arc<Self>> {
        let previous_generation = load_previous_generation(&state_path);
        let state = HostNetworkState {
            schema_version: NETWORK_STATE_SCHEMA_VERSION,
            host_revision: 0,
            updated_at: Utc::now().to_rfc3339(),
            instance_id,
            session_id: Uuid::new_v4(),
            agent_version,
            turn_status: TurnRuntimeStatus::AwaitingCredentials,
            allocation_generation: previous_generation,
            relay_endpoint: None,
            allocation_expires_at: None,
            credential_expires_at: None,
            observed_transport: None,
        };
        persist_state(&state_path, &state)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(TurnManagerInner {
                state,
                bridge: None,
                last_prepare: None,
                retirement_tasks: Vec::new(),
            }),
            probe_sessions,
            state_path,
            kernel_wireguard_addr,
        }))
    }

    pub async fn prepare_turn(&self, request: PrepareTurnRequest) -> Result<PrepareTurnResponse> {
        let mut inner = self.inner.lock().await;
        inner.retirement_tasks.retain(|task| !task.is_finished());
        if let Some((operation_id, response)) = &inner.last_prepare {
            if *operation_id == request.operation_id {
                return Ok(response.clone());
            }
        }
        if request.requested_generation <= inner.state.allocation_generation {
            bail!(
                "requested allocation generation {} must exceed active generation {}",
                request.requested_generation,
                inner.state.allocation_generation
            );
        }
        if !(576..=1500).contains(&request.effective_mtu) {
            bail!("effective TURN MTU must be between 576 and 1500 bytes");
        }
        let credential_expires_at = DateTime::parse_from_rfc3339(&request.credential_expires_at)
            .context("credential_expires_at must be RFC 3339")?
            .with_timezone(&Utc);
        let now = Utc::now();
        if credential_expires_at <= now {
            bail!("TURN credential is already expired");
        }
        if credential_expires_at > now + Duration::hours(48) + Duration::minutes(5) {
            bail!("TURN credential expiry exceeds the 48-hour provider limit");
        }
        let turn_url = request
            .turn_urls
            .iter()
            .find(|url| url.starts_with("turn:") && url.contains("transport=udp"))
            .cloned()
            .context("prepare_turn requires a UDP TURN URL")?;
        let expected_peer_ips = request
            .expected_peer_ips
            .iter()
            .map(|value| {
                value
                    .parse::<IpAddr>()
                    .with_context(|| format!("invalid expected peer IP `{value}`"))
            })
            .collect::<Result<Vec<_>>>()?;
        if expected_peer_ips.is_empty() {
            bail!("prepare_turn requires at least one expected peer IP");
        }

        inner.state.turn_status = TurnRuntimeStatus::Preparing;
        inner.state.host_revision = inner.state.host_revision.saturating_add(1);
        inner.state.updated_at = Utc::now().to_rfc3339();
        persist_state(&self.state_path, &inner.state)?;

        let replacement = match TurnBridgeHandle::start(
            TurnBridgeConfig {
                turn_url,
                username: request.username,
                credential: request.credential,
                expected_peer_ips: expected_peer_ips.clone(),
                kernel_wireguard_addr: self.kernel_wireguard_addr,
                effective_mtu: request.effective_mtu,
                allocation_generation: request.requested_generation,
            },
            self.probe_sessions.clone(),
        )
        .await
        {
            Ok(bridge) => bridge,
            Err(error) => {
                inner.state.turn_status = if inner.bridge.is_some() {
                    TurnRuntimeStatus::Degraded
                } else {
                    TurnRuntimeStatus::Failed
                };
                inner.state.host_revision = inner.state.host_revision.saturating_add(1);
                inner.state.updated_at = Utc::now().to_rfc3339();
                let _ = persist_state(&self.state_path, &inner.state);
                return Err(error);
            }
        };
        let relay_endpoint = replacement.relay_endpoint();
        let response = PrepareTurnResponse {
            allocation_generation: request.requested_generation,
            relay_endpoint: NetworkEndpoint {
                host: relay_endpoint.ip().to_string(),
                port: relay_endpoint.port(),
            },
            allocation_expires_at: None,
            credential_expires_at: request.credential_expires_at.clone(),
            permission_ips: expected_peer_ips
                .into_iter()
                .map(|ip| ip.to_string())
                .collect(),
        };
        let mut ready_state = inner.state.clone();
        ready_state.host_revision = ready_state.host_revision.saturating_add(1);
        ready_state.updated_at = Utc::now().to_rfc3339();
        ready_state.turn_status = TurnRuntimeStatus::Ready;
        ready_state.allocation_generation = request.requested_generation;
        ready_state.relay_endpoint = Some(NetworkEndpoint {
            host: relay_endpoint.ip().to_string(),
            port: relay_endpoint.port(),
        });
        // The TURN client refreshes the allocation internally. Credential
        // expiry is the authoritative replacement deadline.
        ready_state.allocation_expires_at = None;
        ready_state.credential_expires_at = Some(request.credential_expires_at);
        if let Err(error) = persist_state(&self.state_path, &ready_state) {
            let _ = replacement.stop().await;
            inner.state.turn_status = if inner.bridge.is_some() {
                TurnRuntimeStatus::Degraded
            } else {
                TurnRuntimeStatus::Failed
            };
            inner.state.host_revision = inner.state.host_revision.saturating_add(1);
            inner.state.updated_at = Utc::now().to_rfc3339();
            let _ = persist_state(&self.state_path, &inner.state);
            return Err(error).context("failed committing replacement TURN allocation");
        }

        let previous = inner.bridge.replace(replacement);
        inner.state = ready_state;
        inner.last_prepare = Some((request.operation_id, response.clone()));

        if let Some(previous) = previous {
            // Keep the old allocation alive long enough for a control response
            // delivered through it to reach the desktop and for GotaTun to
            // apply/validate the replacement endpoint. The replacement is
            // already authoritative in host state; this task owns only the
            // retiring bridge.
            let retirement = tokio::spawn(async move {
                tokio::time::sleep(REPLACEMENT_OVERLAP).await;
                if let Err(error) = previous.stop().await {
                    eprintln!(
                        "No Land network agent could not stop retired TURN allocation: {error:#}"
                    );
                }
            });
            inner.retirement_tasks.push(retirement);
        }

        Ok(response)
    }

    pub async fn instance_id(&self) -> String {
        self.inner.lock().await.state.instance_id.clone()
    }

    pub async fn host_revision(&self) -> u64 {
        self.inner.lock().await.state.host_revision
    }

    pub async fn status(&self) -> Result<(HostNetworkState, Option<TurnBridgeStats>)> {
        let mut inner = self.inner.lock().await;
        inner.retirement_tasks.retain(|task| !task.is_finished());
        let stats = match inner.bridge.as_ref() {
            Some(bridge) => Some(bridge.stats().await),
            None => None,
        };
        if inner
            .bridge
            .as_ref()
            .is_some_and(TurnBridgeHandle::is_finished)
            && inner.state.turn_status == TurnRuntimeStatus::Ready
        {
            inner.state.turn_status = TurnRuntimeStatus::Degraded;
            inner.state.host_revision = inner.state.host_revision.saturating_add(1);
            inner.state.updated_at = Utc::now().to_rfc3339();
            persist_state(&self.state_path, &inner.state)?;
        }
        if stats
            .as_ref()
            .and_then(|stats| stats.active_peer_tuple)
            .is_some()
        {
            inner.state.observed_transport = Some(TransportKind::CloudflareTurn);
        }
        Ok((inner.state.clone(), stats))
    }

    pub async fn install_probe_session(&self, request: InstallProbeSessionRequest) -> Result<()> {
        let token = hex::decode(request.token.trim())
            .context("probe token must be hexadecimal")?
            .try_into()
            .map_err(|_: Vec<u8>| anyhow::anyhow!("probe token must encode exactly 32 bytes"))?;
        let expires_at = DateTime::parse_from_rfc3339(&request.expires_at)
            .context("probe expires_at must be RFC 3339")?
            .with_timezone(&Utc);
        if expires_at <= Utc::now() {
            bail!("probe session is already expired");
        }
        if request.max_packets_per_second == 0 || request.max_packets_per_second > 1000 {
            bail!("probe rate must be between 1 and 1000 packets per second");
        }
        if request.allowed_paths.is_empty() {
            bail!("probe session requires at least one allowed path");
        }

        self.probe_sessions.lock().await.install_probe_session(
            request.probe_session_id,
            token,
            expires_at,
            usize::from(request.max_packets_per_second),
            request.allowed_paths,
            std::time::Instant::now(),
        );
        Ok(())
    }

    pub async fn stop_turn(&self) -> Result<HostNetworkState> {
        let mut inner = self.inner.lock().await;
        for task in inner.retirement_tasks.drain(..) {
            task.abort();
        }
        let stop_result = match inner.bridge.take() {
            Some(bridge) => bridge.stop().await,
            None => Ok(()),
        };
        inner.state.host_revision = inner.state.host_revision.saturating_add(1);
        inner.state.updated_at = Utc::now().to_rfc3339();
        inner.state.turn_status = TurnRuntimeStatus::AwaitingCredentials;
        inner.state.relay_endpoint = None;
        inner.state.allocation_expires_at = None;
        inner.state.credential_expires_at = None;
        inner.state.observed_transport = None;
        inner.last_prepare = None;
        persist_state(&self.state_path, &inner.state)?;
        stop_result?;
        Ok(inner.state.clone())
    }
}

fn load_previous_generation(path: &Path) -> u64 {
    fs::read(path)
        .ok()
        .and_then(|body| serde_json::from_slice::<HostNetworkState>(&body).ok())
        .map(|state| state.allocation_generation)
        .unwrap_or_default()
}

fn persist_state(path: &Path, state: &HostNetworkState) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("host state path {} has no parent", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(name);
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(state)?)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_network_contracts::state::TurnRuntimeStatus;

    use crate::shared_state;

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("noland-turn-manager-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn restores_generation_but_not_credentials_or_allocation() {
        let root = test_root();
        let path = root.join("network-state.json");
        let previous = HostNetworkState {
            schema_version: NETWORK_STATE_SCHEMA_VERSION,
            host_revision: 11,
            updated_at: Utc::now().to_rfc3339(),
            instance_id: "42".into(),
            session_id: Uuid::new_v4(),
            agent_version: "old".into(),
            turn_status: TurnRuntimeStatus::Ready,
            allocation_generation: 7,
            relay_endpoint: Some(NetworkEndpoint {
                host: "192.0.2.5".into(),
                port: 3478,
            }),
            allocation_expires_at: Some(Utc::now().to_rfc3339()),
            credential_expires_at: Some(Utc::now().to_rfc3339()),
            observed_transport: Some(TransportKind::CloudflareTurn),
        };
        persist_state(&path, &previous).unwrap();

        let manager = TurnManager::new(
            "42".into(),
            "new".into(),
            path.clone(),
            "127.0.0.1:51820".parse().unwrap(),
            shared_state(4, 20),
        )
        .unwrap();
        let (state, bridge) = manager.status().await.unwrap();
        assert_eq!(state.allocation_generation, 7);
        assert_eq!(state.turn_status, TurnRuntimeStatus::AwaitingCredentials);
        assert_eq!(state.relay_endpoint, None);
        assert_eq!(state.credential_expires_at, None);
        assert!(bridge.is_none());
        let persisted: HostNetworkState = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.allocation_generation, 7);
        assert_eq!(
            persisted.turn_status,
            TurnRuntimeStatus::AwaitingCredentials
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rejects_stale_generation_before_contacting_turn() {
        let root = test_root();
        let path = root.join("network-state.json");
        let previous = HostNetworkState {
            schema_version: NETWORK_STATE_SCHEMA_VERSION,
            host_revision: 0,
            updated_at: Utc::now().to_rfc3339(),
            instance_id: "42".into(),
            session_id: Uuid::new_v4(),
            agent_version: "old".into(),
            turn_status: TurnRuntimeStatus::Disabled,
            allocation_generation: 3,
            relay_endpoint: None,
            allocation_expires_at: None,
            credential_expires_at: None,
            observed_transport: None,
        };
        persist_state(&path, &previous).unwrap();
        let manager = TurnManager::new(
            "42".into(),
            "new".into(),
            path,
            "127.0.0.1:51820".parse().unwrap(),
            shared_state(4, 20),
        )
        .unwrap();
        let error = manager
            .prepare_turn(PrepareTurnRequest {
                operation_id: Uuid::new_v4(),
                requested_generation: 3,
                turn_urls: vec!["turn:127.0.0.1:9?transport=udp".into()],
                username: "unused".into(),
                credential: "unused".into(),
                credential_expires_at: (Utc::now() + Duration::hours(1)).to_rfc3339(),
                expected_peer_ips: vec!["192.0.2.9".into()],
                effective_mtu: 1200,
            })
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("must exceed active generation 3"));
        let _ = fs::remove_dir_all(root);
    }
}
