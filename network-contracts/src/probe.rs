use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const V1_PACKET_LEN: usize = 48;
pub const V2_PACKET_LEN: usize = 56;
pub const V3_ACK_PACKET_LEN: usize = 72;
pub const V3_MIN_PACKET_LEN: usize = V3_ACK_PACKET_LEN;
pub const V3_MAX_PACKET_LEN: usize = 1500;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbeDirection {
    #[default]
    Unspecified = 0,
    ClientToHost = 1,
    HostToClient = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbePacket {
    pub version: u8,
    pub packet_type: PacketType,
    pub path: ProbePath,
    pub sequence: u64,
    pub session_id: Uuid,
    pub client_monotonic_us: u64,
    pub profile_generation: u64,
    /// Requested complete UDP payload length for a v3 padded probe.
    pub payload_size: u16,
    /// Complete UDP payload length observed by the acknowledger.
    pub observed_payload_size: u16,
    pub direction: ProbeDirection,
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
            profile_generation: 0,
            payload_size: 0,
            observed_payload_size: 0,
            direction: ProbeDirection::Unspecified,
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
            profile_generation: 0,
            payload_size: 0,
            observed_payload_size: 0,
            direction: ProbeDirection::Unspecified,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn v3(
        packet_type: PacketType,
        path: ProbePath,
        direction: ProbeDirection,
        sequence: u64,
        session_id: Uuid,
        client_monotonic_us: u64,
        profile_generation: u64,
        payload_size: u16,
    ) -> Self {
        Self {
            version: 3,
            packet_type,
            path,
            sequence,
            session_id,
            client_monotonic_us,
            profile_generation,
            payload_size,
            observed_payload_size: 0,
            direction,
        }
    }

    pub fn encode(&self, token: &[u8; 32]) -> Vec<u8> {
        let (len, authenticated_len) = match self.version {
            1 => (V1_PACKET_LEN, V1_AUTHENTICATED_LEN),
            2 => (V2_PACKET_LEN, V2_AUTHENTICATED_LEN),
            3 => {
                let len = if self.packet_type == PacketType::Ack {
                    V3_ACK_PACKET_LEN
                } else {
                    usize::from(self.payload_size)
                };
                if !(V3_MIN_PACKET_LEN..=V3_MAX_PACKET_LEN).contains(&len) {
                    return Vec::new();
                }
                (len, len - TAG_LEN)
            }
            _ => return Vec::new(),
        };
        let mut bytes = vec![0_u8; len];
        bytes[0..4].copy_from_slice(MAGIC);
        bytes[4] = self.version;
        bytes[5] = self.packet_type as u8;
        bytes[6] = if self.version >= 2 {
            self.path as u8
        } else {
            0
        };
        bytes[7] = if self.version == 3 {
            self.direction as u8
        } else {
            0
        };
        bytes[8..16].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[16..32].copy_from_slice(self.session_id.as_bytes());
        if self.version >= 2 {
            bytes[32..40].copy_from_slice(&self.client_monotonic_us.to_be_bytes());
        }
        if self.version == 3 {
            bytes[40..48].copy_from_slice(&self.profile_generation.to_be_bytes());
            bytes[48..50].copy_from_slice(&self.payload_size.to_be_bytes());
            bytes[50..52].copy_from_slice(&self.observed_payload_size.to_be_bytes());
        }
        let tag = authentication_tag(&bytes[..authenticated_len], token);
        bytes[authenticated_len..authenticated_len + TAG_LEN].copy_from_slice(&tag);
        bytes
    }

    pub fn decode_and_verify(bytes: &[u8], token: &[u8; 32]) -> Option<Self> {
        let packet = Self::decode_unverified(bytes)?;
        let authenticated_len = match packet.version {
            1 => V1_AUTHENTICATED_LEN,
            2 => V2_AUTHENTICATED_LEN,
            3 => bytes.len().checked_sub(TAG_LEN)?,
            _ => return None,
        };
        let expected = authentication_tag(&bytes[..authenticated_len], token);
        bool::from(expected.ct_eq(&bytes[authenticated_len..authenticated_len + TAG_LEN]))
            .then_some(packet)
    }

    pub fn decode_unverified(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 || &bytes[0..4] != MAGIC {
            return None;
        }
        let version = bytes[4];
        let expected_len = match version {
            1 if bytes[7] == 0 => V1_PACKET_LEN,
            2 if bytes[7] == 0 => V2_PACKET_LEN,
            3 if (V3_MIN_PACKET_LEN..=V3_MAX_PACKET_LEN).contains(&bytes.len()) => bytes.len(),
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
            (2 | 3, 1) => ProbePath::Direct,
            (2 | 3, 2) => ProbePath::CloudflareTurn,
            _ => return None,
        };
        let direction = match (version, bytes[7]) {
            (1 | 2, 0) | (3, 0) => ProbeDirection::Unspecified,
            (3, 1) => ProbeDirection::ClientToHost,
            (3, 2) => ProbeDirection::HostToClient,
            _ => return None,
        };
        let payload_size = if version == 3 {
            u16::from_be_bytes(bytes[48..50].try_into().ok()?)
        } else {
            0
        };
        let observed_payload_size = if version == 3 {
            u16::from_be_bytes(bytes[50..52].try_into().ok()?)
        } else {
            0
        };
        if version == 3 {
            if bytes[52..56] != [0; 4] {
                return None;
            }
            match packet_type {
                PacketType::Probe
                    if usize::from(payload_size) != bytes.len() || observed_payload_size != 0 =>
                {
                    return None;
                }
                PacketType::Ack
                    if bytes.len() != V3_ACK_PACKET_LEN
                        || usize::from(payload_size) < V3_MIN_PACKET_LEN
                        || usize::from(payload_size) > V3_MAX_PACKET_LEN
                        || observed_payload_size != payload_size =>
                {
                    return None;
                }
                _ => {}
            }
        }
        Some(Self {
            version,
            packet_type,
            path,
            sequence: u64::from_be_bytes(bytes[8..16].try_into().ok()?),
            session_id: Uuid::from_bytes(bytes[16..32].try_into().ok()?),
            client_monotonic_us: if version >= 2 {
                u64::from_be_bytes(bytes[32..40].try_into().ok()?)
            } else {
                0
            },
            profile_generation: if version == 3 {
                u64::from_be_bytes(bytes[40..48].try_into().ok()?)
            } else {
                0
            },
            payload_size,
            observed_payload_size,
            direction,
        })
    }
}

pub fn acknowledge_probe(bytes: &[u8], token: &[u8; 32]) -> Option<Vec<u8>> {
    let probe = ProbePacket::decode_and_verify(bytes, token)?;
    if probe.packet_type != PacketType::Probe {
        return None;
    }
    let observed_payload_size = if probe.version == 3 {
        u16::try_from(bytes.len()).ok()?
    } else {
        0
    };
    Some(
        ProbePacket {
            packet_type: PacketType::Ack,
            observed_payload_size,
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

    #[test]
    fn v3_padded_probe_returns_small_authenticated_observed_length_ack() {
        let token = [0x35; 32];
        let probe = ProbePacket::v3(
            PacketType::Probe,
            ProbePath::Direct,
            ProbeDirection::ClientToHost,
            4,
            Uuid::from_u128(99),
            123,
            8,
            1200,
        )
        .encode(&token);
        assert_eq!(probe.len(), 1200);
        let ack = acknowledge_probe(&probe, &token).unwrap();
        assert_eq!(ack.len(), V3_ACK_PACKET_LEN);
        let decoded = ProbePacket::decode_and_verify(&ack, &token).unwrap();
        assert_eq!(decoded.profile_generation, 8);
        assert_eq!(decoded.payload_size, 1200);
        assert_eq!(decoded.observed_payload_size, 1200);
        assert_eq!(decoded.direction, ProbeDirection::ClientToHost);

        let mut tampered = probe;
        tampered[900] ^= 1;
        assert!(ProbePacket::decode_and_verify(&tampered, &token).is_none());
    }
}
