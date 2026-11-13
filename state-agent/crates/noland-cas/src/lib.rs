//! BLAKE3 content addressing and FastCDC 1/4/8 MiB chunking.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use noland_state_core::constants::{FASTCDC_AVG, FASTCDC_MAX, FASTCDC_MIN};
use noland_state_core::{ChunkRef, Result, StateError};
use serde::{Deserialize, Serialize};

mod capture;
pub use capture::{CapturePut, ChunkSource, ChunkSourceReader, CAPTURE_SEGMENT_MAX};

/// Files at or below this size can be grouped by callers without changing
/// their normal one-chunk manifest representation.
pub const SMALL_FILE_THRESHOLD: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmallFileBucket {
    UpTo4KiB,
    UpTo64KiB,
    UpTo256KiB,
}

pub fn is_small_file(size: u64) -> bool {
    size <= SMALL_FILE_THRESHOLD
}

pub fn small_file_bucket(size: u64) -> Option<SmallFileBucket> {
    match size {
        0..=4_096 => Some(SmallFileBucket::UpTo4KiB),
        4_097..=65_536 => Some(SmallFileBucket::UpTo64KiB),
        65_537..=SMALL_FILE_THRESHOLD => Some(SmallFileBucket::UpTo256KiB),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionReason {
    TooSmall,
    AlreadyCompressed,
    HighEntropy,
    LikelyCompressible,
}

/// Lightweight advice only. Payload bytes and manifest chunk hashes remain
/// unchanged until a caller explicitly applies compression elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompressionHint {
    pub should_compress: bool,
    pub reason: CompressionReason,
    pub sampled_bytes: usize,
    pub entropy_bits_per_byte: f32,
}

pub fn compression_hint(data: &[u8]) -> CompressionHint {
    const SAMPLE_SIZE: usize = 64 * 1024;
    const MIN_COMPRESSION_SIZE: usize = 1024;

    let sample = &data[..data.len().min(SAMPLE_SIZE)];
    if data.len() < MIN_COMPRESSION_SIZE {
        return CompressionHint {
            should_compress: false,
            reason: CompressionReason::TooSmall,
            sampled_bytes: sample.len(),
            entropy_bits_per_byte: byte_entropy(sample),
        };
    }
    if has_compressed_magic(data) {
        return CompressionHint {
            should_compress: false,
            reason: CompressionReason::AlreadyCompressed,
            sampled_bytes: sample.len(),
            entropy_bits_per_byte: byte_entropy(sample),
        };
    }

    let entropy = byte_entropy(sample);
    let should_compress = entropy < 7.5;
    CompressionHint {
        should_compress,
        reason: if should_compress {
            CompressionReason::LikelyCompressible
        } else {
            CompressionReason::HighEntropy
        },
        sampled_bytes: sample.len(),
        entropy_bits_per_byte: entropy,
    }
}

fn byte_entropy(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for byte in data {
        counts[*byte as usize] += 1;
    }
    let len = data.len() as f64;
    -counts
        .iter()
        .filter(|count| **count != 0)
        .map(|count| {
            let probability = *count as f64 / len;
            probability * probability.log2()
        })
        .sum::<f64>() as f32
}

fn has_compressed_magic(data: &[u8]) -> bool {
    data.starts_with(&[0x1f, 0x8b])
        || data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || data.starts_with(b"PK\x03\x04")
        || data.starts_with(&[0x89, b'P', b'N', b'G'])
        || data.starts_with(&[0xff, 0xd8, 0xff])
}

pub fn blake3_hex(data: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(data).to_hex())
}

pub fn blake3_file(path: &Path) -> Result<String> {
    thread_local! {
        static BUFFER: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(vec![0; 64 * 1024]);
    }
    BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        let mut hasher = blake3::Hasher::new();
        let mut file = File::open(path)?;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(format!("blake3:{}", hasher.finalize().to_hex()))
    })
}

#[derive(Debug, Clone)]
pub struct FileChunks {
    pub file_hash: String,
    pub size: u64,
    pub chunks: Vec<ChunkRef>,
    pub payloads: Vec<Vec<u8>>,
}

/// Metadata produced by streaming chunking. Payload ownership stays with the
/// callback, allowing processing memory to remain bounded by the maximum chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkStreamResult {
    pub file_hash: String,
    pub size: u64,
    pub chunks: Vec<ChunkRef>,
}

pub fn chunk_bytes(data: &[u8]) -> FileChunks {
    chunk_bytes_with(data, FASTCDC_MIN, FASTCDC_AVG, FASTCDC_MAX)
}

