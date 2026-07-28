use std::net::{SocketAddr, UdpSocket};

use noland_mic_protocol::auth::{self, MicKey};

use crate::errors::{AppError, AppResult};

/// Trait for microphone transport backends.
pub trait MicrophoneTransport {
    fn start(&mut self, session_id: u64, session_secret: &[u8], ssrc: u32) -> AppResult<()>;
    fn send_frame(
        &mut self,
        sequence: u16,
        timestamp: u32,
        flags: u8,
        opus_payload: &[u8],
    ) -> AppResult<()>;
    fn stop(&mut self);
}

/// Noland UDP V1 transport — sends authenticated microphone packets.
pub struct NolandUdpV1Transport {
    socket: UdpSocket,
    remote_addr: SocketAddr,
    session_id: u64,
    ssrc: u32,
    mic_key: MicKey,
}

impl NolandUdpV1Transport {
    /// Connect to a remote microphone receiver.
    pub fn connect(
        remote_addr: &str,
        session_id: u64,
        session_secret: Vec<u8>,
        ssrc: u32,
    ) -> AppResult<Self> {
        let remote: SocketAddr = remote_addr.parse().map_err(|e| {
            AppError::InvalidInput(format!("Invalid mic remote address '{remote_addr}': {e}"))
        })?;

        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| AppError::Command(format!("Failed to bind mic UDP socket: {e}")))?;

        socket.connect(remote).map_err(|e| {
            AppError::Command(format!("Failed to connect mic socket to {remote}: {e}"))
        })?;

        let mic_key = auth::derive_mic_key(&session_secret, session_id);

        Ok(Self {
            socket,
            remote_addr: remote,
            session_id,
            ssrc,
            mic_key,
        })
    }

    /// Build an authenticated packet as a `Vec<u8>` ready to send.
    fn build_authenticated_packet(
        &self,
        sequence: u16,
        timestamp: u32,
        flags: u8,
        opus_payload: &[u8],
    ) -> Vec<u8> {
        use noland_mic_protocol::packet;
        use noland_mic_protocol::{MAGIC, VERSION};

        let payload_len = opus_payload.len() as u16;
        let total = packet::HEADER_WITH_AUTH + payload_len as usize;
        let mut buf = Vec::with_capacity(total);

        // Header
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.push(VERSION);
        buf.push(flags);
        buf.extend_from_slice(&sequence.to_be_bytes());
        buf.extend_from_slice(&timestamp.to_be_bytes());
        buf.extend_from_slice(&self.ssrc.to_be_bytes());
        buf.extend_from_slice(&self.session_id.to_be_bytes());
        buf.extend_from_slice(&payload_len.to_be_bytes());

        // Auth tag placeholder
        let auth_offset = buf.len();
        buf.extend_from_slice(&[0u8; packet::AUTH_TAG_SIZE]);

        // Payload
        buf.extend_from_slice(opus_payload);

        // Compute auth tag (header without tag + payload), then write it
        let header = &buf[..auth_offset];
        let payload = &buf[auth_offset + packet::AUTH_TAG_SIZE..];
        let mut computed_tag = [0u8; packet::AUTH_TAG_SIZE];
        auth::authenticate_packet(&self.mic_key, header, payload, &mut computed_tag);
        buf[auth_offset..auth_offset + packet::AUTH_TAG_SIZE].copy_from_slice(&computed_tag);

        buf
    }
}

impl MicrophoneTransport for NolandUdpV1Transport {
    fn start(&mut self, _session_id: u64, _secret: &[u8], _ssrc: u32) -> AppResult<()> {
        Ok(())
    }

    fn send_frame(
        &mut self,
        sequence: u16,
        timestamp: u32,
        flags: u8,
        opus_payload: &[u8],
    ) -> AppResult<()> {
        let packet = self.build_authenticated_packet(sequence, timestamp, flags, opus_payload);
        self.socket
            .send(&packet)
            .map_err(|e| AppError::Command(format!("Mic UDP send failed: {e}")))?;
        Ok(())
    }

    fn stop(&mut self) {
        let silence = [0xFC, 0xFF, 0xFE];
        let packet = self.build_authenticated_packet(
            0,
            0,
            noland_mic_protocol::flags::END_OF_STREAM,
            &silence,
        );
        let _ = self.socket.send(&packet);
    }
}
