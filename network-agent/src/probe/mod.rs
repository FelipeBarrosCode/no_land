pub mod udp;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const PACKET_LEN: usize = 48;
const AUTHENTICATED_LEN: usize = 32;
const TAG_LEN: usize = 16;
const MAGIC: &[u8; 4] = b"NLND";
const VERSION: u8 = 1;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Probe = 1,
    Ack = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbePacket {
    pub packet_type: PacketType,
    pub sequence: u64,
    pub session_id: Uuid,
}

impl ProbePacket {
    pub fn encode(&self, token: &[u8; 32]) -> [u8; PACKET_LEN] {
        let mut bytes = [0_u8; PACKET_LEN];
        bytes[0..4].copy_from_slice(MAGIC);
        bytes[4] = VERSION;
        bytes[5] = self.packet_type as u8;
        bytes[8..16].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[16..32].copy_from_slice(self.session_id.as_bytes());
        let tag = authentication_tag(&bytes[..AUTHENTICATED_LEN], token);
        bytes[32..48].copy_from_slice(&tag);
        bytes
    }

    pub fn decode_and_verify(bytes: &[u8], token: &[u8; 32]) -> Option<Self> {
        let packet = Self::decode_unverified(bytes)?;
        let expected = authentication_tag(&bytes[..AUTHENTICATED_LEN], token);
        bool::from(expected.ct_eq(&bytes[AUTHENTICATED_LEN..PACKET_LEN])).then_some(packet)
    }

    pub(crate) fn decode_unverified(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != PACKET_LEN
            || &bytes[0..4] != MAGIC
            || bytes[4] != VERSION
            || bytes[6] != 0
            || bytes[7] != 0
        {
            return None;
        }

        let packet_type = match bytes[5] {
            1 => PacketType::Probe,
            2 => PacketType::Ack,
            _ => return None,
        };
        let sequence = u64::from_be_bytes(bytes[8..16].try_into().ok()?);
        let session_id = Uuid::from_bytes(bytes[16..32].try_into().ok()?);
        Some(Self {
            packet_type,
            sequence,
            session_id,
        })
    }
}

pub fn acknowledge_probe(bytes: &[u8], token: &[u8; 32]) -> Option<[u8; PACKET_LEN]> {
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
    let digest = mac.finalize().into_bytes();
    digest[..TAG_LEN]
        .try_into()
        .expect("truncated HMAC has fixed length")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip_acknowledges_authenticated_probe() {
        let token = [0x5a; 32];
        let session_id = Uuid::from_u128(0x12345678_90ab_cdef_1234_567890abcdef);
        let probe = ProbePacket {
            packet_type: PacketType::Probe,
            sequence: 501,
            session_id,
        }
        .encode(&token);

        assert_eq!(probe.len(), PACKET_LEN);
        let decoded = ProbePacket::decode_and_verify(&probe, &token).expect("valid probe");
        assert_eq!(decoded.sequence, 501);
        assert_eq!(decoded.session_id, session_id);

        let ack = acknowledge_probe(&probe, &token).expect("valid ACK");
        assert_eq!(ack.len(), probe.len());
        let decoded_ack = ProbePacket::decode_and_verify(&ack, &token).expect("valid ACK tag");
        assert_eq!(decoded_ack.packet_type, PacketType::Ack);
        assert_eq!(decoded_ack.sequence, 501);
        assert_eq!(decoded_ack.session_id, session_id);
    }

    #[test]
    fn malformed_or_unauthenticated_packets_are_rejected() {
        let token = [0x11; 32];
        let mut probe = ProbePacket {
            packet_type: PacketType::Probe,
            sequence: 9,
            session_id: Uuid::nil(),
        }
        .encode(&token);

        probe[12] ^= 1;
        assert!(ProbePacket::decode_and_verify(&probe, &token).is_none());
        assert!(acknowledge_probe(&probe, &token).is_none());
        assert!(ProbePacket::decode_and_verify(&probe[..47], &token).is_none());

        let mut reserved = ProbePacket {
            packet_type: PacketType::Probe,
            sequence: 9,
            session_id: Uuid::nil(),
        }
        .encode(&token);
        reserved[6] = 1;
        assert!(ProbePacket::decode_and_verify(&reserved, &token).is_none());
    }
}