pub fn chunk_bytes_with(data: &[u8], min: u64, avg: u64, max: u64) -> FileChunks {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    let file_hash = format!("blake3:{}", hasher.finalize().to_hex());
    if data.is_empty() {
        return FileChunks {
            file_hash,
            size: 0,
            chunks: Vec::new(),
            payloads: Vec::new(),
        };
    }
    if data.len() as u64 <= min {
        return FileChunks {
            file_hash,
            size: data.len() as u64,
            chunks: vec![ChunkRef {
                hash: blake3_hex(data),
                size: data.len() as u64,
            }],
            payloads: vec![data.to_vec()],
        };
    }
    let cuts = fastcdc_offsets(data, min as usize, avg as usize, max as usize);
    let mut chunks = Vec::new();
    let mut payloads = Vec::new();
    let mut start = 0usize;
    for end in cuts {
        let slice = &data[start..end];
        chunks.push(ChunkRef {
            hash: blake3_hex(slice),
            size: slice.len() as u64,
        });
        payloads.push(slice.to_vec());
        start = end;
    }
    FileChunks {
        file_hash,
        size: data.len() as u64,
        chunks,
        payloads,
    }
}

/// Compatibility API that retains chunk payloads for existing callers.
///
/// Unlike the previous implementation, input is read through the bounded
/// streaming chunker. Call [`chunk_file_streaming`] when payloads should be
/// consumed immediately instead of retained in `FileChunks`.
pub fn chunk_file(path: &Path) -> Result<FileChunks> {
    let mut payloads = Vec::new();
    let result = chunk_file_streaming(path, |_, payload| {
        payloads.push(payload.to_vec());
        Ok(())
    })?;
    Ok(FileChunks {
        file_hash: result.file_hash,
        size: result.size,
        chunks: result.chunks,
        payloads,
    })
}

/// Streams a file through FastCDC and invokes `on_chunk` once per chunk.
/// At most `FASTCDC_MAX` input bytes are retained by the chunker.
pub fn chunk_file_streaming(
    path: &Path,
    on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
) -> Result<ChunkStreamResult> {
    ChunkFileReader::default().chunk_file(path, on_chunk)
}

/// One reusable workspace per hashing worker. Small files allocate only their
/// actual size and hash once: their full-file hash is also their sole chunk hash.
/// The FastCDC workspace is allocated lazily and reused for larger files.
#[derive(Default)]
pub struct ChunkFileReader {
    buffer: Vec<u8>,
}

impl ChunkFileReader {
    /// Replay captured bytes and continue a descriptor using a reusable workspace.
    pub fn chunk_reader(
        &mut self,
        reader: impl Read,
        on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
    ) -> Result<ChunkStreamResult> {
        chunk_reader_buffered(
            reader,
            FASTCDC_MIN as usize,
            FASTCDC_AVG as usize,
            FASTCDC_MAX as usize,
            &mut self.buffer,
            on_chunk,
        )
    }

    pub fn chunk_file(
        &mut self,
        path: &Path,
        on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
    ) -> Result<ChunkStreamResult> {
        let mut file = noland_snapshot::open_regular_nofollow(path)?;
        self.chunk_open_file(&mut file, on_chunk)
    }

    /// Consume an already anchored, immutable snapshot without reopening its name.
    pub fn chunk_open_file(
        &mut self,
        file: &mut File,
        mut on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
    ) -> Result<ChunkStreamResult> {
        let size = file.metadata()?.len();
        if is_small_file(size) {
            if self.buffer.len() < size as usize {
                self.buffer.resize(size as usize, 0);
            }
            let payload = &mut self.buffer[..size as usize];
            file.read_exact(payload)?;
            if file.read(&mut [0u8; 1])? != 0 {
                return Err(StateError::Integrity(
                    "small capture source grew while reading".into(),
                ));
            }
            let file_hash = blake3_hex(payload);
            let mut chunks = Vec::new();
            if size != 0 {
                let chunk = ChunkRef {
                    hash: file_hash.clone(),
                    size,
                };
                on_chunk(&chunk, payload)?;
                chunks.push(chunk);
            }
            return Ok(ChunkStreamResult {
                file_hash,
                size,
                chunks,
            });
        }
        chunk_reader_buffered(
            file,
            FASTCDC_MIN as usize,
            FASTCDC_AVG as usize,
            FASTCDC_MAX as usize,
            &mut self.buffer,
            on_chunk,
        )
    }

    /// Capture a live small regular file into this worker's bounded workspace.
    /// Nothing is published to the callback until a stable descriptor read has
    /// succeeded. The callback must retain bytes in immutable operation storage,
    /// never as offsets into this source. Growth beyond the bound selects staging.
    pub fn capture_small_file(
        &mut self,
        file: &mut File,
        on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
    ) -> Result<Option<(ChunkStreamResult, std::fs::Metadata)>> {
        self.capture_small_file_observed(file, on_chunk, |_| Ok(()))
    }

