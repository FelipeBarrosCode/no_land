pub mod udp;

pub use noland_network_contracts::probe::{
    acknowledge_probe, PacketType, ProbePacket, ProbePath, V1_PACKET_LEN, V2_PACKET_LEN,
};

use std::time::Instant;

use chrono::Utc;
use noland_network_contracts::state::TransportKind;

use crate::SharedState;

pub async fn acknowledge_registered_probe(
    bytes: &[u8],
    received_path: TransportKind,
    shared: &SharedState,
) -> Option<Vec<u8>> {
    let header = ProbePacket::decode_unverified(bytes)?;
    if header.packet_type != PacketType::Probe {
        return None;
    }
    let declared_path = match (header.version, header.path) {
        (1, ProbePath::Unspecified) => TransportKind::Direct,
        (2, ProbePath::Direct) => TransportKind::Direct,
        (2, ProbePath::CloudflareTurn) => TransportKind::CloudflareTurn,
        _ => return None,
    };
    if declared_path != received_path {
        return None;
    }
    let mut registry = shared.lock().await;
    let session = registry.get_mut(&header.session_id)?;
    if !session.allows_probe(received_path, Utc::now()) {
        return None;
    }
    let acknowledgement = acknowledge_probe(bytes, &session.token)?;
    session
        .udp_rate
        .allow(Instant::now())
        .then_some(acknowledgement)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use uuid::Uuid;

    use super::*;
    use crate::shared_state;

    #[tokio::test]
    async fn installed_probe_sessions_enforce_declared_and_received_paths() {
        let shared = shared_state(4, 20);
        let session_id = Uuid::from_u128(77);
        let token = [0x33; 32];
        shared.lock().await.install_probe_session(
            session_id,
            token,
            Utc::now() + Duration::minutes(1),
            20,
            vec![TransportKind::Direct],
            Instant::now(),
        );
        let direct =
            ProbePacket::v2(PacketType::Probe, ProbePath::Direct, 1, session_id, 5).encode(&token);
        assert!(
            acknowledge_registered_probe(&direct, TransportKind::Direct, &shared)
                .await
                .is_some()
        );
        assert!(
            acknowledge_registered_probe(&direct, TransportKind::CloudflareTurn, &shared)
                .await
                .is_none()
        );

        let relay = ProbePacket::v2(
            PacketType::Probe,
            ProbePath::CloudflareTurn,
            2,
            session_id,
            6,
        )
        .encode(&token);
        assert!(
            acknowledge_registered_probe(&relay, TransportKind::CloudflareTurn, &shared)
                .await
                .is_none()
        );
    }
}
