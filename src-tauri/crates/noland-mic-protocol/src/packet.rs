use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

use crate::{flags, MAGIC, MAX_PACKET_SIZE, VERSION};

/// A serialized Noland microphone packet (V1).
///
/// Layout (network byte order):
///
/// ```text
///  0..4   magic            u32  "NLM1" = 0x4E4C4D31
///  4..5   version          u8   1
///  5..6   flags            u8
///  6..8   sequence         u16
///  8..12  timestamp        u32  48 kHz sample clock
/// 12..16  ssrc             u32  random per capture session
/// 16..24  session_id       u64
/// 24..26  payload_length   u16
/// 26..42  auth_tag         [u8;16]
/// 42..    opus_payload     bytes
/// ```
pub const HEADER_SIZE: usize = 26;
pub const AUTH_TAG_SIZE: usize = 16;
pub const HEADER_WITH_AUTH: usize = HEADER_SIZE + AUTH_TAG_SIZE;

#[derive(Debug, Error)]
pub enum PacketError {
    #[error("packet too small: {0} bytes (minimum {1})")]
    TooSmall(usize, usize),
    #[error("packet too large: {0} bytes (max {1})")]
    TooLarge(usize, usize),
    #[error("invalid magic: expected 0x{0:08X}, got 0x{1:08X}")]
    InvalidMagic(u32, u32),
    #[error("unsupported version: {0}")]
    InvalidVersion(u8),
    #[error("payload length {0} exceeds remaining data")]
    PayloadLengthMismatch(u16),
}

/// Parsed view of a NolandMicPacketV1. The `opus_payload` slice points into the
/// original buffer — no allocation.
#[derive(Debug, Clone)]
pub struct ParsedPacket<'a> {
    pub flags: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub session_id: u64,
    pub auth_tag: &'a [u8; AUTH_TAG_SIZE],
    pub opus_payload: &'a [u8],
}

impl<'a> ParsedPacket<'a> {
    /// Parse a packet from a raw byte slice. Returns a view; does not validate
    /// authentication — use `auth::verify_packet` for that.
    pub fn parse(buf: &'a [u8]) -> Result<Self, PacketError> {
        if buf.len() < HEADER_WITH_AUTH {
            return Err(PacketError::TooSmall(buf.len(), HEADER_WITH_AUTH));
        }
        if buf.len() > MAX_PACKET_SIZE {
            return Err(PacketError::TooLarge(buf.len(), MAX_PACKET_SIZE));
        }

        let mut cursor: &[u8] = buf;

        let magic = cursor.get_u32();
        if magic != MAGIC {
            return Err(PacketError::InvalidMagic(MAGIC, magic));
        }

        let version = cursor.get_u8();
        if version != VERSION {
            return Err(PacketError::InvalidVersion(version));
        }

        let flags = cursor.get_u8();
        let sequence = cursor.get_u16();
        let timestamp = cursor.get_u32();
        let ssrc = cursor.get_u32();
        let session_id = cursor.get_u64();
        let payload_length = cursor.get_u16();

        // cursor is now at byte 26 (start of auth_tag)
        let remaining = cursor.len();
        if remaining < AUTH_TAG_SIZE + payload_length as usize {
            return Err(PacketError::PayloadLengthMismatch(payload_length));
        }

        let auth_tag: &[u8; AUTH_TAG_SIZE] = cursor[..AUTH_TAG_SIZE]
            .try_into()
            .expect("slice is exactly 16 bytes");
        cursor.advance(AUTH_TAG_SIZE);

        let opus_payload = &cursor[..payload_length as usize];

        Ok(ParsedPacket {
            flags,
            sequence,
            timestamp,
            ssrc,
            session_id,
            auth_tag,
            opus_payload,
        })
    }

    /// Whether the end-of-stream flag is set.
    pub fn is_eos(&self) -> bool {
        self.flags & flags::END_OF_STREAM != 0
    }

    /// Whether the muted flag is set.
    pub fn is_muted(&self) -> bool {
        self.flags & flags::MUTED != 0
    }

    /// Whether this packet signals a capture discontinuity.
    pub fn is_discontinuity(&self) -> bool {
        self.flags & flags::DISCONTINUITY != 0
    }

    /// Whether FEC is enabled for this frame.
    pub fn has_fec(&self) -> bool {
        self.flags & flags::FEC_ENABLED != 0
    }
}

/// Builder for encoding a NolandMicPacketV1 into a buffer.
pub struct PacketBuilder {
    buf: BytesMut,
}