    fn capture_small_file_observed(
        &mut self,
        file: &mut File,
        mut on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
        mut after_read: impl FnMut(usize) -> Result<()>,
    ) -> Result<Option<(ChunkStreamResult, std::fs::Metadata)>> {
        use std::io::{Seek, SeekFrom};
        for attempt in 0..2 {
            let before = file.metadata()?;
            if !before.is_file() {
                return Err(StateError::UnsafePath(
                    "small capture source is not a regular file".into(),
                ));
            }
            if !is_small_file(before.len()) {
                return Ok(None);
            }
            let size = before.len() as usize;
            if self.buffer.len() < size {
                self.buffer.resize(size, 0);
            }
            file.seek(SeekFrom::Start(0))?;
            let payload = &mut self.buffer[..size];
            let read = file.read_exact(payload);
            if let Err(error) = &read {
                if error.kind() != std::io::ErrorKind::UnexpectedEof {
                    return Err(read.unwrap_err().into());
                }
            }
            let eof = read.is_ok() && file.read(&mut [0u8; 1])? == 0;
            after_read(attempt)?;
            let after = file.metadata()?;
            if !eof || !noland_snapshot::same_file_identity(&before, &after) {
                continue;
            }
            let file_hash = blake3_hex(payload);
            let mut chunks = Vec::new();
            if size != 0 {
                let chunk = ChunkRef {
                    hash: file_hash.clone(),
                    size: size as u64,
                };
                on_chunk(&chunk, payload)?;
                chunks.push(chunk);
            }
            return Ok(Some((
                ChunkStreamResult {
                    file_hash,
                    size: size as u64,
                    chunks,
                },
                before,
            )));
        }
        Err(StateError::Integrity(
            "small source changed during both capture attempts".into(),
        ))
    }
}

/// Streams any reader using the manifest FastCDC parameters.
pub fn chunk_reader(
    reader: impl Read,
    on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
) -> Result<ChunkStreamResult> {
    chunk_reader_with(
        reader,
        FASTCDC_MIN as usize,
        FASTCDC_AVG as usize,
        FASTCDC_MAX as usize,
        on_chunk,
    )
}

/// Parameterized streaming FastCDC, primarily useful for tests and specialized
/// callers. Boundaries are byte-for-byte equivalent to [`fastcdc_offsets`].
pub fn chunk_reader_with(
    reader: impl Read,
    min: usize,
    avg: usize,
    max: usize,
    on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
) -> Result<ChunkStreamResult> {
    chunk_reader_buffered(reader, min, avg, max, &mut Vec::new(), on_chunk)
}

fn chunk_reader_buffered(
    mut reader: impl Read,
    min: usize,
    avg: usize,
    max: usize,
    buffer: &mut Vec<u8>,
    mut on_chunk: impl FnMut(&ChunkRef, &[u8]) -> Result<()>,
) -> Result<ChunkStreamResult> {
    validate_chunk_sizes(min, avg, max)?;

    let mask = mask_for(avg);
    buffer.resize(max, 0);
    let mut buffered = 0usize;
    let mut eof = false;
    let mut file_hasher = blake3::Hasher::new();
    let mut total_size = 0u64;
    let mut chunks = Vec::new();

    loop {
        while buffered < max && !eof {
            let read = reader.read(&mut buffer[buffered..max])?;
            if read == 0 {
                eof = true;
            } else {
                file_hasher.update(&buffer[buffered..buffered + read]);
                buffered += read;
            }
        }
        if buffered == 0 {
            break;
        }

        let cut = streaming_cut(&buffer[..buffered], min, max, mask, eof);
        let payload = &buffer[..cut];
        let chunk = ChunkRef {
            hash: blake3_hex(payload),
            size: cut as u64,
        };
        on_chunk(&chunk, payload)?;
        total_size += cut as u64;
        chunks.push(chunk);

        buffer.copy_within(cut..buffered, 0);
        buffered -= cut;
    }

    Ok(ChunkStreamResult {
        file_hash: format!("blake3:{}", file_hasher.finalize().to_hex()),
        size: total_size,
        chunks,
    })
}

fn validate_chunk_sizes(min: usize, avg: usize, max: usize) -> Result<()> {
    if min == 0 || min > avg || avg > max {
        return Err(StateError::Invalid(format!(
            "FastCDC sizes must satisfy 0 < min <= avg <= max; got {min}/{avg}/{max}"
        )));
    }
    Ok(())
}

fn streaming_cut(data: &[u8], min: usize, max: usize, mask: u64, eof: bool) -> usize {
    if eof && data.len() <= min {
        return data.len();
    }

    let window_end = data.len().min(max);
    let mut hash = 0u64;
    let mut i = min.min(window_end);
    while i < window_end {
        hash = (hash << 1).wrapping_add(GEAR[data[i] as usize]);
        if (hash & mask) == 0 {
            return i + 1;
        }
        i += 1;
    }
    window_end
}

/// FastCDC using a gear hash. Parameters are stored in the bundle manifest.
pub fn fastcdc_offsets(data: &[u8], min: usize, avg: usize, max: usize) -> Vec<usize> {
    let mut offsets = Vec::new();
    if data.is_empty() {
        return offsets;
    }
    let mask = mask_for(avg);
    let mut pos = 0usize;
    while pos < data.len() {
        let remaining = data.len() - pos;
        if remaining <= min {
            offsets.push(data.len());
            break;
        }
        let window_end = remaining.min(max);
        let mut hash: u64 = 0;
        let mut i = min.min(window_end);
        let mut cut = window_end;
        while i < window_end {
            hash = (hash << 1).wrapping_add(GEAR[data[pos + i] as usize]);
            if i >= min && (hash & mask) == 0 {
                cut = i + 1;
                break;
            }
            i += 1;
        }
        pos += cut;
        offsets.push(pos);
    }
    if *offsets.last().unwrap_or(&0) != data.len() {
        offsets.push(data.len());
    }
    offsets
}

