//! Safe userspace parsing for `bpf/noland_observer.h`.
//!
//! Records are decoded field-by-field instead of being cast to a Rust struct;
//! this avoids alignment and padding assumptions at the kernel/userspace boundary.

use std::fmt;

pub const ABI_VERSION: u16 = 1;
pub const COMM_LEN: usize = 16;
pub const PATH_LEN: usize = 4096;
pub const NAME_LEN: usize = 64;
pub const EVENT_V1_SIZE: usize = 8448;

/// The record came from an operation-attempt hook. For fentry programs the
/// operation has no return value yet, so this flag never implies success or denial.
pub const FLAG_ATTEMPT: u16 = 1 << 0;
pub const FLAG_PARTIAL_PATH: u16 = 1 << 1;
pub const FLAG_PARENT_AND_NAME: u16 = 1 << 2;
pub const FLAG_SAMPLED: u16 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum RawEventKind {
    ProcessFork = 1,
    ProcessExec = 2,
    ProcessExit = 3,
    FsOpen = 16,
    FsRead = 17,
    FsWrite = 18,
    FsMmap = 19,
    FsCreate = 20,
    FsTruncate = 21,
    FsRename = 22,
    FsUnlink = 23,
    FsMkdir = 24,
    FsRmdir = 25,
    FsSymlink = 26,
    FsChmod = 27,
    FsChown = 28,
}

impl TryFrom<u16> for RawEventKind {
    type Error = AbiError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match value {
            1 => Self::ProcessFork,
            2 => Self::ProcessExec,
            3 => Self::ProcessExit,
            16 => Self::FsOpen,
            17 => Self::FsRead,
            18 => Self::FsWrite,
            19 => Self::FsMmap,
            20 => Self::FsCreate,
            21 => Self::FsTruncate,
            22 => Self::FsRename,
            23 => Self::FsUnlink,
            24 => Self::FsMkdir,
            25 => Self::FsRmdir,
            26 => Self::FsSymlink,
            27 => Self::FsChmod,
            28 => Self::FsChown,
            other => return Err(AbiError::UnknownKind(other)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: RawEventKind,
    pub flags: u16,
    pub timestamp_ns: u64,
    pub cgroup_id: u64,
    pub dev: u64,
    pub ino: u64,
    pub result: i64,
    pub offset: u64,
    pub length: u64,
    pub pid: u32,
    pub tgid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub gid: u32,
    pub mnt_ns: u32,
    pub mode: u32,
    pub operation_flags: u32,
    pub sequence: u64,
    pub accumulated_count: u32,
    pub comm: Vec<u8>,
    pub path: Vec<u8>,
    pub name: Vec<u8>,
    pub dest_path: Vec<u8>,
    pub dest_name: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiError {
    Truncated { needed: usize, actual: usize },
    UnsupportedVersion(u16),
    InvalidSize(usize),
    UnknownKind(u16),
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, actual } => {
                write!(f, "truncated BPF event: need {needed} bytes, got {actual}")
            }
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported BPF event ABI version {version}")
            }
            Self::InvalidSize(size) => write!(f, "invalid BPF event record size {size}"),
            Self::UnknownKind(kind) => write!(f, "unknown BPF event kind {kind}"),
        }
    }
}

impl std::error::Error for AbiError {}

