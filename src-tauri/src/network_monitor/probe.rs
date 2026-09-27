use noland_network_contracts::probe::{PacketType, ProbePacket};
use uuid::Uuid;

pub(crate) const PACKET_LEN: usize = noland_network_contracts::probe::V1_PACKET_LEN;

pub(crate) fn encode_probe(sequence: u64, session_id: Uuid, token: &[u8; 32]) -> [u8; PACKET_LEN] {
    ProbePacket::v1(PacketType::Probe, sequence, session_id)
        .encode(token)
        .try_into()
        .expect("v1 probes have a fixed 48-byte wire format")
}

pub(crate) fn validate_ack(bytes: &[u8], session_id: Uuid, token: &[u8; 32]) -> Option<u64> {
    let packet = ProbePacket::decode_and_verify(bytes, token)?;
    (packet.version == 1
        && packet.packet_type == PacketType::Ack
        && packet.session_id == session_id)
        .then_some(packet.sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_contract_produces_the_existing_v1_layout() {
        let token = [0x5a; 32];
        let session_id = Uuid::from_u128(0x12345678_90ab_cdef_1234_567890abcdef);
        let packet = encode_probe(0x0102_0304_0506_0708, session_id, &token);

        assert_eq!(packet.len(), 48);
        assert_eq!(&packet[0..4], b"NLND");
        assert_eq!(packet[4], 1);
        assert_eq!(packet[5], 1);
    }

    #[test]
    fn only_a_matching_authenticated_ack_is_accepted() {
        let token = [0x11; 32];
        let session_id = Uuid::from_u128(42);
        let ack = ProbePacket::v1(PacketType::Ack, 501, session_id).encode(&token);
        assert_eq!(validate_ack(&ack, session_id, &token), Some(501));
        assert!(validate_ack(&ack, Uuid::from_u128(43), &token).is_none());
        assert!(validate_ack(&ack, session_id, &[0x12; 32]).is_none());
    }
}