fn mask_for(avg: usize) -> u64 {
    let bits = (avg.max(2) as f64).log2().floor() as u32;
    (1u64 << bits.min(31)) - 1
}

pub fn write_chunk_payloads(dir: &Path, chunks: &FileChunks) -> Result<()> {
    let cas = LocalCas::new(dir)?;
    for (chunk, payload) in chunks.chunks.iter().zip(chunks.payloads.iter()) {
        cas.put_verified(&chunk.hash, payload)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasPut {
    pub hash: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub reused: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub total_bytes: u64,
    pub pinned_bytes: u64,
    pub objects: u64,
    pub pinned_objects: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvictionResult {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_evicted: u64,
    pub objects_evicted: u64,
    /// False when pinned objects alone exceed the requested byte limit.
    pub target_met: bool,
}

/// A local, immutable content-addressed cache rooted at a caller-selected path.
/// Objects retain the historical flat `<root>/<blake3 hex>` layout.
#[derive(Debug, Clone)]
pub struct LocalCas {
    root: PathBuf,
}

static CACHE_MUTATION: RwLock<()> = RwLock::new(());

/// Hardlink leases protect active inputs from GC, including concurrent backups.
/// Each lease has its own links; releasing one cannot unpin another operation.
pub struct CasLease {
    cas: LocalCas,
    root: PathBuf,
    retained: AtomicBool,
    capture: std::sync::Mutex<capture::CaptureSegments>,
}

impl CasLease {
    /// Publish an immutable prefix without closing admission for later chunks.
    pub fn checkpoint(&self) -> Result<()> {
        self.capture
            .lock()
            .map_err(|_| StateError::Storage("capture lock poisoned".into()))?
            .checkpoint()?;
        for path in [
            self.root.as_path(),
            self.root.parent().unwrap(),
            self.cas.root(),
            self.cas.root().parent().unwrap(),
        ] {
            File::open(path)?.sync_all()?;
        }
        Ok(())
    }

    pub fn capture_streaming_prehashed(&self, hash: &str, data: &[u8]) -> Result<CapturePut> {
        self.capture
            .lock()
            .map_err(|_| StateError::Storage("capture lock poisoned".into()))?
            .put_streaming(&self.root, hash, data)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Make lease names durable before a retry checkpoint references them.
    pub fn sync_all(&self) -> Result<()> {
        // Sealing appends a footer, flushes buffered payloads and fsyncs every
        // segment before its name can enter an authenticated retry checkpoint.
        self.capture
            .lock()
            .map_err(|_| StateError::Storage("capture lock poisoned".into()))?
            .finish()?;
        for directory in [
            Some(self.root.as_path()),
            self.root.parent(),
            Some(self.cas.root()),
            self.cas.root().parent(),
        ]
        .into_iter()
        .flatten()
        {
            File::open(directory)?.sync_all()?;
        }
        Ok(())
    }

    /// Used only after an authenticated capture checkpoint durably references
    /// this lease. Its owner releases it after the corresponding packs are sealed.
    pub fn retain_for_retry(&self) {
        self.retained.store(true, Ordering::Release);
    }

    pub fn cache_path(&self, hash: &str) -> Result<PathBuf> {
        self.cas.path_for_hash(hash)
    }

    /// Tiny new chunks share bounded append-only containers owned by this lease.
    /// Larger chunks retain the existing independently leased CAS behavior.
    pub fn capture_prehashed(&self, hash: &str, data: &[u8]) -> Result<CapturePut> {
        if !data.is_empty() && is_small_file(data.len() as u64) {
            return self
                .capture
                .lock()
                .map_err(|_| StateError::Storage("capture lock poisoned".into()))?
                .put(&self.root, hash, data);
        }
        let stored = self.put_prehashed(hash, data)?;
        Ok(CapturePut {
            source: stored.path.into(),
            bytes: stored.bytes,
            reused: stored.reused,
            cache_path: Some(self.cache_path(hash)?),
        })
    }

    pub fn put_prehashed(&self, hash: &str, data: &[u8]) -> Result<CasPut> {
        let _lock = CACHE_MUTATION
            .read()
            .map_err(|_| StateError::Storage("CAS lock poisoned".into()))?;
        let mut stored = self.cas.put_prehashed(hash, data)?;
        let leased = self.root.join(object_name(hash)?);
        match std::fs::hard_link(&stored.path, &leased) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        stored.path = leased;
        Ok(stored)
    }
}

impl Drop for CasLease {
    fn drop(&mut self) {
        if self.retained.load(Ordering::Acquire) {
            return;
        }
        // Close buffered writers before removing their directory, including on
        // cancellation. The owning hashing scope joins all users before Drop.
        if let Ok(capture) = self.capture.get_mut() {
            capture.abort();
        }
        let _ = std::fs::remove_dir_all(&self.root);
        let max = std::env::var("NOLAND_CAS_CACHE_BYTES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1024 * 1024 * 1024);
        let _ = self.cas.evict_to(max);
    }
}

impl LocalCas {
    /// Reopen an authenticated operation-owned lease. Uncheckpointed trailing
    /// segments are never reused or overwritten; new segments get fresh names.
    pub fn resume_lease(&self, root: &Path) -> Result<CasLease> {
        if root.parent() != Some(self.root.join(".leases").as_path())
            || root
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(StateError::UnsafePath("invalid resumed CAS lease".into()));
        }
        for path in root.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(StateError::UnsafePath("unsafe resumed CAS lease".into()));
            }
        }
        Ok(CasLease {
            cas: self.clone(),
            root: root.to_owned(),
            retained: AtomicBool::new(true),
            capture: std::sync::Mutex::new(capture::CaptureSegments::resume(root)?),
        })
    }
    pub fn lease(&self) -> Result<CasLease> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = self.root.join(".leases");
        std::fs::create_dir_all(&base)?;
        let root = base.join(format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        Ok(CasLease {
            cas: self.clone(),
            root,
            retained: AtomicBool::new(false),
            capture: std::sync::Mutex::new(capture::CaptureSegments::default()),
        })
    }

    pub fn release_lease(&self, root: &Path) -> Result<()> {
        if root.parent() != Some(self.root.join(".leases").as_path())
            || std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(StateError::UnsafePath("invalid CAS lease directory".into()));
        }
        match std::fs::remove_dir_all(root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for_hash(&self, hash: &str) -> Result<PathBuf> {
        Ok(self.root.join(object_name(hash)?))
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.path_for_hash(hash).is_ok_and(|path| path.is_file())
    }

    pub fn open(&self, hash: &str) -> Result<File> {
        let path = self.path_for_hash(hash)?;
        File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                StateError::NotFound(hash.to_owned())
            } else {
                error.into()
            }
        })
    }

    pub fn read(&self, hash: &str) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        self.open(hash)?.read_to_end(&mut data)?;
        Ok(data)
    }

    pub fn put(&self, data: &[u8]) -> Result<CasPut> {
        let hash = blake3_hex(data);
        self.put_verified(&hash, data)
    }

    /// Atomically publishes bytes only when they match `hash`. Publication uses
    /// a same-directory hard link, so an existing immutable object is never
    /// replaced, including under concurrent writers.
    pub fn put_verified(&self, hash: &str, data: &[u8]) -> Result<CasPut> {
        if blake3_hex(data) != hash {
            return Err(StateError::Integrity(format!(
                "payload does not match CAS key {hash}"
            )));
        }
        self.put_prehashed(hash, data)
    }

    /// Publishes bytes whose hash was computed immediately before this call by
    /// the trusted local chunking pipeline. Existing objects are still verified;
    /// this only avoids hashing a newly produced chunk twice.
    pub fn put_prehashed(&self, hash: &str, data: &[u8]) -> Result<CasPut> {
        let path = self.path_for_hash(hash)?;
        if path.exists() {
            self.verify_object(hash, &path)?;
            return Ok(CasPut {
                hash: hash.to_owned(),
                path,
                bytes: data.len() as u64,
                reused: true,
            });
        }

        let temp = self.temp_path(object_name(hash)?);
        let mut budget =
            noland_state_core::disk::DiskWriteBudget::reserve(&self.root, data.len() as u64)?;
        let write_result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(data)?;
            budget.consume_allocated(data.len() as u64)?;
            file.sync_all()?;
            match std::fs::hard_link(&temp, &path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.verify_object(hash, &path)
                }
                Err(error) => Err(error.into()),
            }
        })();
        let _ = std::fs::remove_file(&temp);
        write_result?;

        Ok(CasPut {
            hash: hash.to_owned(),
            path,
            bytes: data.len() as u64,
            reused: false,
        })
    }

    pub fn pin(&self, hash: &str) -> Result<u64> {
        let object = self.path_for_hash(hash)?;
        let bytes = object
            .metadata()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    StateError::NotFound(hash.to_owned())
                } else {
                    error.into()
                }
            })?
            .len();
        let pins = self.root.join(".pins");
        std::fs::create_dir_all(&pins)?;
        let marker = pins.join(object_name(hash)?);
        match OpenOptions::new().write(true).create_new(true).open(marker) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        Ok(bytes)
    }

    pub fn unpin(&self, hash: &str) -> Result<bool> {
        let marker = self.root.join(".pins").join(object_name(hash)?);
        match std::fs::remove_file(marker) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn is_pinned(&self, hash: &str) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if self
                .path_for_hash(hash)
                .ok()
                .and_then(|path| std::fs::metadata(path).ok())
                .is_some_and(|meta| meta.nlink() > 1)
            {
                return true;
            }
        }
        object_name(hash)
            .map(|name| self.root.join(".pins").join(name).is_file())
            .unwrap_or(false)
    }

    pub fn usage(&self) -> Result<CacheUsage> {
        let mut usage = CacheUsage::default();
        for object in self.objects()? {
            usage.total_bytes = usage.total_bytes.saturating_add(object.bytes);
            usage.objects += 1;
            if self.is_pinned(&object.hash) {
                usage.pinned_bytes = usage.pinned_bytes.saturating_add(object.bytes);
                usage.pinned_objects += 1;
            }
        }
        Ok(usage)
    }

    /// Removes oldest unpinned objects until total object bytes are at or below
    /// `max_bytes`. Pinned bytes are always retained.
    pub fn evict_to(&self, max_bytes: u64) -> Result<EvictionResult> {
        let _lock = CACHE_MUTATION
            .write()
            .map_err(|_| StateError::Storage("CAS lock poisoned".into()))?;
        let mut objects = self.objects()?;
        let bytes_before = objects.iter().map(|object| object.bytes).sum::<u64>();
        let mut bytes_after = bytes_before;
        objects.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.hash.cmp(&right.hash))
        });

        let mut result = EvictionResult {
            bytes_before,
            bytes_after,
            ..EvictionResult::default()
        };
        for object in objects {
            if bytes_after <= max_bytes {
                break;
            }
            if self.is_pinned(&object.hash) {
                continue;
            }
            match std::fs::remove_file(&object.path) {
                Ok(()) => {
                    bytes_after = bytes_after.saturating_sub(object.bytes);
                    result.bytes_evicted = result.bytes_evicted.saturating_add(object.bytes);
                    result.objects_evicted += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    bytes_after = bytes_after.saturating_sub(object.bytes);
                }
                Err(error) => return Err(error.into()),
            }
        }
        result.bytes_after = bytes_after;
        result.target_met = bytes_after <= max_bytes;
        Ok(result)
    }

    fn verify_object(&self, hash: &str, path: &Path) -> Result<()> {
        if blake3_file(path)? != hash {
            return Err(StateError::Integrity(format!(
                "existing CAS object does not match key {hash}"
            )));
        }
        Ok(())
    }

    fn temp_path(&self, object_name: &str) -> PathBuf {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        self.root.join(format!(
            ".{object_name}.tmp-{}-{sequence}",
            std::process::id()
        ))
    }

    fn objects(&self) -> Result<Vec<CasObject>> {
        let mut objects = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_object_name(&name) {
                continue;
            }
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            objects.push(CasObject {
                hash: format!("blake3:{name}"),
                path: entry.path(),
                bytes: metadata.len(),
                modified: metadata
                    .modified()
                    .unwrap_or(SystemTime::UNIX_EPOCH)
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            });
        }
        Ok(objects)
    }
}

