use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

pub(crate) const PACKET_LEN: usize = 48;
const AUTHENTICATED_LEN: usize = 32;
const TAG_LEN: usize = 16;
const MAGIC: &[u8; 4] = b"NLND";
const VERSION: u8 = 1;
const PROBE_TYPE: u8 = 1;
const ACK_TYPE: u8 = 2;

type HmacSha256 = Hmac<Sha256>;

pub(crate) fn encode_probe(sequence: u64, session_id: Uuid, token: &[u8; 32]) -> [u8; PACKET_LEN] {
    encode_packet(PROBE_TYPE, sequence, session_id, token)
}

pub(crate) fn validate_ack(bytes: &[u8], session_id: Uuid, token: &[u8; 32]) -> Option<u64> {
    if bytes.len() != PACKET_LEN
        || &bytes[0..4] != MAGIC
        || bytes[4] != VERSION
        || bytes[5] != ACK_TYPE
        || bytes[6] != 0
        || bytes[7] != 0
        || &bytes[16..32] != session_id.as_bytes()
    {
        return None;
    }

    let mut mac = HmacSha256::new_from_slice(token).ok()?;
    mac.update(&bytes[..AUTHENTICATED_LEN]);
    mac.verify_truncated_left(&bytes[AUTHENTICATED_LEN..])
        .ok()?;

    Some(u64::from_be_bytes(bytes[8..16].try_into().ok()?))
}

fn encode_packet(
    packet_type: u8,
    sequence: u64,
    session_id: Uuid,
    token: &[u8; 32],
) -> [u8; PACKET_LEN] {
    let mut bytes = [0_u8; PACKET_LEN];
    bytes[0..4].copy_from_slice(MAGIC);
    bytes[4] = VERSION;
    bytes[5] = packet_type;
    bytes[8..16].copy_from_slice(&sequence.to_be_bytes());
    bytes[16..32].copy_from_slice(session_id.as_bytes());

    if let Ok(mut mac) = HmacSha256::new_from_slice(token) {
        mac.update(&bytes[..AUTHENTICATED_LEN]);
        let digest = mac.finalize().into_bytes();
        bytes[AUTHENTICATED_LEN..].copy_from_slice(&digest[..TAG_LEN]);
    }

    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ack(sequence: u64, session_id: Uuid, token: &[u8; 32]) -> [u8; PACKET_LEN] {
        encode_packet(ACK_TYPE, sequence, session_id, token)
    }

    #[test]
    fn probe_has_the_exact_wire_layout() {
        let token = [0x5a; 32];
        let session_id = Uuid::from_u128(0x12345678_90ab_cdef_1234_567890abcdef);
        let packet = encode_probe(0x0102_0304_0506_0708, session_id, &token);

        assert_eq!(packet.len(), 48);
        assert_eq!(&packet[0..4], b"NLND");
        assert_eq!(packet[4], 1);
        assert_eq!(packet[5], 1);
        assert_eq!(&packet[6..8], &[0, 0]);
        assert_eq!(&packet[8..16], &0x0102_0304_0506_0708_u64.to_be_bytes());
        assert_eq!(&packet[16..32], session_id.as_bytes());

        let mut mac = HmacSha256::new_from_slice(&token).expect("fixed-length HMAC key");
        mac.update(&packet[..32]);
        mac.verify_truncated_left(&packet[32..])
            .expect("packet tag should verify");
    }

    #[test]
    fn valid_ack_is_accepted() {
        let token = [0x11; 32];
        let session_id = Uuid::from_u128(42);
        let packet = ack(501, session_id, &token);

        assert_eq!(validate_ack(&packet, session_id, &token), Some(501));
    }

    #[test]
    fn ack_validation_rejects_every_authenticated_header_mismatch() {
        let token = [0x22; 32];
        let session_id = Uuid::from_u128(7);
        let valid = ack(9, session_id, &token);

        assert!(validate_ack(&valid[..47], session_id, &token).is_none());

        for index in [0, 4, 5, 6, 7, 16] {
            let mut malformed = valid;
            malformed[index] ^= 1;
            assert!(validate_ack(&malformed, session_id, &token).is_none());
        }

        assert!(validate_ack(&valid, Uuid::from_u128(8), &token).is_none());
        assert!(validate_ack(&valid, session_id, &[0x23; 32]).is_none());
    }

    #[test]
    fn probe_packet_is_not_accepted_as_an_ack() {
        let token = [0x33; 32];
        let session_id = Uuid::from_u128(10);
        let probe = encode_probe(1, session_id, &token);

        assert!(validate_ack(&probe, session_id, &token).is_none());
    }
}
