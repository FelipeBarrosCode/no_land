//! Lease-local, append-only small-chunk capture. Headers and the sealed footer
//! bound every read; record digests let retry verify just a referenced slice.
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use noland_state_core::{disk::DiskWriteBudget, Result, StateError};
use serde::{Deserialize, Serialize};

use crate::{object_name, FASTCDC_MAX, SMALL_FILE_THRESHOLD};

pub const CAPTURE_SEGMENT_MAX: u64 = 32 * 1024 * 1024;
const HEADER_LEN: u64 = 16;
const RECORD_LEN: u64 = 40;
const FOOTER_LEN: u64 = 56;
const VERSION: u32 = 2;
const BUFFER_BYTES: usize = 256 * 1024;
const READER_FILES: usize = 4;

/// Old checkpoints serialize `File` as a bare path string. New checkpoints may
/// instead reference one immutable record in a lease's `.segments/*.capture`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChunkSource {
    File(PathBuf),
    Slice {
        path: PathBuf,
        offset: u64,
        len: u64,
    },
}

impl From<PathBuf> for ChunkSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

impl ChunkSource {
    pub fn path(&self) -> &Path {
        match self {
            Self::File(path) | Self::Slice { path, .. } => path,
        }
    }

    /// Slice lengths come from the capture/checkpoint, so planning doesn't stat
    /// the same container once per record. Reads additionally validate its seal.
    pub fn len(&self) -> Result<u64> {
        match self {
            Self::File(path) => {
                let metadata = std::fs::symlink_metadata(path)?;
                if !metadata.is_file() {
                    return Err(StateError::UnsafePath(
                        "chunk source is not a regular file".into(),
                    ));
                }
                Ok(metadata.len())
            }
            Self::Slice { offset, len, .. } => {
                check_bounds(*offset, *len, CAPTURE_SEGMENT_MAX - FOOTER_LEN)?;
                Ok(*len)
            }
        }
    }

    pub fn is_empty(&self) -> Result<bool> {
        self.len().map(|len| len == 0)
    }

    pub fn read(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        ChunkSourceReader::default().read_into(self, None, &mut bytes)?;
        Ok(bytes)
    }

    pub fn verify(&self, hash: &str) -> Result<()> {
        let mut bytes = Vec::new();
        ChunkSourceReader::default().read_verified(self, hash, &mut bytes)
    }
}

/// A bounded descriptor cache per pack worker plus a caller-owned payload
/// buffer avoids reopening and reallocating for every tiny record.
#[derive(Default)]
pub struct ChunkSourceReader {
    segments: VecDeque<(PathBuf, SegmentReader)>,
}

impl ChunkSourceReader {
    pub fn read_verified(
        &mut self,
        source: &ChunkSource,
        hash: &str,
        bytes: &mut Vec<u8>,
    ) -> Result<()> {
        object_name(hash)?;
        self.read_into(source, Some(hash), bytes)
    }

    fn read_into(
        &mut self,
        source: &ChunkSource,
        expected: Option<&str>,
        bytes: &mut Vec<u8>,
    ) -> Result<()> {
        let record_hash = match source {
            ChunkSource::File(path) => {
                let mut file = noland_snapshot::open_regular_nofollow(path)?;
                let before = file.metadata()?;
                let len = before.len();
                if len > FASTCDC_MAX {
                    return Err(invalid("chunk source exceeds maximum chunk size"));
                }
                bytes.resize(len as usize, 0);
                file.read_exact(bytes)?;
                if file.read(&mut [0u8; 1])? != 0
                    || !noland_snapshot::same_file_identity(&before, &file.metadata()?)
                {
                    return Err(invalid("chunk source grew while reading"));
                }
                None
            }
            ChunkSource::Slice { path, offset, len } => {
                let index = self.segments.iter().position(|(cached, _)| cached == path);
                let mut reader = match index {
                    Some(index) => self.segments.remove(index).expect("cached segment"),
                    None => {
                        if self.segments.len() == READER_FILES {
                            self.segments.pop_front();
                        }
                        (path.clone(), SegmentReader::open(path)?)
                    }
                };
                let hash = reader.1.read_slice(*offset, *len, bytes)?;
                self.segments.push_back(reader);
                Some(hash)
            }
        };
        if record_hash.is_some() || expected.is_some() {
            let actual = blake3::hash(bytes);
            if record_hash.is_some_and(|hash| actual.as_bytes() != &hash)
                || expected.is_some_and(|hash| hash != format!("blake3:{}", actual.to_hex()))
            {
                return Err(invalid("captured chunk hash mismatch"));
            }
        }
        Ok(())
    }
}