impl PacketBuilder {
    /// Create a new builder with a pre-allocated buffer.
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(MAX_PACKET_SIZE),
        }
    }

    /// Build a packet with all fields. The `auth_tag` is zero-filled; caller
    /// should compute and overwrite it after building.
    pub fn build(
        &mut self,
        flags: u8,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        session_id: u64,
        opus_payload: &[u8],
    ) -> &[u8] {
        self.buf.clear();
        self.buf.reserve(HEADER_WITH_AUTH + opus_payload.len());

        self.buf.put_u32(MAGIC);
        self.buf.put_u8(VERSION);
        self.buf.put_u8(flags);
        self.buf.put_u16(sequence);
        self.buf.put_u32(timestamp);
        self.buf.put_u32(ssrc);
        self.buf.put_u64(session_id);
        self.buf.put_u16(opus_payload.len() as u16);

        // Zero auth tag placeholder
        self.buf.extend_from_slice(&[0u8; AUTH_TAG_SIZE]);
        // Opus payload
        self.buf.extend_from_slice(opus_payload);

        &self.buf[..]
    }

    /// Return a mutable reference to the auth tag bytes in the built packet so
    /// the caller can overwrite them after computing the MAC.
    pub fn auth_tag_mut(&mut self) -> &mut [u8; AUTH_TAG_SIZE] {
        let start = HEADER_SIZE;
        let end = start + AUTH_TAG_SIZE;
        (&mut self.buf[start..end])
            .try_into()
            .expect("auth tag is 16 bytes")
    }

    /// Return the section of the buffer that should be authenticated (everything
    /// except the auth tag itself).
    pub fn authenticated_section(&self) -> (&[u8], &[u8]) {
        let header_without_auth = &self.buf[..HEADER_SIZE];
        let payload = &self.buf[HEADER_WITH_AUTH..];
        (header_without_auth, payload)
    }
}

impl Default for PacketBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the wire size in bytes for a packet with the given Opus payload length.
pub fn wire_size(opus_payload_len: usize) -> usize {
    HEADER_WITH_AUTH + opus_payload_len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_normal_packet() {
        let mut builder = PacketBuilder::new();
        let opus = [0xFC, 0xDE, 0x01, 0x02, 0x03];
        let built = builder.build(
            flags::OPUS_FRAME,
            42,
            48000,
            0xABCD1234,
            0xDEADBEEF_CAFE1234,
            &opus,
        );

        // Overwrite auth tag with fake bytes
        let tag = builder.auth_tag_mut();
        tag.copy_from_slice(b"aaaaaaaaaaaaaaaa");

        let parsed = ParsedPacket::parse(built).expect("roundtrip parse");
        assert_eq!(parsed.flags, flags::OPUS_FRAME);
        assert_eq!(parsed.sequence, 42);
        assert_eq!(parsed.timestamp, 48000);
        assert_eq!(parsed.ssrc, 0xABCD1234);
        assert_eq!(parsed.session_id, 0xDEADBEEF_CAFE1234);
        assert_eq!(parsed.opus_payload, opus);
        assert_eq!(parsed.auth_tag, b"aaaaaaaaaaaaaaaa");
        assert!(!parsed.is_eos());
        assert!(!parsed.is_muted());
    }

    #[test]
    fn roundtrip_flags() {
        let mut builder = PacketBuilder::new();
        let flags = flags::OPUS_FRAME | flags::FEC_ENABLED | flags::MUTED | flags::END_OF_STREAM;
        let built = builder.build(flags, 0, 0, 0, 0, &[]);
        let tag = builder.auth_tag_mut();
        tag.copy_from_slice(b"bbbbbbbbbbbbbbbb");

        let parsed = ParsedPacket::parse(built).expect("flags parse");
        assert!(parsed.is_eos());
        assert!(parsed.is_muted());
        assert!(parsed.has_fec());
    }

    #[test]
    fn reject_wrong_magic() {
        let mut buf = BytesMut::with_capacity(HEADER_WITH_AUTH);
        buf.put_u32(0xDEADBEEF);
        buf.resize(HEADER_WITH_AUTH, 0);
        let err = ParsedPacket::parse(&buf).unwrap_err();
        assert!(matches!(err, PacketError::InvalidMagic(_, _)));
    }

    #[test]
    fn reject_too_small() {
        let buf = [0u8; 10];
        let err = ParsedPacket::parse(&buf).unwrap_err();
        assert!(matches!(err, PacketError::TooSmall(10, HEADER_WITH_AUTH)));
    }
}
