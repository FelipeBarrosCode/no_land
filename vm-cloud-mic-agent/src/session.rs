use std::net::SocketAddr;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub session_token: String,
    pub expected_peer_ip: String,
    pub ssrc: u32,
    pub rtp_port: u16,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub frame_ms: u32,
    pub bitrate_kbps: u32,
    pub started_at: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionStartRequest {
    pub session_id: String,
    pub session_token: String,
    pub expected_peer_ip: String,
    pub ssrc: u32,
    pub rtp_port: u16,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub frame_ms: u32,
    pub bitrate_kbps: u32,
}

pub struct SessionManager {
    current: Option<Session>,
    wg_ip: Option<String>,
}

impl SessionManager {
    pub fn new(wg_ip: Option<String>) -> Self {
        Self { current: None, wg_ip }
    }

    pub async fn start(&mut self, req: SessionStartRequest) -> Result<(), String> {
        self.current = Some(Session {
            session_id: req.session_id,
            session_token: req.session_token,
            expected_peer_ip: req.expected_peer_ip,
            ssrc: req.ssrc,
            rtp_port: req.rtp_port,
            codec: req.codec,
            sample_rate: req.sample_rate,
            channels: req.channels,
            frame_ms: req.frame_ms,
            bitrate_kbps: req.bitrate_kbps,
            started_at: chrono::Local::now().to_rfc3339(),
        });
        Ok(())
    }

    pub async fn stop(&mut self) {
        self.current = None;
    }

    pub fn is_active(&self) -> bool {
        self.current.is_some()
    }

    pub fn accepts_peer(&self, peer: &SocketAddr) -> bool {
        let Some(ref session) = self.current else {
            return false;
        };
        peer.ip().to_string() == session.expected_peer_ip
    }

    pub fn session(&self) -> Option<&Session> {
        self.current.as_ref()
    }
}