fn invalid(message: &str) -> StateError {
    StateError::Integrity(message.into())
}

fn check_bounds(offset: u64, len: u64, data_end: u64) -> Result<()> {
    if len == 0
        || len > FASTCDC_MAX
        || offset < HEADER_LEN + RECORD_LEN
        || offset.checked_add(len).is_none_or(|end| end > data_end)
    {
        return Err(invalid("invalid capture slice bounds"));
    }
    Ok(())
}

struct SegmentReader {
    file: File,
    data_end: u64,
    identity: std::fs::Metadata,
    version: u32,
}

impl SegmentReader {
    fn open(path: &Path) -> Result<Self> {
        let mut file = noland_snapshot::open_regular_nofollow(path)?;
        let identity = file.metadata()?;
        let size = identity.len();
        if !(HEADER_LEN + RECORD_LEN + 1 + FOOTER_LEN..=CAPTURE_SEGMENT_MAX).contains(&size) {
            return Err(invalid("invalid capture segment size"));
        }
        let mut header = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        let limit = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if &header[..4] != b"NLCP"
            || !matches!(version, 1 | 2)
            || limit > CAPTURE_SEGMENT_MAX
            || size > limit
        {
            return Err(invalid("invalid capture segment header"));
        }
        let data_end = size - FOOTER_LEN;
        file.seek(SeekFrom::Start(data_end))?;
        let mut footer = [0u8; FOOTER_LEN as usize];
        file.read_exact(&mut footer)?;
        let declared_size = u64::from_le_bytes(footer[8..16].try_into().unwrap());
        let records = u64::from_le_bytes(footer[16..24].try_into().unwrap());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&header);
        hasher.update(&footer[..24]);
        if &footer[..4] != b"NLCE"
            || footer[4..8] != version.to_le_bytes()
            || declared_size != size
            || records == 0
            || records > (data_end - HEADER_LEN) / (RECORD_LEN + 1)
            || &footer[24..] != hasher.finalize().as_bytes()
        {
            return Err(invalid("unsealed or corrupt capture segment"));
        }
        if !noland_snapshot::same_file_identity(&identity, &file.metadata()?) {
            return Err(invalid("capture segment changed while opening"));
        }
        Ok(Self {
            file,
            data_end,
            identity,
            version,
        })
    }

    fn read_slice(&mut self, offset: u64, len: u64, bytes: &mut Vec<u8>) -> Result<[u8; 32]> {
        check_bounds(offset, len, self.data_end)?;
        if self.version == 1 && len > SMALL_FILE_THRESHOLD {
            return Err(invalid("legacy capture record exceeds maximum"));
        }
        self.file.seek(SeekFrom::Start(offset - RECORD_LEN))?;
        let mut header = [0u8; RECORD_LEN as usize];
        self.file.read_exact(&mut header)?;
        if &header[..4] != b"NLCS"
            || u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64 != len
        {
            return Err(invalid("invalid capture record header"));
        }
        bytes.resize(len as usize, 0);
        self.file.read_exact(bytes)?;
        if !noland_snapshot::same_file_identity(&self.identity, &self.file.metadata()?) {
            return Err(invalid("sealed capture segment changed while reading"));
        }
        Ok(header[8..].try_into().unwrap())
    }
}

#[derive(Debug)]
pub struct CapturePut {
    pub source: ChunkSource,
    pub bytes: u64,
    /// An operation-local duplicate (no cache_path), or a verified CAS hit.
    pub reused: bool,
    pub cache_path: Option<PathBuf>,
}

pub(super) struct CaptureSegments {
    seen: HashMap<String, ChunkSource>,
    writer: Option<SegmentWriter>,
    next: u64,
    limit: u64,
    finished: bool,
    failed: bool,
}

impl Default for CaptureSegments {
    fn default() -> Self {
        Self {
            seen: HashMap::new(),
            writer: None,
            next: 0,
            limit: CAPTURE_SEGMENT_MAX,
            finished: false,
            failed: false,
        }
    }
}

impl CaptureSegments {
    pub(super) fn put(&mut self, root: &Path, hash: &str, data: &[u8]) -> Result<CapturePut> {
        self.put_bounded(root, hash, data, SMALL_FILE_THRESHOLD)
    }