pub fn parse_record(data: &[u8]) -> Result<RawEvent, AbiError> {
    if data.len() < 8 {
        return Err(AbiError::Truncated {
            needed: 8,
            actual: data.len(),
        });
    }
    let version = u16_at(data, 0);
    if version != ABI_VERSION {
        return Err(AbiError::UnsupportedVersion(version));
    }
    let size = u16_at(data, 2) as usize;
    if size < EVENT_V1_SIZE {
        return Err(AbiError::InvalidSize(size));
    }
    if data.len() < size {
        return Err(AbiError::Truncated {
            needed: size,
            actual: data.len(),
        });
    }

    Ok(RawEvent {
        kind: RawEventKind::try_from(u16_at(data, 4))?,
        flags: u16_at(data, 6),
        timestamp_ns: u64_at(data, 8),
        cgroup_id: u64_at(data, 16),
        dev: u64_at(data, 24),
        ino: u64_at(data, 32),
        result: i64_at(data, 40),
        offset: u64_at(data, 48),
        length: u64_at(data, 56),
        pid: u32_at(data, 64),
        tgid: u32_at(data, 68),
        ppid: u32_at(data, 72),
        uid: u32_at(data, 76),
        gid: u32_at(data, 80),
        mnt_ns: u32_at(data, 84),
        mode: u32_at(data, 88),
        operation_flags: u32_at(data, 92),
        sequence: u64_at(data, 8432),
        accumulated_count: u32_at(data, 8440),
        comm: c_field(data, 96, COMM_LEN),
        path: c_field(data, 112, PATH_LEN),
        name: c_field(data, 4208, NAME_LEN),
        dest_path: c_field(data, 4272, PATH_LEN),
        dest_name: c_field(data, 8368, NAME_LEN),
    })
}

fn c_field(data: &[u8], offset: usize, len: usize) -> Vec<u8> {
    let field = &data[offset..offset + len];
    let end = field.iter().position(|byte| *byte == 0).unwrap_or(len);
    field[..end].to_vec()
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        data[offset..offset + 2]
            .try_into()
            .expect("validated field"),
    )
}
fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .expect("validated field"),
    )
}
fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .expect("validated field"),
    )
}
fn i64_at(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .expect("validated field"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: RawEventKind) -> Vec<u8> {
        let mut bytes = vec![0; EVENT_V1_SIZE];
        bytes[0..2].copy_from_slice(&ABI_VERSION.to_le_bytes());
        bytes[2..4].copy_from_slice(&(EVENT_V1_SIZE as u16).to_le_bytes());
        bytes[4..6].copy_from_slice(&(kind as u16).to_le_bytes());
        bytes[6..8].copy_from_slice(&FLAG_PARENT_AND_NAME.to_le_bytes());
        bytes[8..16].copy_from_slice(&123_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&456_u64.to_le_bytes());
        bytes[68..72].copy_from_slice(&42_u32.to_le_bytes());
        bytes[72..76].copy_from_slice(&7_u32.to_le_bytes());
        bytes[76..80].copy_from_slice(&1000_u32.to_le_bytes());
        bytes[80..84].copy_from_slice(&1001_u32.to_le_bytes());
        bytes[96..98].copy_from_slice(b"mv");
        bytes[112..116].copy_from_slice(b"/old");
        bytes[4208..4212].copy_from_slice(b"save");
        bytes[4272..4276].copy_from_slice(b"/new");
        bytes[8368..8372].copy_from_slice(b"save");
        bytes[8432..8440].copy_from_slice(&99_u64.to_le_bytes());
        bytes[8440..8444].copy_from_slice(&4_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_fixed_v1_record() {
        let event = parse_record(&record(RawEventKind::FsRename)).unwrap();
        assert_eq!(event.kind, RawEventKind::FsRename);
        assert_eq!(event.path, b"/old");
        assert_eq!(event.name, b"save");
        assert_eq!(event.dest_path, b"/new");
        assert_eq!(event.tgid, 42);
        assert_eq!(event.cgroup_id, 456);
    }

    #[test]
    fn accepts_compatible_tail_extension() {
        let mut bytes = record(RawEventKind::ProcessExec);
        bytes.extend_from_slice(&[1, 2, 3, 4]);
        let size = bytes.len() as u16;
        bytes[2..4].copy_from_slice(&size.to_le_bytes());
        assert!(parse_record(&bytes).is_ok());
    }

    #[test]
    fn rejects_short_declared_layout() {
        let mut bytes = record(RawEventKind::ProcessExec);
        bytes[2..4].copy_from_slice(&100_u16.to_le_bytes());
        assert_eq!(parse_record(&bytes), Err(AbiError::InvalidSize(100)));
    }
}
