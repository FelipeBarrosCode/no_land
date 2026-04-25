/// Minimal RTP packet parser/builder for Opus (RFC 7587).
///
/// MVP: parse only, no full validation.
#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

impl RtpPacket {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }

        let first = data[0];
        let version = (first >> 6) & 0x03;
        if version != 2 {
            return None;
        }

        let padding = (first & 0x20) != 0;
        let extension = (first & 0x10) != 0;
        let csrc_count = first & 0x0F;

        let second = data[1];
        let marker = (second & 0x80) != 0;
        let payload_type = second & 0x7F;

        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        let header_len = 12 + (csrc_count as usize) * 4;
        if data.len() < header_len {
            return None;
        }

        let payload = data[header_len..].to_vec();

        Some(RtpPacket {
            version,
            padding,
            extension,
            csrc_count,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            payload,
        })
    }

    pub fn build(
        payload_type: u8,
        sequence_number: u16,
        timestamp: u32,
        ssrc: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = Vec::with_capacity(12 + payload.len());
        // Version 2, no padding, no extension, 0 CSRC
        packet.push(0x80);
        packet.push(payload_type & 0x7F);
        packet.extend_from_slice(&sequence_number.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    /// Timestamp increment for a given frame duration at 48 kHz.
    pub fn timestamp_increment(frame_ms: u32) -> u32 {
        48000 * frame_ms / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_packet() {
        let raw = vec![
            0x80, 0x6F, // V=2, PT=111
            0x00, 0x01, // seq=1
            0x00, 0x00, 0x03, 0xC0, // timestamp=960
            0x12, 0x34, 0x56, 0x78, // ssrc
            0xAB, 0xCD, // payload
        ];
        let pkt = RtpPacket::parse(&raw).unwrap();
        assert_eq!(pkt.version, 2);
        assert_eq!(pkt.payload_type, 111);
        assert_eq!(pkt.sequence_number, 1);
        assert_eq!(pkt.timestamp, 960);
        assert_eq!(pkt.ssrc, 0x12345678);
        assert_eq!(pkt.payload, vec![0xAB, 0xCD]);
    }

    #[test]
    fn test_parse_too_short() {
        assert!(RtpPacket::parse(&[0x80]).is_none());
    }

    #[test]
    fn test_parse_wrong_version() {
        let raw = vec![0x00; 20];
        assert!(RtpPacket::parse(&raw).is_none());
    }

    #[test]
    fn test_timestamp_increment_20ms() {
        assert_eq!(RtpPacket::timestamp_increment(20), 960);
    }

    #[test]
    fn test_timestamp_increment_10ms() {
        assert_eq!(RtpPacket::timestamp_increment(10), 480);
    }

    #[test]
    fn test_build_and_parse_roundtrip() {
        let payload = vec![0x01, 0x02, 0x03];
        let raw = RtpPacket::build(111, 42, 960, 0x12345678, &payload);
        let pkt = RtpPacket::parse(&raw).unwrap();
        assert_eq!(pkt.payload_type, 111);
        assert_eq!(pkt.sequence_number, 42);
        assert_eq!(pkt.timestamp, 960);
        assert_eq!(pkt.ssrc, 0x12345678);
        assert_eq!(pkt.payload, payload);
    }
}