    pub(super) fn put_streaming(
        &mut self,
        root: &Path,
        hash: &str,
        data: &[u8],
    ) -> Result<CapturePut> {
        self.put_bounded(root, hash, data, FASTCDC_MAX)
    }

    fn put_bounded(
        &mut self,
        root: &Path,
        hash: &str,
        data: &[u8],
        maximum: u64,
    ) -> Result<CapturePut> {
        if self.finished || self.failed {
            return Err(invalid("capture segments already sealed or failed"));
        }
        object_name(hash)?;
        if data.is_empty() || data.len() as u64 > maximum {
            return Err(invalid("capture record exceeds chunk limit"));
        }
        if let Some(source) = self.seen.get(hash) {
            if source.len()? != data.len() as u64 {
                return Err(invalid("duplicate capture hash has different length"));
            }
            return Ok(CapturePut {
                source: source.clone(),
                bytes: data.len() as u64,
                reused: true,
                cache_path: None,
            });
        }
        let result = (|| {
            if self.writer.as_ref().is_some_and(|writer| {
                writer.bytes + RECORD_LEN + data.len() as u64 + FOOTER_LEN > self.limit
            }) {
                self.writer.take().unwrap().finish()?;
            }
            if self.writer.is_none() {
                self.writer = Some(SegmentWriter::create(root, self.next, self.limit)?);
                self.next += 1;
            }
            self.writer.as_mut().unwrap().append(hash, data)
        })();
        let source = match result {
            Ok(source) => source,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        self.seen.insert(hash.into(), source.clone());
        Ok(CapturePut {
            source,
            bytes: data.len() as u64,
            reused: false,
            cache_path: None,
        })
    }

    pub(super) fn finish(&mut self) -> Result<()> {
        self.checkpoint()?;
        self.finished = true;
        Ok(())
    }

    pub(super) fn checkpoint(&mut self) -> Result<()> {
        if self.failed {
            return Err(invalid("cannot seal failed capture"));
        }
        if let Some(writer) = self.writer.take() {
            if let Err(error) = writer.finish() {
                self.failed = true;
                return Err(error);
            }
        }
        self.seen.clear();
        Ok(())
    }

    pub(super) fn resume(root: &Path) -> Result<Self> {
        let directory = root.join(".segments");
        let mut capture = Self::default();
        if directory.exists() {
            if std::fs::symlink_metadata(&directory)?
                .file_type()
                .is_symlink()
            {
                return Err(invalid("capture segment directory is a symlink"));
            }
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                let index = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .and_then(|v| u64::from_str_radix(v, 16).ok())
                    .ok_or_else(|| invalid("invalid capture segment name"))?;
                capture.next = capture.next.max(
                    index
                        .checked_add(1)
                        .ok_or_else(|| invalid("capture segment sequence overflow"))?,
                );
            }
        }
        Ok(capture)
    }

    pub(super) fn abort(&mut self) {
        self.writer.take();
    }
}

struct SegmentWriter {
    path: PathBuf,
    file: BufWriter<File>,
    header: [u8; HEADER_LEN as usize],
    bytes: u64,
    records: u64,
    limit: u64,
    // Admission covers the segment's remaining output, including buffered bytes.
    // Writes/flush/fsync still propagate ENOSPC; no per-record statvfs is needed.
    budget: DiskWriteBudget,
    allocated_bytes: u64,
}

