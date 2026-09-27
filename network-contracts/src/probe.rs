use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const V1_PACKET_LEN: usize = 48;
pub const V2_PACKET_LEN: usize = 56;
const V1_AUTHENTICATED_LEN: usize = 32;
const V2_AUTHENTICATED_LEN: usize = 40;
const TAG_LEN: usize = 16;
const MAGIC: &[u8; 4] = b"NLND";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Probe = 1,
    Ack = 2,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbePath {
    #[default]
    Unspecified = 0,
    Direct = 1,
    CloudflareTurn = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbePacket {
    pub version: u8,
    pub packet_type: PacketType,
    pub path: ProbePath,
    pub sequence: u64,
    pub session_id: Uuid,
    pub client_monotonic_us: u64,
}

impl ProbePacket {
    pub fn v1(packet_type: PacketType, sequence: u64, session_id: Uuid) -> Self {
        Self {
            version: 1,
            packet_type,
            path: ProbePath::Unspecified,
            sequence,
            session_id,
            client_monotonic_us: 0,
        }
    }

    pub fn v2(
        packet_type: PacketType,
        path: ProbePath,
        sequence: u64,
        session_id: Uuid,
        client_monotonic_us: u64,
    ) -> Self {
        Self {
            version: 2,
            packet_type,
            path,
            sequence,
            session_id,
            client_monotonic_us,
        }
    }

    pub fn encode(&self, token: &[u8; 32]) -> Vec<u8> {
        let (len, authenticated_len) = match self.version {
            1 => (V1_PACKET_LEN, V1_AUTHENTICATED_LEN),
            2 => (V2_PACKET_LEN, V2_AUTHENTICATED_LEN),
            _ => return Vec::new(),
        };
        let mut bytes = vec![0_u8; len];
        bytes[0..4].copy_from_slice(MAGIC);
        bytes[4] = self.version;
        bytes[5] = self.packet_type as u8;
        bytes[6] = if self.version == 2 {
            self.path as u8
        } else {
            0
        };
        bytes[8..16].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[16..32].copy_from_slice(self.session_id.as_bytes());
        if self.version == 2 {
            bytes[32..40].copy_from_slice(&self.client_monotonic_us.to_be_bytes());
        }
        let tag = authentication_tag(&bytes[..authenticated_len], token);
        bytes[authenticated_len..authenticated_len + TAG_LEN].copy_from_slice(&tag);
        bytes
    }

    pub fn decode_and_verify(bytes: &[u8], token: &[u8; 32]) -> Option<Self> {
        let packet = Self::decode_unverified(bytes)?;
        let authenticated_len = if packet.version == 1 {
            V1_AUTHENTICATED_LEN
        } else {
            V2_AUTHENTICATED_LEN
        };
        let expected = authentication_tag(&bytes[..authenticated_len], token);
        bool::from(expected.ct_eq(&bytes[authenticated_len..authenticated_len + TAG_LEN]))
            .then_some(packet)
    }

    pub fn decode_unverified(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 || &bytes[0..4] != MAGIC || bytes[7] != 0 {
            return None;
        }
        let version = bytes[4];
        let expected_len = match version {
            1 => V1_PACKET_LEN,
            2 => V2_PACKET_LEN,
            _ => return None,
        };
        if bytes.len() != expected_len {
            return None;
        }
        let packet_type = match bytes[5] {
            1 => PacketType::Probe,
            2 => PacketType::Ack,
            _ => return None,
        };
        let path = match (version, bytes[6]) {
            (1, 0) | (2, 0) => ProbePath::Unspecified,
            (2, 1) => ProbePath::Direct,
            (2, 2) => ProbePath::CloudflareTurn,
            _ => return None,
        };
        Some(Self {
            version,
            packet_type,
            path,
            sequence: u64::from_be_bytes(bytes[8..16].try_into().ok()?),
            session_id: Uuid::from_bytes(bytes[16..32].try_into().ok()?),
            client_monotonic_us: if version == 2 {
                u64::from_be_bytes(bytes[32..40].try_into().ok()?)
            } else {
                0
            },
        })
    }
}

pub fn acknowledge_probe(bytes: &[u8], token: &[u8; 32]) -> Option<Vec<u8>> {
    let probe = ProbePacket::decode_and_verify(bytes, token)?;
    if probe.packet_type != PacketType::Probe {
        return None;
    }
    Some(
        ProbePacket {
            packet_type: PacketType::Ack,
            ..probe
        }
        .encode(token),
    )
}

fn authentication_tag(payload: &[u8], token: &[u8; 32]) -> [u8; TAG_LEN] {
    let mut mac = HmacSha256::new_from_slice(token).expect("HMAC accepts a 32-byte key");
    mac.update(payload);
    mac.finalize().into_bytes()[..TAG_LEN]
        .try_into()
        .expect("fixed tag length")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_remains_wire_compatible() {
        let token = [0x5a; 32];
        let packet = ProbePacket::v1(PacketType::Probe, 501, Uuid::from_u128(42)).encode(&token);
        assert_eq!(packet.len(), V1_PACKET_LEN);
        assert_eq!(
            ProbePacket::decode_and_verify(&packet, &token)
                .unwrap()
                .sequence,
            501
        );
    }

    #[test]
    fn v2_ack_preserves_path_sequence_session_and_timestamp() {
        let token = [0x11; 32];
        let session = Uuid::from_u128(7);
        let probe = ProbePacket::v2(
            PacketType::Probe,
            ProbePath::CloudflareTurn,
            9,
            session,
            123_456,
        )
        .encode(&token);
        let ack = acknowledge_probe(&probe, &token).expect("valid probe");
        assert_eq!(ack.len(), V2_PACKET_LEN);
        let decoded = ProbePacket::decode_and_verify(&ack, &token).expect("valid ack");
        assert_eq!(decoded.packet_type, PacketType::Ack);
        assert_eq!(decoded.path, ProbePath::CloudflareTurn);
        assert_eq!(decoded.sequence, 9);
        assert_eq!(decoded.session_id, session);
        assert_eq!(decoded.client_monotonic_us, 123_456);
    }

    #[test]
    fn malformed_packets_and_reserved_fields_are_rejected() {
        let token = [0x22; 32];
        let packet =
            ProbePacket::v2(PacketType::Probe, ProbePath::Direct, 1, Uuid::nil(), 1).encode(&token);
        assert!(ProbePacket::decode_and_verify(&packet[..55], &token).is_none());
        let mut reserved = packet.clone();
        reserved[7] = 1;
        assert!(ProbePacket::decode_and_verify(&reserved, &token).is_none());
        assert!(ProbePacket::decode_and_verify(&packet, &[0x23; 32]).is_none());
    }
}
