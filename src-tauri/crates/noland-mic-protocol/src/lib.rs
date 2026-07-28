pub mod auth;
pub mod packet;

/// Magic bytes identifying a Noland microphone packet: "NLM1"
pub const MAGIC: u32 = 0x4E4C4D31;

/// Current protocol version.
pub const VERSION: u8 = 1;

/// Maximum UDP datagram size for microphone packets.
pub const MAX_PACKET_SIZE: usize = 1200;

/// Default UDP port for the microphone receiver on the VM.
pub const DEFAULT_RECEIVER_PORT: u16 = 48020;

/// Audio constants.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 1;
pub const SAMPLES_PER_FRAME_10MS: u16 = 480; // 48 kHz * 0.010

/// Packet flag bits.
pub mod flags {
    pub const OPUS_FRAME: u8 = 0x01;
    pub const FEC_ENABLED: u8 = 0x02;
    pub const MUTED: u8 = 0x04;
    pub const END_OF_STREAM: u8 = 0x08;
    pub const DISCONTINUITY: u8 = 0x10;
}

/// Result of packet validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    Valid,
    InvalidMagic,
    InvalidVersion,
    InvalidSize,
    ExpiredSession,
    AuthFailed,
    Replayed,
    WrongSource,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        matches!(self, ValidationResult::Valid)
    }
}
