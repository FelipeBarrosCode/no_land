use std::{net::SocketAddr, time::Instant};

use tokio::net::UdpSocket;

use crate::{
    probe::{acknowledge_probe, PacketType, ProbePacket, PACKET_LEN},
    SharedState,
};

pub async fn run(addr: SocketAddr, shared: SharedState) -> std::io::Result<()> {
    let socket = UdpSocket::bind(addr).await?;
    let mut buffer = [0_u8; 2_048];

    loop {
        let (received, peer) = socket.recv_from(&mut buffer).await?;
        if received != PACKET_LEN {
            continue;
        }

        let bytes = &buffer[..received];
        let Some(header) = ProbePacket::decode_unverified(bytes) else {
            continue;
        };
        if header.packet_type != PacketType::Probe {
            continue;
        }

        let response = {
            let mut registry = shared.lock().await;
            let Some(session) = registry.get_mut(&header.session_id) else {
                continue;
            };
            let Some(ack) = acknowledge_probe(bytes, &session.token) else {
                continue;
            };
            session.udp_rate.allow(Instant::now()).then_some(ack)
        };

        if let Some(response) = response {
            debug_assert!(response.len() <= received);
            socket.send_to(&response, peer).await?;
        }
    }
}
