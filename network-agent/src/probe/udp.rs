use std::net::SocketAddr;

use tokio::net::UdpSocket;

use noland_network_contracts::state::TransportKind;

use crate::{probe::acknowledge_registered_probe, SharedState};

pub async fn run(addr: SocketAddr, shared: SharedState) -> std::io::Result<()> {
    let socket = UdpSocket::bind(addr).await?;
    let mut buffer = [0_u8; 2_048];

    loop {
        let (received, peer) = socket.recv_from(&mut buffer).await?;
        if let Some(response) =
            acknowledge_registered_probe(&buffer[..received], TransportKind::Direct, &shared).await
        {
            debug_assert!(response.len() <= received);
            socket.send_to(response.as_slice(), peer).await?;
        }
    }
}
