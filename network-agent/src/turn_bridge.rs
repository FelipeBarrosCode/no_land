use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use tokio::{
    net::UdpSocket,
    sync::{watch, RwLock},
    task::JoinHandle,
};
use turn::client::{Client, ClientConfig};
use webrtc_util::Conn;

use crate::{probe::acknowledge_registered_probe, SharedState};

const MIN_WIREGUARD_DATAGRAM: usize = 32;
const PERMISSION_PRIME_PORT: u16 = 9;

pub struct TurnBridgeConfig {
    pub turn_url: String,
    pub username: String,
    pub credential: String,
    pub expected_peer_ips: Vec<IpAddr>,
    pub kernel_wireguard_addr: SocketAddr,
    pub effective_mtu: u16,
    pub allocation_generation: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBridgeStats {
    pub allocation_generation: u64,
    pub relay_endpoint: Option<SocketAddr>,
    pub allowed_peer_ips: Vec<String>,
    pub wireguard_packets_received: u64,
    pub wireguard_packets_sent: u64,
    pub probe_packets_received: u64,
    pub dropped_packets: u64,
    pub last_packet_at_unix_ms: Option<u64>,
    pub active_peer_tuple: Option<SocketAddr>,
}

pub struct TurnBridgeHandle {
    relay_endpoint: SocketAddr,
    stats: Arc<RwLock<TurnBridgeStats>>,
    cancel: watch::Sender<bool>,
    task: JoinHandle<Result<()>>,
}

impl TurnBridgeHandle {
    pub async fn start(config: TurnBridgeConfig, probe_sessions: SharedState) -> Result<Self> {
        if config.expected_peer_ips.is_empty() {
            bail!("TURN bridge requires at least one expected public peer IP");
        }
        if config.effective_mtu < 576 {
            bail!("TURN bridge effective MTU must be at least 576 bytes");
        }

        let server_authority = udp_turn_server_address(&config.turn_url)?;
        let server_addr = tokio::net::lookup_host(&server_authority)
            .await
            .with_context(|| format!("failed resolving TURN server {server_authority}"))?
            .find(SocketAddr::is_ipv4)
            .context("TURN server did not resolve to an IPv4 address")?;
        let server_addr = server_addr.to_string();
        let conn = Arc::new(
            UdpSocket::bind("0.0.0.0:0")
                .await
                .context("failed binding the host TURN client socket")?,
        );
        let client = Client::new(ClientConfig {
            stun_serv_addr: server_addr.clone(),
            turn_serv_addr: server_addr,
            username: config.username,
            password: config.credential,
            realm: String::new(),
            software: "noland-network-agent".to_string(),
            rto_in_ms: 0,
            conn,
            vnet: None,
        })
        .await
        .context("failed creating TURN client")?;
        client
            .listen()
            .await
            .context("failed starting TURN client")?;
        let relay = client
            .allocate()
            .await
            .context("failed allocating Cloudflare TURN relay")?;
        let relay_endpoint = relay
            .local_addr()
            .context("TURN allocation omitted its relay endpoint")?;

        // TURN permissions are IP based. The Rust TURN client creates them on
        // first send_to; an empty datagram to the discard port primes each
        // expected IP before raw peer packets can reach the allocation.
        for ip in &config.expected_peer_ips {
            relay
                .send_to(&[], SocketAddr::new(*ip, PERMISSION_PRIME_PORT))
                .await
                .with_context(|| format!("failed creating TURN permission for {ip}"))?;
        }

        let kernel_socket = UdpSocket::bind(if config.kernel_wireguard_addr.is_ipv4() {
            "127.0.0.1:0"
        } else {
            "[::1]:0"
        })
        .await
        .context("failed binding the kernel WireGuard bridge socket")?;
        kernel_socket
            .connect(config.kernel_wireguard_addr)
            .await
            .context("failed connecting the kernel WireGuard bridge socket")?;

        let stats = Arc::new(RwLock::new(TurnBridgeStats {
            allocation_generation: config.allocation_generation,
            relay_endpoint: Some(relay_endpoint),
            allowed_peer_ips: config
                .expected_peer_ips
                .iter()
                .map(ToString::to_string)
                .collect(),
            ..TurnBridgeStats::default()
        }));
        let (cancel, cancel_rx) = watch::channel(false);
        let task_stats = stats.clone();
        let allowed_ips = config.expected_peer_ips.into_iter().collect::<HashSet<_>>();
        let maximum_datagram = usize::from(config.effective_mtu)
            .saturating_add(256)
            .clamp(MIN_WIREGUARD_DATAGRAM, 4096);
        let task = tokio::spawn(async move {
            let result = run_bridge(
                relay,
                kernel_socket,
                allowed_ips,
                maximum_datagram,
                probe_sessions,
                task_stats,
                cancel_rx,
            )
            .await;
            let _ = client.close().await;
            result
        });

        Ok(Self {
            relay_endpoint,
            stats,
            cancel,
            task,
        })
    }