#[derive(Debug)]
struct CasObject {
    hash: String,
    path: PathBuf,
    bytes: u64,
    modified: u128,
}

fn object_name(hash: &str) -> Result<&str> {
    let name = hash
        .strip_prefix("blake3:")
        .ok_or_else(|| StateError::Invalid(format!("invalid CAS hash {hash}")))?;
    if !is_object_name(name) {
        return Err(StateError::Invalid(format!("invalid CAS hash {hash}")));
    }
    Ok(name)
}

fn is_object_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const GEAR: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        // Deterministic splitmix-style constants; not a secret.
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        table[i] = z ^ (z >> 31);
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_file_is_single_chunk() {
        let data = b"save-game";
        let chunks = chunk_bytes(data);
        assert_eq!(chunks.chunks.len(), 1);
        assert_eq!(chunks.file_hash, blake3_hex(data));
    }

    #[test]
    fn respects_min_avg_max_and_is_stable() {
        let data = vec![7u8; 12 * 1024 * 1024];
        let a = chunk_bytes(&data);
        let b = chunk_bytes(&data);
        assert_eq!(a.chunks, b.chunks);
        for chunk in &a.chunks {
            assert!(chunk.size >= FASTCDC_MIN || a.chunks.len() == 1);
            assert!(chunk.size <= FASTCDC_MAX);
        }
        assert!(a.chunks.len() >= 2);
    }

    #[test]
    fn identical_payloads_reuse_hash() {
        let data = vec![3u8; 6 * 1024 * 1024];
        let a = chunk_bytes(&data);
        let b = chunk_bytes(&data);
        assert_eq!(a.chunks[0].hash, b.chunks[0].hash);
    }

    #[test]
    fn chunk_file_matches_existing_in_memory_behavior() {
        let dir = temp_test_dir("chunk-file");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.bin");
        let data = deterministic_bytes(10 * 1024 * 1024 + 17);
        std::fs::write(&path, &data).unwrap();

        let expected = chunk_bytes(&data);
        let actual = chunk_file(&path).unwrap();
        assert_eq!(actual.file_hash, expected.file_hash);
        assert_eq!(actual.size, expected.size);
        assert_eq!(actual.chunks, expected.chunks);
        assert_eq!(actual.payloads, expected.payloads);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn streaming_boundaries_and_payloads_match_slice_chunking() {
        const MIN: usize = 64;
        const AVG: usize = 256;
        const MAX: usize = 512;

        for len in [0, 1, MIN, MIN + 1, MAX, MAX + 1, 4_097, 32_000] {
            let data = deterministic_bytes(len);
            let expected = chunk_bytes_with(&data, MIN as u64, AVG as u64, MAX as u64);
            let mut payloads = Vec::new();
            let actual = chunk_reader_with(&data[..], MIN, AVG, MAX, |_, payload| {
                payloads.push(payload.to_vec());
                Ok(())
            })
            .unwrap();

            assert_eq!(actual.file_hash, expected.file_hash, "length {len}");
            assert_eq!(actual.size, expected.size, "length {len}");
            assert_eq!(actual.chunks, expected.chunks, "length {len}");
            assert_eq!(payloads, expected.payloads, "length {len}");
        }
    }

    #[test]
    fn reusable_small_file_fast_path_preserves_all_hash_and_chunk_boundaries() {
        let root = temp_test_dir("reusable-chunker");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("input");
        let mut reader = ChunkFileReader::default();
        for length in [
            0,
            1,
            4096,
            65536,
            SMALL_FILE_THRESHOLD as usize,
            SMALL_FILE_THRESHOLD as usize + 1,
            FASTCDC_MIN as usize,
            FASTCDC_MIN as usize + 1,
            FASTCDC_MAX as usize + 17,
            97,
        ] {
            let data = deterministic_bytes(length);
            std::fs::write(&path, &data).unwrap();
            let expected = chunk_bytes(&data);
            let mut payloads = Vec::new();
            let actual = reader
                .chunk_file(&path, |_, bytes| {
                    payloads.push(bytes.to_vec());
                    Ok(())
                })
                .unwrap();
            assert_eq!(actual.file_hash, expected.file_hash, "length {length}");
            assert_eq!(actual.chunks, expected.chunks, "length {length}");
            assert_eq!(payloads, expected.payloads, "length {length}");
            if length <= SMALL_FILE_THRESHOLD as usize && length != 97 {
                assert!(reader.buffer.len() <= SMALL_FILE_THRESHOLD as usize);
            }
        }
        let allocation = reader.buffer.as_ptr();
        let capacity = reader.buffer.capacity();
        reader.chunk_file(&path, |_, _| Ok(())).unwrap();
        assert_eq!(reader.buffer.as_ptr(), allocation);
        assert_eq!(reader.buffer.capacity(), capacity);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_capture_retries_before_publishing_and_keeps_retry_bytes_off_the_live_source() {
        let root = temp_test_dir("direct-retry");
        let cas = LocalCas::new(root.join("cas")).unwrap();
        let lease = cas.lease().unwrap();
        let path = root.join("live");
        std::fs::write(&path, b"before").unwrap();
        let mut file = noland_snapshot::open_regular_nofollow(&path).unwrap();
        let mut reader = ChunkFileReader::default();
        let mut captured = Vec::new();
        let (summary, _) = reader
            .capture_small_file_observed(
                &mut file,
                |chunk, bytes| {
                    captured.push(lease.capture_prehashed(&chunk.hash, bytes)?.source);
                    Ok(())
                },
                |attempt| {
                    if attempt == 0 {
                        std::fs::write(&path, b"stable retry bytes")?;
                    }
                    Ok(())
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            captured.len(),
            1,
            "the rejected attempt must not publish chunks"
        );
        assert_eq!(summary.file_hash, blake3_hex(b"stable retry bytes"));
        std::fs::write(&path, b"later live bytes").unwrap();
        reader.chunk_file(&path, |_, _| Ok(())).unwrap(); // Reuse the workspace.
        lease.sync_all().unwrap();
        lease.retain_for_retry();
        let lease_path = lease.root().to_path_buf();
        drop(lease);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(captured[0].read().unwrap(), b"stable retry bytes");
        captured[0].verify(&summary.file_hash).unwrap();
        cas.release_lease(&lease_path).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unstable_or_growing_direct_capture_never_publishes_a_partial_attempt() {
        let root = temp_test_dir("direct-unstable");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("live");
        std::fs::write(&path, b"a").unwrap();
        let mut file = noland_snapshot::open_regular_nofollow(&path).unwrap();
        let mut reader = ChunkFileReader::default();
        let mut attempts = 0;
        assert!(reader
            .capture_small_file_observed(
                &mut file,
                |_, _| panic!("unstable bytes published"),
                |attempt| {
                    attempts += 1;
                    std::fs::write(&path, vec![b'a'; attempt + 2])?;
                    Ok(())
                }
            )
            .is_err());
        assert_eq!(attempts, 2);
        assert!(reader
            .capture_small_file_observed(
                &mut file,
                |_, _| panic!("grown bytes published"),
                |_| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)?
                        .set_len(SMALL_FILE_THRESHOLD + 1)?;
                    Ok(())
                }
            )
            .unwrap()
            .is_none());
        assert!(reader.buffer.len() <= SMALL_FILE_THRESHOLD as usize);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn streaming_reader_keeps_working_set_bounded_by_max_chunk() {
        use std::cell::Cell;
        use std::rc::Rc;

        struct GeneratedReader {
            remaining: usize,
            requested: Rc<Cell<usize>>,
            offset: usize,
        }

        impl Read for GeneratedReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.requested.set(self.requested.get().max(buf.len()));
                let len = self.remaining.min(buf.len()).min(997);
                for (index, byte) in buf[..len].iter_mut().enumerate() {
                    *byte = ((self.offset + index) % 251) as u8;
                }
                self.offset += len;
                self.remaining -= len;
                Ok(len)
            }
        }

        const MAX: usize = 8 * 1024;
        const TOTAL: usize = 32 * 1024 * 1024;
        let requested = Rc::new(Cell::new(0));
        let reader = GeneratedReader {
            remaining: TOTAL,
            requested: requested.clone(),
            offset: 0,
        };
        let mut emitted = 0usize;
        let mut largest_chunk = 0usize;
        let result = chunk_reader_with(reader, 1024, 4096, MAX, |_, payload| {
            emitted += payload.len();
            largest_chunk = largest_chunk.max(payload.len());
            Ok(())
        })
        .unwrap();

        assert_eq!(result.size, TOTAL as u64);
        assert_eq!(emitted, TOTAL);
        assert!(requested.get() <= MAX);
        assert!(largest_chunk <= MAX);
    }

    #[test]
    fn local_cas_reuses_pins_and_evicts_by_bytes() {
        let dir = temp_test_dir("cas");
        let cas = LocalCas::new(&dir).unwrap();
        let first = cas.put(&[1; 100]).unwrap();
        let reused = cas.put(&[1; 100]).unwrap();
        let pinned = cas.put(&[2; 200]).unwrap();
        let third = cas.put(&[3; 300]).unwrap();

        assert!(!first.reused);
        assert!(reused.reused);
        assert_eq!(first.path, reused.path);
        assert_eq!(cas.read(&first.hash).unwrap(), vec![1; 100]);
        assert_eq!(cas.pin(&pinned.hash).unwrap(), 200);
        assert_eq!(cas.usage().unwrap().pinned_bytes, 200);

        let eviction = cas.evict_to(200).unwrap();
        assert!(eviction.target_met);
        assert_eq!(eviction.bytes_before, 600);
        assert_eq!(eviction.bytes_after, 200);
        assert_eq!(eviction.bytes_evicted, 400);
        assert!(!cas.contains(&first.hash));
        assert!(cas.contains(&pinned.hash));
        assert!(!cas.contains(&third.hash));

        assert!(cas.unpin(&pinned.hash).unwrap());
        assert!(cas.evict_to(0).unwrap().target_met);
        assert!(!cas.contains(&pinned.hash));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn local_cas_rejects_mismatched_content() {
        let dir = temp_test_dir("cas-integrity");
        let cas = LocalCas::new(&dir).unwrap();
        let hash = blake3_hex(b"expected");
        assert!(matches!(
            cas.put_verified(&hash, b"different"),
            Err(StateError::Integrity(_))
        ));
        assert!(!cas.contains(&hash));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn independent_operation_leases_survive_gc_until_the_last_reader_releases() {
        let root = temp_test_dir("lease-gc");
        let cas = LocalCas::new(&root).unwrap();
        let first = cas.lease().unwrap();
        let second = cas.lease().unwrap();
        let bytes = b"two active operations";
        let hash = blake3_hex(bytes);
        first.put_prehashed(&hash, bytes).unwrap();
        let input = second.put_prehashed(&hash, bytes).unwrap().path;
        assert!(!cas.evict_to(0).unwrap().target_met);
        drop(first);
        assert!(!cas.evict_to(0).unwrap().target_met);
        assert_eq!(std::fs::read(input).unwrap(), bytes);
        drop(second);
        assert!(cas.evict_to(0).unwrap().target_met);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn small_file_buckets_include_threshold_boundaries() {
        assert_eq!(small_file_bucket(0), Some(SmallFileBucket::UpTo4KiB));
        assert_eq!(small_file_bucket(4 * 1024), Some(SmallFileBucket::UpTo4KiB));
        assert_eq!(
            small_file_bucket(4 * 1024 + 1),
            Some(SmallFileBucket::UpTo64KiB)
        );
        assert_eq!(
            small_file_bucket(64 * 1024 + 1),
            Some(SmallFileBucket::UpTo256KiB)
        );
        assert!(is_small_file(SMALL_FILE_THRESHOLD));
        assert_eq!(small_file_bucket(SMALL_FILE_THRESHOLD + 1), None);
        assert!(!is_small_file(SMALL_FILE_THRESHOLD + 1));
    }

    #[test]
    fn compression_hint_is_metadata_only() {
        let repetitive = vec![b'a'; 8 * 1024];
        let gzip = [b"\x1f\x8b".as_slice(), repetitive.as_slice()].concat();
        assert!(compression_hint(&repetitive).should_compress);
        assert_eq!(
            compression_hint(&gzip).reason,
            CompressionReason::AlreadyCompressed
        );
    }

    fn deterministic_bytes(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| {
                let value = (index as u64)
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (value >> 33) as u8
            })
            .collect()
    }

    pub(crate) fn temp_test_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("noland-{label}-{}-{sequence}", std::process::id()))
    }
}
