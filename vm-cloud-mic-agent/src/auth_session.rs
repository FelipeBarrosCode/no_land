use std::time::Instant;

use noland_mic_protocol::auth;
use noland_mic_protocol::packet::ParsedPacket;
use ring::hmac;

/// An authenticated microphone session.
///
/// Holds the derived key for packet authentication and enforces expiration
/// and replay protection.
pub struct AuthSession {
    pub session_id: u64,
    pub ssrc: u32,
    mic_key: hmac::Key,
    expires_at: Instant,
    /// Sliding window for replay protection (bitmask of seen sequences).
    replay_window: u128,
    /// Base sequence for the replay window.
    replay_base: u16,
    /// Whether the session has been activated.
    active: bool,
}

impl AuthSession {
    /// Create a new authenticated session. Packets from this session will only
    /// be accepted until `expires_at`.
    pub fn new(session_id: u64, ssrc: u32, session_secret: &[u8], expires_at: Instant) -> Self {
        let mic_key = auth::derive_mic_key(session_secret, session_id);
        Self {
            session_id,
            ssrc,
            mic_key,
            expires_at,
            replay_window: 0,
            replay_base: 0,
            active: true,
        }
    }

    /// Whether the session is still valid.
    pub fn is_active(&self) -> bool {
        self.active && Instant::now() < self.expires_at
    }

    /// Deactivate the session (on explicit stop or timeout).
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Verify and authenticate a parsed packet.
    ///
    /// Returns `true` if the packet is valid, authentic, and not replayed.
    pub fn verify_packet(&mut self, packet: &ParsedPacket<'_>, raw_buf: &[u8]) -> bool {
        if !self.active {
            return false;
        }

        // Check SSRC
        if packet.ssrc != self.ssrc {
            return false;
        }

        // Check expiration
        if Instant::now() >= self.expires_at {
            self.active = false;
            return false;
        }

        // Authenticate: verify the auth tag
        let header_len = noland_mic_protocol::packet::HEADER_SIZE;
        let header_without_auth = &raw_buf[..header_len];
        let payload_offset = header_len + noland_mic_protocol::packet::AUTH_TAG_SIZE;

        if !auth::verify_packet(
            &self.mic_key,
            header_without_auth,
            &raw_buf[payload_offset..],
            packet.auth_tag,
        ) {
            return false;
        }

        // Replay protection: sliding window
        if !self.check_replay(packet.sequence) {
            return false;
        }

        true
    }

    /// Check replay protection. Returns `false` if this sequence has been seen.
    fn check_replay(&mut self, seq: u16) -> bool {
        // Initialize base on first packet
        if self.replay_window == 0 && self.replay_base == 0 {
            self.replay_base = seq;
            self.replay_window = 1u128 << 0;
            return true;
        }

        let diff = seq.wrapping_sub(self.replay_base) as i16;

        if diff < 0 {
            // Packet is behind the window
            let behind = (-diff) as u16;
            if behind > 127 {
                return false; // Too old
            }
            let bit = 1u128 << behind;
            if self.replay_window & bit != 0 {
                return false; // Already seen
            }
            self.replay_window |= bit;
            return true;
        }

        // Packet is ahead
        let ahead = diff as u16;
        if ahead >= 128 {
            // Window shift needed
            let shift = ahead - 127;
            self.replay_base = self.replay_base.wrapping_add(shift);
            self.replay_window >>= shift as usize;
        }

        let bit = 1u128 << 0; // New packet is at position 0 after shift
        if self.replay_window & bit != 0 {
            return false;
        }
        self.replay_window |= bit;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_session() -> AuthSession {
        AuthSession::new(
            12345,
            0xABCD,
            b"test-secret-key-123456",
            Instant::now() + Duration::from_secs(300),
        )
    }

    #[test]
    fn test_replay_protection_accepts_first() {
        let mut session = make_session();
        assert!(session.check_replay(100));
    }

    #[test]
    fn test_replay_protection_rejects_duplicate() {
        let mut session = make_session();
        assert!(session.check_replay(100));
        assert!(!session.check_replay(100));
    }

    #[test]
    fn test_replay_protection_accepts_sequence() {
        let mut session = make_session();
        for seq in 0..10u16 {
            assert!(session.check_replay(seq), "Should accept seq {seq}");
        }
        // Duplicate should be rejected
        assert!(!session.check_replay(5));
    }

    #[test]
    fn test_replay_very_old_rejected() {
        let mut session = make_session();
        assert!(session.check_replay(1000));
        // 200 is way behind (800 behind, which is >127)
        assert!(!session.check_replay(200));
    }
}