impl SegmentWriter {
    fn create(root: &Path, next: u64, limit: u64) -> Result<Self> {
        let directory = root.join(".segments");
        std::fs::create_dir_all(&directory)?;
        let budget = DiskWriteBudget::reserve(&directory, limit)?;
        let path = directory.join(format!("{next:016x}.capture"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = BufWriter::with_capacity(BUFFER_BYTES, options.open(&path)?);
        let mut header = [0; HEADER_LEN as usize];
        header[..4].copy_from_slice(b"NLCP");
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        header[8..].copy_from_slice(&limit.to_le_bytes());
        file.write_all(&header)?;
        Ok(Self {
            path,
            file,
            header,
            bytes: HEADER_LEN,
            records: 0,
            limit,
            budget,
            allocated_bytes: 0,
        })
    }

    fn append(&mut self, hash: &str, data: &[u8]) -> Result<ChunkSource> {
        if self.bytes + RECORD_LEN + data.len() as u64 + FOOTER_LEN > self.limit {
            return Err(invalid("capture segment exceeds hard maximum"));
        }
        let digest = blake3::Hash::from_hex(object_name(hash)?)
            .map_err(|_| invalid("invalid capture record hash"))?;
        self.file.write_all(b"NLCS")?;
        self.file.write_all(&(data.len() as u32).to_le_bytes())?;
        self.file.write_all(digest.as_bytes())?;
        let offset = self.bytes + RECORD_LEN;
        self.file.write_all(data)?;
        self.bytes = offset + data.len() as u64;
        self.records += 1;
        self.account_allocated()?;
        Ok(ChunkSource::Slice {
            path: self.path.clone(),
            offset,
            len: data.len() as u64,
        })
    }

    fn account_allocated(&mut self) -> Result<()> {
        // BufWriter can flush automatically or bypass its buffer for a large
        // record. Only bytes actually forwarded to File stop being reserved.
        let written = self.bytes - self.file.buffer().len() as u64;
        if written > self.allocated_bytes {
            self.budget
                .consume_allocated(written - self.allocated_bytes)?;
            self.allocated_bytes = written;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        let mut footer = [0; FOOTER_LEN as usize];
        footer[..4].copy_from_slice(b"NLCE");
        footer[4..8].copy_from_slice(&VERSION.to_le_bytes());
        footer[8..16].copy_from_slice(&(self.bytes + FOOTER_LEN).to_le_bytes());
        footer[16..24].copy_from_slice(&self.records.to_le_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.header);
        hasher.update(&footer[..24]);
        footer[24..].copy_from_slice(hasher.finalize().as_bytes());
        self.file.write_all(&footer)?;
        self.bytes += FOOTER_LEN;
        self.file.flush()?;
        self.account_allocated()?;
        let mut permissions = self.file.get_ref().metadata()?.permissions();
        permissions.set_readonly(true);
        self.file.get_ref().set_permissions(permissions)?;
        self.file.get_ref().sync_all()?;
        File::open(self.path.parent().expect("segment directory"))?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blake3_hex, LocalCas};
    use std::sync::Arc;

    #[test]
    fn incremental_regular_chunk_seals_survive_restart_without_reusing_unsealed_names() {
        let root = crate::tests::temp_test_dir("incremental-seal");
        let cas = LocalCas::new(&root).unwrap();
        let lease = cas.lease().unwrap();
        let data = vec![42; FASTCDC_MAX as usize];
        let hash = blake3_hex(&data);
        let first = lease
            .capture_streaming_prehashed(&hash, &data)
            .unwrap()
            .source;
        assert!(first.verify(&hash).is_err());
        lease.checkpoint().unwrap();
        first.verify(&hash).unwrap();
        let later = lease
            .capture_streaming_prehashed(&blake3_hex(b"unsealed"), b"unsealed")
            .unwrap()
            .source;
        let lease_root = lease.root().to_path_buf();
        lease.retain_for_retry();
        drop(lease);
        let resumed = cas.resume_lease(&lease_root).unwrap();
        let final_chunk = resumed
            .capture_streaming_prehashed(&hash, &data)
            .unwrap()
            .source;
        assert_ne!(final_chunk.path(), later.path());
        assert_ne!(final_chunk.path(), first.path());
        resumed.checkpoint().unwrap();
        first.verify(&hash).unwrap();
        final_chunk.verify(&hash).unwrap();
        assert!(later.read().is_err());
        drop(resumed);
        cas.release_lease(&lease_root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_v1_seals_remain_readable_after_regular_chunk_extension() {
        let root = crate::tests::temp_test_dir("legacy-seal");
        let cas = LocalCas::new(&root).unwrap();
        let lease = cas.lease().unwrap();
        let hash = blake3_hex(b"legacy");
        let chunk = lease.capture_prehashed(&hash, b"legacy").unwrap().source;
        lease.sync_all().unwrap();
        let mut bytes = std::fs::read(chunk.path()).unwrap();
        let footer = bytes.len() - FOOTER_LEN as usize;
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[footer + 4..footer + 8].copy_from_slice(&1u32.to_le_bytes());
        let mut digest = blake3::Hasher::new();
        digest.update(&bytes[..HEADER_LEN as usize]);
        digest.update(&bytes[footer..footer + 24]);
        bytes[footer + 24..].copy_from_slice(digest.finalize().as_bytes());
        std::fs::remove_file(chunk.path()).unwrap();
        std::fs::write(chunk.path(), bytes).unwrap();
        chunk.verify(&hash).unwrap();
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn segment_budget_keeps_buffered_bytes_reserved_and_consumes_actual_writes() {
        let root = crate::tests::temp_test_dir("capture-write-budget");
        let mut writer = SegmentWriter::create(&root, 0, CAPTURE_SEGMENT_MAX).unwrap();
        let tiny = b"buffered record";
        writer.append(&blake3_hex(tiny), tiny).unwrap();
        assert_eq!(writer.file.get_ref().metadata().unwrap().len(), 0);
        assert_eq!(writer.budget.remaining(), CAPTURE_SEGMENT_MAX);
        let payload = vec![7; SMALL_FILE_THRESHOLD as usize];
        writer.append(&blake3_hex(&payload), &payload).unwrap();
        let allocated = writer.file.get_ref().metadata().unwrap().len();
        assert!(allocated > 0);
        assert_eq!(writer.budget.remaining(), CAPTURE_SEGMENT_MAX - allocated);
        writer.finish().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_duplicate_chunks_publish_one_record_and_one_container() {
        let root = crate::tests::temp_test_dir("capture-concurrent");
        let cas = LocalCas::new(&root).unwrap();
        let lease = Arc::new(cas.lease().unwrap());
        let mut outputs = Vec::new();
        std::thread::scope(|scope| {
            let threads = (0..8)
                .map(|_| {
                    let lease = lease.clone();
                    scope.spawn(move || {
                        (0..32u8)
                            .map(|value| {
                                let bytes = vec![value; 1024];
                                lease
                                    .capture_prehashed(&blake3_hex(&bytes), &bytes)
                                    .unwrap()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                outputs.extend(thread.join().unwrap());
            }
        });
        assert_eq!(outputs.iter().filter(|stored| !stored.reused).count(), 32);
        assert!(outputs.iter().all(|stored| stored.cache_path.is_none()));
        let segment = outputs[0].source.path().to_path_buf();
        assert!(
            outputs[0].source.read().is_err(),
            "unsealed capture cannot be read"
        );
        lease.sync_all().unwrap();
        let expected_size = HEADER_LEN + FOOTER_LEN + 32 * (RECORD_LEN + 1024);
        assert_eq!(std::fs::metadata(&segment).unwrap().len(), expected_size);
        assert_eq!(
            std::fs::read_dir(segment.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            cas.usage().unwrap().objects,
            0,
            "tiny capture creates no CAS inodes"
        );
        for stored in outputs {
            let bytes = stored.source.read().unwrap();
            stored.source.verify(&blake3_hex(&bytes)).unwrap();
        }
        let lease_root = lease.root().to_path_buf();
        drop(lease);
        assert!(!lease_root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn segments_rotate_with_hard_bounds_and_keep_references_immutable() {
        let root = crate::tests::temp_test_dir("capture-rotate");
        std::fs::create_dir_all(&root).unwrap();
        let mut capture = CaptureSegments {
            limit: 256,
            ..CaptureSegments::default()
        };
        let sources = (0..10u8)
            .map(|value| {
                let bytes = [value; 40];
                let hash = blake3_hex(&bytes);
                (
                    hash.clone(),
                    capture.put(&root, &hash, &bytes).unwrap().source,
                )
            })
            .collect::<Vec<_>>();
        capture.finish().unwrap();
        capture.finish().unwrap();
        assert!(capture.put(&root, &blake3_hex(b"late"), b"late").is_err());
        assert_eq!(
            std::fs::read_dir(root.join(".segments")).unwrap().count(),
            5
        );
        for (hash, source) in sources {
            source.verify(&hash).unwrap();
            assert_eq!(source.len().unwrap(), 40);
            assert!(std::fs::metadata(source.path()).unwrap().len() <= 256);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn writable(path: &Path) -> File {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        OpenOptions::new().write(true).open(path).unwrap()
    }

    #[test]
    fn slice_verification_rejects_corruption_truncation_and_invalid_bounds() {
        let root = crate::tests::temp_test_dir("capture-integrity");
        let cas = LocalCas::new(&root).unwrap();
        let lease = cas.lease().unwrap();
        let first_hash = blake3_hex(b"first");
        let first = lease
            .capture_prehashed(&first_hash, b"first")
            .unwrap()
            .source;
        let second_hash = blake3_hex(b"second");
        let second = lease
            .capture_prehashed(&second_hash, b"second")
            .unwrap()
            .source;
        lease.sync_all().unwrap();
        let ChunkSource::Slice { path, offset, len } = first.clone() else {
            panic!("slice")
        };
        let original = std::fs::read(&path).unwrap();
        for (offset, len) in [
            (0, len),
            (offset, 0),
            (u64::MAX, len),
            (offset, SMALL_FILE_THRESHOLD + 1),
            (original.len() as u64 - 1, len),
            (offset + 1, len),
        ] {
            assert!(ChunkSource::Slice {
                path: path.clone(),
                offset,
                len
            }
            .verify(&first_hash)
            .is_err());
        }
        let mut file = writable(&path);
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(b"!").unwrap();
        file.sync_all().unwrap();
        assert!(first.verify(&first_hash).is_err());
        second.verify(&second_hash).unwrap(); // Only the referenced slice is hashed.
        file.set_len(original.len() as u64 - 1).unwrap();
        assert!(second.verify(&second_hash).is_err());
        std::fs::write(&path, &original).unwrap();
        file.seek(SeekFrom::Start(8)).unwrap();
        file.write_all(&u64::MAX.to_le_bytes()).unwrap();
        assert!(first.read().is_err());
        std::fs::write(&path, &original).unwrap();
        file.seek(SeekFrom::Start(original.len() as u64 - 1))
            .unwrap();
        file.write_all(&[original[original.len() - 1] ^ 1]).unwrap();
        assert!(first.read().is_err());
        drop(file);
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn aborted_capture_closes_writer_and_removes_lease_without_touching_source() {
        let root = crate::tests::temp_test_dir("capture-abort");
        let cas = LocalCas::new(root.join("cas")).unwrap();
        let source = root.join("source");
        std::fs::write(&source, b"user save").unwrap();
        let lease = cas.lease().unwrap();
        let chunk = lease
            .capture_prehashed(&blake3_hex(b"user save"), b"user save")
            .unwrap();
        let directory = lease.root().to_path_buf();
        assert!(chunk.source.path().exists());
        drop(lease);
        assert!(!directory.exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"user save");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_checkpoint_paths_and_new_slices_round_trip() {
        let old: ChunkSource = serde_json::from_str("\"/lease/chunk\"").unwrap();
        assert_eq!(old, ChunkSource::File(PathBuf::from("/lease/chunk")));
        assert_eq!(serde_json::to_string(&old).unwrap(), "\"/lease/chunk\"");
        let source = ChunkSource::Slice {
            path: PathBuf::from("/lease/.segments/0.capture"),
            offset: 56,
            len: 3,
        };
        assert_eq!(
            serde_json::from_slice::<ChunkSource>(&serde_json::to_vec(&source).unwrap()).unwrap(),
            source
        );
    }

    #[test]
    fn capture_retry_child() {
        let Some(root) = std::env::var_os("NOLAND_CAPTURE_TEST_CHILD") else {
            return;
        };
        let root = PathBuf::from(root);
        let cas = LocalCas::new(root.join("cas")).unwrap();
        let lease = cas.lease().unwrap();
        let bytes = b"immutable capture across process exit";
        let source = lease
            .capture_prehashed(&blake3_hex(bytes), bytes)
            .unwrap()
            .source;
        lease.sync_all().unwrap();
        let mut checkpoint = File::create(root.join("checkpoint.json")).unwrap();
        checkpoint
            .write_all(&serde_json::to_vec(&(lease.root(), source)).unwrap())
            .unwrap();
        checkpoint.sync_all().unwrap();
        File::open(&root).unwrap().sync_all().unwrap();
        lease.retain_for_retry();
    }

    #[test]
    fn durable_capture_is_readable_by_a_new_process_owner() {
        let root = crate::tests::temp_test_dir("capture-process-retry");
        std::fs::create_dir_all(&root).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "capture::tests::capture_retry_child"])
            .env("NOLAND_CAPTURE_TEST_CHILD", &root)
            .status()
            .unwrap();
        assert!(status.success());
        let (lease, source): (PathBuf, ChunkSource) =
            serde_json::from_slice(&std::fs::read(root.join("checkpoint.json")).unwrap()).unwrap();
        source
            .verify(&blake3_hex(b"immutable capture across process exit"))
            .unwrap();
        let cas = LocalCas::new(root.join("cas")).unwrap();
        cas.evict_to(0).unwrap();
        source
            .verify(&blake3_hex(b"immutable capture across process exit"))
            .unwrap();
        cas.release_lease(&lease).unwrap();
        assert!(!source.path().exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