    pub fn relay_endpoint(&self) -> SocketAddr {
        self.relay_endpoint
    }

    pub async fn stats(&self) -> TurnBridgeStats {
        self.stats.read().await.clone()
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub async fn stop(self) -> Result<()> {
        let _ = self.cancel.send(true);
        self.task
            .await
            .map_err(|error| anyhow!("TURN bridge task failed: {error}"))?
    }
}

async fn run_bridge<C: Conn + Send + Sync + 'static>(
    relay: C,
    kernel_socket: UdpSocket,
    allowed_ips: HashSet<IpAddr>,
    maximum_datagram: usize,
    probe_sessions: SharedState,
    stats: Arc<RwLock<TurnBridgeStats>>,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    let mut relay_buffer = vec![0_u8; maximum_datagram + 1];
    let mut kernel_buffer = vec![0_u8; maximum_datagram + 1];
    let mut pending_peer_tuple = None;
    let mut active_peer_tuple = None;

    loop {
        tokio::select! {
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return Ok(());
                }
            }
            received = relay.recv_from(&mut relay_buffer) => {
                let (received, source) = received.context("TURN relay receive failed")?;
                if !allowed_ips.contains(&source.ip()) {
                    record_drop(&stats).await;
                    continue;
                }
                let packet = &relay_buffer[..received];
                if packet.starts_with(b"NLND") {
                    if let Some(response) = acknowledge_registered_probe(
                        packet,
                        noland_network_contracts::state::TransportKind::CloudflareTurn,
                        &probe_sessions,
                    ).await {
                        relay.send_to(&response, source).await.context("TURN probe acknowledgement failed")?;
                        let mut current = stats.write().await;
                        current.probe_packets_received = current.probe_packets_received.saturating_add(1);
                        current.last_packet_at_unix_ms = Some(unix_time_ms());
                    } else {
                        record_drop(&stats).await;
                    }
                    continue;
                }
                if received < MIN_WIREGUARD_DATAGRAM || received > maximum_datagram {
                    record_drop(&stats).await;
                    continue;
                }
                kernel_socket.send(packet).await.context("kernel WireGuard bridge send failed")?;
                pending_peer_tuple = Some(source);
                let mut current = stats.write().await;
                current.wireguard_packets_received = current.wireguard_packets_received.saturating_add(1);
                current.last_packet_at_unix_ms = Some(unix_time_ms());
            }
            received = kernel_socket.recv(&mut kernel_buffer) => {
                let received = received.context("kernel WireGuard bridge receive failed")?;
                if received < MIN_WIREGUARD_DATAGRAM || received > maximum_datagram {
                    record_drop(&stats).await;
                    continue;
                }
                if let Some(candidate) = pending_peer_tuple.take() {
                    active_peer_tuple = Some(candidate);
                }
                let Some(destination) = active_peer_tuple else {
                    record_drop(&stats).await;
                    continue;
                };
                relay
                    .send_to(&kernel_buffer[..received], destination)
                    .await
                    .context("TURN WireGuard return send failed")?;
                let mut current = stats.write().await;
                current.wireguard_packets_sent = current.wireguard_packets_sent.saturating_add(1);
                current.last_packet_at_unix_ms = Some(unix_time_ms());
                current.active_peer_tuple = Some(destination);
            }
        }
    }
}

async fn record_drop(stats: &Arc<RwLock<TurnBridgeStats>>) {
    let mut stats = stats.write().await;
    stats.dropped_packets = stats.dropped_packets.saturating_add(1);
}

fn udp_turn_server_address(url: &str) -> Result<String> {
    let value = url.trim();
    let value = value
        .strip_prefix("turn:")
        .ok_or_else(|| anyhow!("TURN URL must use the turn: scheme"))?;
    let (authority, query) = value.split_once('?').unwrap_or((value, ""));
    if !query.is_empty() && !query.split('&').any(|part| part == "transport=udp") {
        bail!("TURN URL does not select UDP transport");
    }
    if authority.trim().is_empty() {
        bail!("TURN URL has no server authority");
    }
    let authority = if authority.starts_with('[') {
        authority.to_string()
    } else if authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        authority.to_string()
    } else {
        format!("{authority}:3478")
    };
    Ok(authority)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_cloudflare_udp_turn_urls() {
        assert_eq!(
            udp_turn_server_address("turn:turn.cloudflare.com:3478?transport=udp").unwrap(),
            "turn.cloudflare.com:3478"
        );
        assert_eq!(
            udp_turn_server_address("turn:turn.cloudflare.com?transport=udp").unwrap(),
            "turn.cloudflare.com:3478"
        );
    }

    #[test]
    fn rejects_tls_and_tcp_turn_urls() {
        assert!(udp_turn_server_address("turns:turn.cloudflare.com:5349?transport=tcp").is_err());
        assert!(udp_turn_server_address("turn:turn.cloudflare.com:3478?transport=tcp").is_err());
    }
}
