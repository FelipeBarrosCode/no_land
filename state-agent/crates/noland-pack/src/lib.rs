//! Immutable encrypted packfiles. Target 512 MiB, hard max 1 GiB.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use noland_cas::{blake3_hex, compression_hint, ChunkSource, ChunkSourceReader, FileChunks};
use noland_crypto::{encrypt, pack_key, Decryptor, MasterKey, NONCE_LEN};
use noland_state_core::constants::{PACK_MAX, PACK_TARGET};
use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAGIC: &[u8; 4] = b"NLPK";
const VERSION: u32 = 2;
const PACK_HEADER_LEN: u64 = 4 + 4 + 16;
const RECORD_HEADER_LEN: u64 = 4 + 4 + 4 + NONCE_LEN as u64;
// XChaCha20-Poly1305 appends a 16-byte authentication tag to each v1 record.
const AEAD_TAG_LEN: u64 = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackCompression {
    #[default]
    None,
    Zstd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackIndexEntry {
    pub chunk_hash: String,
    pub pack_id: String,
    pub offset: u64,
    pub ciphertext_len: u32,
    pub plaintext_len: u32,
    #[serde(default)]
    pub compression: PackCompression,
    pub nonce: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltPack {
    pub pack_id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub entries: Vec<PackIndexEntry>,
    pub checksum: String,
}

#[derive(Default)]
pub struct PackBuilder {
    target: u64,
    max: u64,
    current: Vec<Pending>,
    current_size: u64,
    finished: Vec<BuiltPack>,
}

struct Pending {
    hash: String,
    plaintext: Vec<u8>,
}

struct PackWriter {
    pack_id: String,
    path: PathBuf,
    file: Option<BufWriter<File>>,
    key: [u8; 32],
    bytes: u64,
    entries: Vec<PackIndexEntry>,
    committed: bool,
    hasher: blake3::Hasher,
    compressor: ChunkCompressor,
    write_budget: Option<noland_state_core::disk::DiskWriteBudget>,
    allocated_bytes: u64,
}

impl PackWriter {
    fn create(dest_dir: &Path, master: &MasterKey) -> Result<Self> {
        std::fs::create_dir_all(dest_dir)?;
        let pack_id = Uuid::new_v4().to_string();
        let key = pack_key(master, &pack_id);
        let path = dest_dir.join(format!("{pack_id}.pack"));
        let write_budget = noland_state_core::disk::DiskWriteBudget::reserve(&path, 1024 * 1024)?;
        let mut file = BufWriter::with_capacity(256 * 1024, File::create(&path)?);
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        let uuid = Uuid::parse_str(&pack_id).unwrap();
        file.write_all(uuid.as_bytes())?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(MAGIC);
        hasher.update(&VERSION.to_le_bytes());
        hasher.update(uuid.as_bytes());

        Ok(Self {
            pack_id,
            path,
            file: Some(file),
            key,
            bytes: PACK_HEADER_LEN,
            entries: Vec::new(),
            committed: false,
            hasher,
            compressor: ChunkCompressor::default(),
            write_budget: Some(write_budget),
            allocated_bytes: 0,
        })
    }

    fn write_chunk(&mut self, hash: String, plaintext: &[u8]) -> Result<()> {
        let plaintext_len = u32::try_from(plaintext.len())
            .map_err(|_| StateError::msg("chunk plaintext length exceeds pack format"))?;
        // Compression cannot increase a stored record. Each admission window is
        // reserved and byte-counted, and buffered flush/fsync propagate ENOSPC.
        let bound = RECORD_HEADER_LEN + AEAD_TAG_LEN + plaintext.len() as u64;
        let buffered = self
            .file
            .as_ref()
            .expect("unfinished pack writer")
            .buffer()
            .len() as u64;
        let available = self
            .write_budget
            .as_ref()
            .expect("writer budget")
            .remaining()
            .checked_sub(buffered)
            .ok_or_else(|| StateError::Storage("buffered pack bytes exceed disk budget".into()))?;
        if available < bound {
            self.file
                .as_mut()
                .expect("unfinished pack writer")
                .flush()?;
            self.account_allocated()?;
            self.write_budget.take();
            let bytes = bound.max(1024 * 1024);
            self.write_budget = Some(noland_state_core::disk::DiskWriteBudget::reserve(
                &self.path, bytes,
            )?);
        }
        let (compression, stored) = self.compressor.compress(plaintext)?;
        let blob = encrypt(&self.key, hash.as_bytes(), stored)?;
        let ciphertext_len = u32::try_from(blob.ciphertext.len())
            .map_err(|_| StateError::msg("chunk ciphertext length exceeds pack format"))?;
        let offset = self.bytes;
        let file = self.file.as_mut().expect("unfinished pack writer");
        file.write_all(b"NLCH")?;
        file.write_all(&plaintext_len.to_le_bytes())?;
        file.write_all(&ciphertext_len.to_le_bytes())?;
        file.write_all(&blob.nonce)?;
        file.write_all(&blob.ciphertext)?;
        self.hasher.update(b"NLCH");
        self.hasher.update(&plaintext_len.to_le_bytes());
        self.hasher.update(&ciphertext_len.to_le_bytes());
        self.hasher.update(&blob.nonce);
        self.hasher.update(&blob.ciphertext);

        self.bytes += RECORD_HEADER_LEN + u64::from(ciphertext_len);
        self.account_allocated()?;
        self.entries.push(PackIndexEntry {
            chunk_hash: hash,
            pack_id: self.pack_id.clone(),
            offset,
            ciphertext_len,
            plaintext_len,
            compression,
            nonce: blob.nonce.to_vec(),
        });
        Ok(())
    }

    fn account_allocated(&mut self) -> Result<()> {
        let written = self.bytes
            - self
                .file
                .as_ref()
                .expect("unfinished pack writer")
                .buffer()
                .len() as u64;
        if written > self.allocated_bytes {
            self.write_budget
                .as_mut()
                .expect("writer budget")
                .consume_allocated(written - self.allocated_bytes)?;
            self.allocated_bytes = written;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<BuiltPack> {
        self.file
            .as_mut()
            .expect("unfinished pack writer")
            .flush()?;
        self.account_allocated()?;
        self.file
            .as_ref()
            .expect("unfinished pack writer")
            .get_ref()
            .sync_all()?;
        self.file.take();
        File::open(self.path.parent().expect("pack parent"))?.sync_all()?;
        if let Some(parent) = self.path.parent().and_then(Path::parent) {
            File::open(parent)?.sync_all()?;
        }
        self.committed = true;
        Ok(BuiltPack {
            pack_id: std::mem::take(&mut self.pack_id),
            path: std::mem::take(&mut self.path),
            bytes: self.bytes,
            entries: std::mem::take(&mut self.entries),
            checksum: format!("blake3:{}", self.hasher.finalize().to_hex()),
        })
    }
}

#[derive(Default)]
struct ChunkCompressor {
    context: Option<zstd::bulk::Compressor<'static>>,
    buffer: Vec<u8>,
}

impl ChunkCompressor {
    fn compress<'a>(&'a mut self, plaintext: &'a [u8]) -> Result<(PackCompression, &'a [u8])> {
        if !compression_hint(plaintext).should_compress {
            return Ok((PackCompression::None, plaintext));
        }
        if self.context.is_none() {
            // Keep the v2 format and measured/default level 1 unchanged.
            self.context = Some(zstd::bulk::Compressor::new(1)?);
        }
        let bound = zstd::zstd_safe::compress_bound(plaintext.len());
        if self.buffer.len() < bound {
            self.buffer.resize(bound, 0);
        }
        let length = self
            .context
            .as_mut()
            .expect("compression context")
            .compress_to_buffer(plaintext, &mut self.buffer[..])
            .map_err(|error| StateError::msg(format!("chunk compression failed: {error}")))?;
        if length >= plaintext.len() {
            Ok((PackCompression::None, plaintext))
        } else {
            Ok((PackCompression::Zstd, &self.buffer[..length]))
        }
    }
}

impl Drop for PackWriter {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl PackBuilder {
    pub fn new() -> Self {
        Self {
            target: PACK_TARGET,
            max: PACK_MAX,
            ..Self::default()
        }
    }

    pub fn with_limits(target: u64, max: u64) -> Self {
        Self {
            target,
            max,
            ..Self::default()
        }
    }

    pub fn add_file_chunks(&mut self, chunks: FileChunks, known: &mut dyn FnMut(&str) -> bool) {
        for (meta, payload) in chunks.chunks.into_iter().zip(chunks.payloads.into_iter()) {
            if known(&meta.hash) {
                continue;
            }
            self.add_chunk(meta.hash, payload);
        }
    }

    pub fn add_chunk(&mut self, hash: String, plaintext: Vec<u8>) {
        let size = plaintext.len() as u64;
        if self.current_size + size > self.max && !self.current.is_empty() {
            // caller must flush via finish_into
            panic!("pack builder overflowed max without flush; use add_chunk_to");
        }
        if self.current_size >= self.target && !self.current.is_empty() {
            return; // caller should flush; keep API simple via push_and_maybe_seal
        }
        self.current_size += size;
        self.current.push(Pending { hash, plaintext });
    }

    pub fn needs_flush(&self) -> bool {
        self.current_size >= self.target
    }

    pub fn flush(&mut self, dest_dir: &Path, master: &MasterKey) -> Result<Option<BuiltPack>> {
        if self.current.is_empty() {
            return Ok(None);
        }
        let pack = seal_pack(dest_dir, master, std::mem::take(&mut self.current))?;
        self.current_size = 0;
        self.finished.push(pack.clone());
        Ok(Some(pack))
    }

    pub fn finish(mut self, dest_dir: &Path, master: &MasterKey) -> Result<Vec<BuiltPack>> {
        self.flush(dest_dir, master)?;
        Ok(self.finished)
    }
}

pub fn pack_chunk_files<S: Into<ChunkSource>>(
    dest_dir: &Path,
    master: &MasterKey,
    chunks: impl IntoIterator<Item = (String, S)>,
    known: impl FnMut(&str) -> bool,
) -> Result<Vec<BuiltPack>> {
    pack_chunk_files_with_limits(dest_dir, master, chunks, known, PACK_TARGET, PACK_MAX)
}

pub fn pack_chunk_files_with_limits<S: Into<ChunkSource>>(
    dest_dir: &Path,
    master: &MasterKey,
    chunks: impl IntoIterator<Item = (String, S)>,
    mut known: impl FnMut(&str) -> bool,
    target: u64,
    max: u64,
) -> Result<Vec<BuiltPack>> {
    if target == 0 || target > max || max < PACK_HEADER_LEN {
        return Err(StateError::msg("invalid pack size limits"));
    }

    let mut current: Option<PackWriter> = None;
    let mut out = Vec::new();
    let mut reader = ChunkSourceReader::default();
    let mut payload = Vec::new();
    for (hash, source) in chunks {
        if known(&hash) {
            continue;
        }

        let source = source.into();
        let ciphertext_len = source
            .len()?
            .checked_add(AEAD_TAG_LEN)
            .ok_or_else(|| StateError::msg("chunk length exceeds pack format"))?;
        let record_len = RECORD_HEADER_LEN
            .checked_add(ciphertext_len)
            .ok_or_else(|| StateError::msg("chunk length exceeds pack format"))?;
        if PACK_HEADER_LEN
            .checked_add(record_len)
            .is_none_or(|bytes| bytes > max)
        {
            return Err(StateError::msg("chunk exceeds pack hard maximum"));
        }
        reader.read_verified(&source, &hash, &mut payload)?;

        let should_seal = current.as_ref().is_some_and(|pack| {
            pack.bytes >= target
                || pack
                    .bytes
                    .checked_add(record_len)
                    .is_none_or(|bytes| bytes > max)
        });
        if should_seal {
            out.push(current.take().expect("current pack exists").finish()?);
        }

        let pack = match current.as_mut() {
            Some(pack) => pack,
            None => current.insert(PackWriter::create(dest_dir, master)?),
        };
        pack.write_chunk(hash, &payload)?;
        debug_assert!(pack.bytes <= max);
    }

    if let Some(pack) = current {
        out.push(pack.finish()?);
    }
    Ok(out)
}

pub fn pack_chunks(
    dest_dir: &Path,
    master: &MasterKey,
    chunks: impl IntoIterator<Item = (String, Vec<u8>)>,
    mut known: impl FnMut(&str) -> bool,
) -> Result<Vec<BuiltPack>> {
    let mut builder = PackBuilder::new();
    let mut out = Vec::new();
    for (hash, payload) in chunks {
        if known(&hash) {
            continue;
        }
        if builder.current_size + payload.len() as u64 > builder.max && !builder.current.is_empty()
        {
            if let Some(pack) = builder.flush(dest_dir, master)? {
                out.push(pack);
            }
        }
        if builder.current_size >= builder.target && !builder.current.is_empty() {
            if let Some(pack) = builder.flush(dest_dir, master)? {
                out.push(pack);
            }
        }
        builder.current_size += payload.len() as u64;
        builder.current.push(Pending {
            hash,
            plaintext: payload,
        });
    }
    if let Some(pack) = builder.flush(dest_dir, master)? {
        out.push(pack);
    }
    Ok(out)
}

fn seal_pack(dest_dir: &Path, master: &MasterKey, pending: Vec<Pending>) -> Result<BuiltPack> {
    let mut writer = PackWriter::create(dest_dir, master)?;
    for item in pending {
        writer.write_chunk(item.hash, &item.plaintext)?;
    }
    let pack = writer.finish()?;
    if pack.bytes > PACK_MAX {
        let _ = std::fs::remove_file(&pack.path);
        return Err(StateError::msg("pack exceeded 1 GiB hard maximum"));
    }
    Ok(pack)
}

pub fn extract_chunk(
    pack_path: &Path,
    entry: &PackIndexEntry,
    master: &MasterKey,
) -> Result<Vec<u8>> {
    PackDecodeContext::new(&entry.pack_id, master)?.extract_chunk(pack_path, entry)
}

fn validate_entry(entry: &PackIndexEntry, pack_id: &str, minimum_offset: u64) -> Result<u64> {
    if entry.pack_id != pack_id
        || entry.offset < minimum_offset
        || entry.plaintext_len as u64 > noland_state_core::constants::FASTCDC_MAX
        || (entry.ciphertext_len as u64) < AEAD_TAG_LEN
        || entry.ciphertext_len as u64 > noland_state_core::constants::FASTCDC_MAX + 1024
        || entry.nonce.len() != NONCE_LEN
    {
        return Err(StateError::Integrity("invalid streaming pack index".into()));
    }
    let end = entry
        .offset
        .checked_add(RECORD_HEADER_LEN + u64::from(entry.ciphertext_len))
        .ok_or_else(|| StateError::Integrity("pack offset overflow".into()))?;
    if end > PACK_MAX {
        return Err(StateError::Integrity("pack exceeds format limit".into()));
    }
    Ok(end)
}

fn validate_pack_header(header: &[u8], pack_id: &str) -> Result<()> {
    if header.len() != PACK_HEADER_LEN as usize || &header[..4] != MAGIC {
        return Err(StateError::Integrity("bad pack header".into()));
    }
    let version = u32::from_le_bytes(header[4..8].try_into().expect("four version bytes"));
    let id = Uuid::from_slice(&header[8..24])
        .map_err(|_| StateError::Integrity("bad pack ID".into()))?;
    if !(1..=VERSION).contains(&version) || id.to_string() != pack_id {
        return Err(StateError::Integrity("bad pack version or ID".into()));
    }
    Ok(())
}

fn validate_record_header(header: &[u8], entry: &PackIndexEntry) -> Result<()> {
    if header.len() != RECORD_HEADER_LEN as usize
        || &header[..4] != b"NLCH"
        || u32::from_le_bytes(header[4..8].try_into().expect("four bytes")) != entry.plaintext_len
        || u32::from_le_bytes(header[8..12].try_into().expect("four bytes")) != entry.ciphertext_len
        || header[12..] != entry.nonce
    {
        return Err(StateError::Integrity("bad pack record".into()));
    }
    Ok(())
}

/// One derived cipher and one lazy zstd context per immutable pack. Can also be
/// reused for successive file-backed extractions via `extract_chunk`.
pub struct PackDecodeContext {
    pack_id: String,
    master: MasterKey,
    cipher: Decryptor,
    decompressor: Option<zstd::bulk::Decompressor<'static>>,
}

impl PackDecodeContext {
    pub fn new(pack_id: &str, master: &MasterKey) -> Result<Self> {
        Ok(Self {
            pack_id: pack_id.into(),
            master: master.clone(),
            cipher: Decryptor::new(&pack_key(master, pack_id))?,
            decompressor: None,
        })
    }

    pub fn extract_chunk(&mut self, pack_path: &Path, entry: &PackIndexEntry) -> Result<Vec<u8>> {
        use std::io::Seek;
        let end = validate_entry(entry, &self.pack_id, PACK_HEADER_LEN)?;
        let mut file = File::open(pack_path)?;
        let file_len = file.metadata()?.len();
        if end > file_len || file_len > PACK_MAX {
            return Err(StateError::Integrity(
                "pack record outside file bounds".into(),
            ));
        }
        let mut header = [0; PACK_HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        validate_pack_header(&header, &self.pack_id)?;
        file.seek(std::io::SeekFrom::Start(entry.offset))?;
        let mut header = [0; RECORD_HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        validate_record_header(&header, entry)?;
        let mut stored = vec![0; entry.ciphertext_len as usize];
        file.read_exact(&mut stored)?;
        self.decode_payload(entry, &mut stored)
    }

    fn decode_payload(&mut self, entry: &PackIndexEntry, stored: &mut Vec<u8>) -> Result<Vec<u8>> {
        if stored.len() != entry.ciphertext_len as usize {
            return Err(StateError::Integrity("bad pack record length".into()));
        }
        let nonce: [u8; NONCE_LEN] = entry
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| StateError::Integrity("bad pack nonce".into()))?;
        let length = self
            .cipher
            .decrypt_in_place(&nonce, entry.chunk_hash.as_bytes(), stored)?
            .len();
        stored.truncate(length);
        let plain = match entry.compression {
            PackCompression::None => std::mem::take(stored),
            PackCompression::Zstd => {
                if self.decompressor.is_none() {
                    self.decompressor = Some(zstd::bulk::Decompressor::new()?);
                }
                let plain = self
                    .decompressor
                    .as_mut()
                    .expect("decompression context")
                    .decompress(stored, entry.plaintext_len as usize)
                    .map_err(|error| {
                        StateError::Integrity(format!("chunk decompression failed: {error}"))
                    })?;
                stored.clear(); // Retain the compressed-input allocation for the next record.
                plain
            }
        };
        if plain.len() != entry.plaintext_len as usize || blake3_hex(&plain) != entry.chunk_hash {
            return Err(StateError::Integrity(
                "chunk hash or length mismatch".into(),
            ));
        }
        Ok(plain)
    }
}

/// Incremental decoder for immutable v1/v2 packs. Retains only the current
/// selected encrypted record, never a whole pack or a decrypted on-disk copy.
pub struct PackStreamDecoder {
    pack_id: String,
    entries: Vec<PackIndexEntry>,
    cursor: u64,
    header: Vec<u8>,
    next: usize,
    record_header: Vec<u8>,
    record: Vec<u8>,
    context: Option<PackDecodeContext>,
    failed: bool,
}

impl PackStreamDecoder {
    pub fn new(pack_id: &str, mut entries: Vec<PackIndexEntry>) -> Result<Self> {
        entries.sort_by_key(|entry| entry.offset);
        let mut end = PACK_HEADER_LEN;
        for entry in &entries {
            // Bound allocations using the authenticated index before reading any network length.
            end = validate_entry(entry, pack_id, end)?;
        }
        Ok(Self {
            pack_id: pack_id.into(),
            entries,
            cursor: 0,
            header: Vec::new(),
            next: 0,
            record_header: Vec::with_capacity(RECORD_HEADER_LEN as usize),
            record: Vec::new(),
            context: None,
            failed: false,
        })
    }

    pub fn received_bytes(&self) -> u64 {
        self.cursor
    }

    /// Limit one async consumer turn to one authenticated record, including any
    /// skipped prefix. Compressed input must not inflate many queued records at once.
    pub fn next_record_input_limit(&self) -> usize {
        if self.failed {
            return 1; // A subsequent push reports the failure instead of a zero-length turn.
        }
        self.entries.get(self.next).map_or(usize::MAX, |entry| {
            (entry.offset + RECORD_HEADER_LEN + u64::from(entry.ciphertext_len) - self.cursor)
                as usize
        })
    }

    pub fn push(
        &mut self,
        bytes: &[u8],
        master: &MasterKey,
        mut consume: impl FnMut(&PackIndexEntry, &[u8]) -> Result<()>,
    ) -> Result<()> {
        self.push_owned(bytes, master, |entry, plain| consume(entry, &plain))
    }

    /// Moves verified plaintext directly to the consumer. Use
    /// `next_record_input_limit` for one-record backpressure. After any error,
    /// restart with a new decoder; already accepted records may be replayed.
    pub fn push_owned(
        &mut self,
        bytes: &[u8],
        master: &MasterKey,
        consume: impl FnMut(&PackIndexEntry, Vec<u8>) -> Result<()>,
    ) -> Result<()> {
        if self.failed {
            return Err(StateError::Integrity(
                "streaming pack decoder requires restart".into(),
            ));
        }
        let result = self.push_owned_inner(bytes, master, consume);
        self.failed = result.is_err();
        result
    }

    fn push_owned_inner(
        &mut self,
        mut bytes: &[u8],
        master: &MasterKey,
        mut consume: impl FnMut(&PackIndexEntry, Vec<u8>) -> Result<()>,
    ) -> Result<()> {
        if let Some(context) = &self.context {
            if context.master.0 != master.0 {
                return Err(StateError::Integrity("streaming pack key changed".into()));
            }
        } else {
            self.context = Some(PackDecodeContext::new(&self.pack_id, master)?);
        }
        while !bytes.is_empty() {
            if self.header.len() < PACK_HEADER_LEN as usize {
                let count = bytes
                    .len()
                    .min(PACK_HEADER_LEN as usize - self.header.len());
                self.header.extend_from_slice(&bytes[..count]);
                self.cursor += count as u64;
                bytes = &bytes[count..];
                if self.header.len() == PACK_HEADER_LEN as usize {
                    validate_pack_header(&self.header, &self.pack_id)?;
                }
                continue;
            }
            let Some(entry) = self.entries.get(self.next) else {
                self.cursor = self
                    .cursor
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| StateError::Integrity("pack length overflow".into()))?;
                if self.cursor > PACK_MAX {
                    return Err(StateError::Integrity("pack exceeds format limit".into()));
                }
                break;
            };
            if self.cursor < entry.offset {
                let count = bytes.len().min((entry.offset - self.cursor) as usize);
                self.cursor += count as u64;
                bytes = &bytes[count..];
                continue;
            }
            if self.record_header.len() < RECORD_HEADER_LEN as usize {
                let count = bytes
                    .len()
                    .min(RECORD_HEADER_LEN as usize - self.record_header.len());
                self.record_header.extend_from_slice(&bytes[..count]);
                self.cursor += count as u64;
                bytes = &bytes[count..];
                if self.record_header.len() == RECORD_HEADER_LEN as usize {
                    validate_record_header(&self.record_header, entry)?;
                    self.record.reserve(entry.ciphertext_len as usize);
                }
                continue;
            }
            let length = entry.ciphertext_len as usize;
            let count = bytes.len().min(length - self.record.len());
            self.record.extend_from_slice(&bytes[..count]);
            self.cursor += count as u64;
            bytes = &bytes[count..];
            if self.record.len() == length {
                let plain = self
                    .context
                    .as_mut()
                    .expect("decode context")
                    .decode_payload(entry, &mut self.record)?;
                consume(entry, plain)?;
                self.record_header.clear();
                self.next += 1;
            }
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        if self.failed
            || self.header.len() != PACK_HEADER_LEN as usize
            || self.next != self.entries.len()
            || !self.record_header.is_empty()
            || !self.record.is_empty()
        {
            return Err(StateError::Integrity("truncated streaming pack".into()));
        }
        Ok(())
    }
}

pub fn index_by_hash(entries: &[PackIndexEntry]) -> BTreeMap<String, PackIndexEntry> {
    entries
        .iter()
        .cloned()
        .map(|e| (e.chunk_hash.clone(), e))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_cas::chunk_bytes;

    #[test]
    fn owned_decode_moves_raw_allocation_and_reuses_compressed_context() {
        let dir = std::env::temp_dir().join(format!("noland-owned-decode-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let payloads = [b"raw".to_vec(), vec![3; 128 * 1024], vec![7; 128 * 1024]];
        let packs = pack_chunks(
            &dir,
            &master,
            payloads.iter().map(|p| (blake3_hex(p), p.clone())),
            |_| false,
        )
        .unwrap();
        let pack = &packs[0];
        let bytes = std::fs::read(&pack.path).unwrap();
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
        let first_end = (pack.entries[0].offset
            + RECORD_HEADER_LEN
            + u64::from(pack.entries[0].ciphertext_len)) as usize;
        decoder
            .push_owned(&bytes[..first_end - 1], &master, |_, _| {
                panic!("incomplete tag released")
            })
            .unwrap();
        let allocation = decoder.record.as_ptr();
        let mut output = Vec::new();
        decoder
            .push_owned(&bytes[first_end - 1..first_end], &master, |_, plain| {
                assert_eq!(plain.as_ptr(), allocation);
                output.push(plain);
                Ok(())
            })
            .unwrap();
        for input in bytes[first_end..].chunks(17) {
            decoder
                .push_owned(input, &master, |_, plain| {
                    output.push(plain);
                    Ok(())
                })
                .unwrap();
        }
        decoder.finish().unwrap();
        assert_eq!(output, payloads);
        assert!(decoder.context.as_ref().unwrap().decompressor.is_some());
        let mut context = PackDecodeContext::new(&pack.pack_id, &master).unwrap();
        for (entry, expected) in pack.entries.iter().zip(&payloads) {
            assert_eq!(context.extract_chunk(&pack.path, entry).unwrap(), *expected);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_consumers_require_restart_and_fresh_decoders_replay_verified_records() {
        let dir = std::env::temp_dir().join(format!("noland-decode-retry-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let pack = pack_chunks(
            &dir,
            &master,
            [(blake3_hex(b"retry"), b"retry".to_vec())],
            |_| false,
        )
        .unwrap()
        .remove(0);
        let bytes = std::fs::read(&pack.path).unwrap();
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
        assert!(decoder
            .push_owned(&bytes, &master, |_, _| Err(StateError::Storage(
                "sink failed".into()
            )))
            .is_err());
        assert!(decoder.finish().is_err());
        assert!(decoder.next_record_input_limit() > 0);
        assert!(decoder.push_owned(&[], &master, |_, _| Ok(())).is_err());
        let mut fresh = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
        fresh
            .push_owned(&bytes, &master, |_, plain| {
                assert_eq!(plain, b"retry");
                Ok(())
            })
            .unwrap();
        fresh.finish().unwrap();
        let mut wrong_size = pack.entries[0].clone();
        wrong_size.plaintext_len += 1;
        let mut corrupt = bytes.clone();
        let length_offset = wrong_size.offset as usize + 4;
        corrupt[length_offset..length_offset + 4]
            .copy_from_slice(&wrong_size.plaintext_len.to_le_bytes());
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, vec![wrong_size]).unwrap();
        assert!(decoder
            .push_owned(&corrupt, &master, |_, _| panic!("wrong size released"))
            .is_err());
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
        decoder
            .push_owned(&bytes[..24], &master, |_, _| Ok(()))
            .unwrap();
        assert!(decoder
            .push_owned(&bytes[24..], &MasterKey::generate(), |_, _| panic!(
                "changed key released"
            ))
            .is_err());
        let mut invalid = pack.entries[0].clone();
        invalid.ciphertext_len = 15;
        assert!(PackStreamDecoder::new(&pack.pack_id, vec![invalid]).is_err());
        std::fs::write(&pack.path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(extract_chunk(&pack.path, &pack.entries[0], &master).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pack_write_windows_release_allocated_bytes_but_keep_buffered_output_reserved() {
        let root =
            std::env::temp_dir().join(format!("noland-pack-write-budget-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let mut writer = PackWriter::create(&root, &master).unwrap();
        writer.write_chunk(blake3_hex(b"tiny"), b"tiny").unwrap();
        assert_eq!(
            writer
                .file
                .as_ref()
                .unwrap()
                .get_ref()
                .metadata()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            writer.write_budget.as_ref().unwrap().remaining(),
            1024 * 1024
        );
        let mut payload = vec![3; 800 * 1024];
        payload[..2].copy_from_slice(b"\x1f\x8b"); // Deliberately bypass compression.
        for _ in 0..2 {
            writer.write_chunk(blake3_hex(&payload), &payload).unwrap();
            let allocated = writer
                .file
                .as_ref()
                .unwrap()
                .get_ref()
                .metadata()
                .unwrap()
                .len();
            assert_eq!(allocated, writer.allocated_bytes);
            assert!(writer.write_budget.as_ref().unwrap().remaining() < 256 * 1024);
        }
        let pack = writer.finish().unwrap();
        assert!(pack.bytes > 1024 * 1024);
        assert_eq!(pack.checksum, noland_cas::blake3_file(&pack.path).unwrap());
        for entry in pack.entries.iter().skip(1) {
            assert_eq!(extract_chunk(&pack.path, entry, &master).unwrap(), payload);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn highly_compressed_network_buffers_emit_at_most_one_record_per_turn() {
        let dir = std::env::temp_dir().join(format!("noland-stream-bound-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let chunks = (1u8..=4)
            .map(|value| {
                let bytes = vec![value; 1024 * 1024];
                (blake3_hex(&bytes), bytes)
            })
            .collect::<Vec<_>>();
        let packs = pack_chunks(&dir, &master, chunks, |_| false).unwrap();
        let pack = &packs[0];
        let input = std::fs::read(&pack.path).unwrap();
        assert_eq!(pack.checksum, noland_cas::blake3_file(&pack.path).unwrap());
        assert!(input.len() < 64 * 1024);
        let mut remaining = input.as_slice();
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
        let mut total = 0;
        while !remaining.is_empty() {
            let take = remaining.len().min(decoder.next_record_input_limit());
            assert!(take > 0);
            let mut emitted = 0;
            decoder
                .push(&remaining[..take], &master, |_, plain| {
                    emitted += 1;
                    assert_eq!(plain.len(), 1024 * 1024);
                    Ok(())
                })
                .unwrap();
            assert!(emitted <= 1);
            total += emitted;
            remaining = &remaining[take..];
        }
        decoder.finish().unwrap();
        assert_eq!(total, 4);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streaming_decoder_authenticates_fragmented_records_before_releasing_plaintext() {
        let dir = std::env::temp_dir().join(format!("noland-stream-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let payload = b"private saved state".repeat(500);
        let packs = pack_chunks(
            &dir,
            &master,
            vec![(blake3_hex(&payload), payload.clone())],
            |_| false,
        )
        .unwrap();
        let pack = &packs[0];
        let bytes = std::fs::read(&pack.path).unwrap();
        for version in [1u32, 2] {
            let mut bytes = bytes.clone();
            bytes[4..8].copy_from_slice(&version.to_le_bytes());
            let mut decoder = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
            let mut output = Vec::new();
            let split = bytes.len() - 1;
            for part in bytes[..split].chunks(3) {
                decoder
                    .push(part, &master, |_, plain| {
                        output.extend_from_slice(plain);
                        Ok(())
                    })
                    .unwrap();
            }
            assert!(
                output.is_empty(),
                "plaintext must wait for the complete authentication tag"
            );
            assert!(decoder.finish().is_err());
            decoder
                .push(&bytes[split..], &master, |_, plain| {
                    output.extend_from_slice(plain);
                    Ok(())
                })
                .unwrap();
            decoder.finish().unwrap();
            assert_eq!(output, payload);
        }
        let mut corrupt = bytes;
        *corrupt.last_mut().unwrap() ^= 1;
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, pack.entries.clone()).unwrap();
        let mut released = false;
        assert!(decoder
            .push(&corrupt, &master, |_, _| {
                released = true;
                Ok(())
            })
            .is_err());
        assert!(!released);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streaming_decoder_skips_unselected_records_and_bounds_index_allocations() {
        let dir = std::env::temp_dir().join(format!("noland-stream-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let packs = pack_chunks(
            &dir,
            &master,
            vec![
                (blake3_hex(b"unused"), b"unused".to_vec()),
                (blake3_hex(b"selected"), b"selected".to_vec()),
                (blake3_hex(b"trailing"), b"trailing".to_vec()),
            ],
            |_| false,
        )
        .unwrap();
        let pack = &packs[0];
        let entry = pack
            .entries
            .iter()
            .find(|entry| entry.chunk_hash == blake3_hex(b"selected"))
            .unwrap()
            .clone();
        let mut decoder = PackStreamDecoder::new(&pack.pack_id, vec![entry.clone()]).unwrap();
        let mut output = Vec::new();
        for bytes in std::fs::read(&pack.path).unwrap().chunks(5) {
            decoder
                .push(bytes, &master, |_, plain| {
                    output.extend_from_slice(plain);
                    Ok(())
                })
                .unwrap();
        }
        decoder.finish().unwrap();
        assert_eq!(output, b"selected");
        let mut invalid = entry;
        invalid.ciphertext_len = u32::MAX;
        assert!(PackStreamDecoder::new(&pack.pack_id, vec![invalid]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pack_encrypt_extract_and_never_mutates() {
        let dir = std::env::temp_dir().join(format!("noland-pack-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let data = b"minecraft-world-region".repeat(100);
        let chunks = chunk_bytes(&data);
        let hash = chunks.chunks[0].hash.clone();
        let packs = pack_chunks(
            &dir,
            &master,
            chunks
                .chunks
                .into_iter()
                .zip(chunks.payloads)
                .map(|(c, p)| (c.hash, p)),
            |_| false,
        )
        .unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].entries[0].compression, PackCompression::Zstd);
        assert!(packs[0].entries[0].ciphertext_len < packs[0].entries[0].plaintext_len);
        let extracted = extract_chunk(&packs[0].path, &packs[0].entries[0], &master).unwrap();
        assert_eq!(blake3_hex(&extracted), hash);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pack_builder_finish_returns_the_final_pack_once() {
        let dir = std::env::temp_dir().join(format!("noland-pack-{}", Uuid::new_v4()));
        let master = MasterKey::generate();
        let payload = b"one chunk".to_vec();
        let mut builder = PackBuilder::with_limits(64, 1024);
        builder.add_chunk(blake3_hex(&payload), payload);

        let packs = builder.finish(&dir, &master).unwrap();

        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].entries.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pack_chunk_files_respects_small_limits_and_round_trips() {
        let root = std::env::temp_dir().join(format!("noland-pack-{}", Uuid::new_v4()));
        let chunks_dir = root.join("chunks");
        let packs_dir = root.join("packs");
        std::fs::create_dir_all(&chunks_dir).unwrap();
        let master = MasterKey::generate();
        let mut expected = BTreeMap::new();
        let mut chunks = Vec::new();
        for index in 0..5 {
            let payload = vec![index; 10];
            let hash = blake3_hex(&payload);
            let path = chunks_dir.join(format!("{index}.chunk"));
            std::fs::write(&path, &payload).unwrap();
            expected.insert(hash.clone(), payload);
            chunks.push((hash, path));
        }
        let two_record_pack = PACK_HEADER_LEN + 2 * (RECORD_HEADER_LEN + 10 + AEAD_TAG_LEN);

        let packs = pack_chunk_files_with_limits(
            &packs_dir,
            &master,
            chunks,
            |_| false,
            two_record_pack - 1,
            two_record_pack,
        )
        .unwrap();

        assert_eq!(packs.len(), 3);
        assert_eq!(
            packs
                .iter()
                .map(|pack| pack.entries.len())
                .collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
        assert!(packs.iter().all(|pack| pack.bytes <= two_record_pack));
        assert!(packs[..2]
            .iter()
            .all(|pack| pack.bytes >= two_record_pack - 1));
        for pack in &packs {
            for entry in &pack.entries {
                let extracted = extract_chunk(&pack.path, entry, &master).unwrap();
                assert_eq!(&extracted, expected.get(&entry.chunk_hash).unwrap());
            }
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pack_chunk_files_rejects_a_chunk_over_the_hard_max() {
        let root = std::env::temp_dir().join(format!("noland-pack-{}", Uuid::new_v4()));
        let chunks_dir = root.join("chunks");
        let packs_dir = root.join("packs");
        std::fs::create_dir_all(&chunks_dir).unwrap();
        let payload = vec![7; 10];
        let path = chunks_dir.join("oversized.chunk");
        std::fs::write(&path, &payload).unwrap();
        let one_record_pack = PACK_HEADER_LEN + RECORD_HEADER_LEN + 10 + AEAD_TAG_LEN;

        let result = pack_chunk_files_with_limits(
            &packs_dir,
            &MasterKey::generate(),
            [(blake3_hex(&payload), path)],
            |_| false,
            one_record_pack - 1,
            one_record_pack - 1,
        );

        assert!(result.is_err());
        assert!(!packs_dir.exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn buffered_slice_packs_keep_v2_level_one_payloads_and_full_checksums() {
        let root = std::env::temp_dir().join(format!("noland-slice-pack-{}", Uuid::new_v4()));
        let cas = noland_cas::LocalCas::new(root.join("cas")).unwrap();
        let lease = cas.lease().unwrap();
        let master = MasterKey::generate();
        let payloads = [
            b"tiny".to_vec(),
            b"compressible-data".repeat(4096),
            vec![0x32; 256 * 1024],
        ];
        let chunks = payloads
            .iter()
            .map(|bytes| {
                let hash = blake3_hex(bytes);
                let source = lease.capture_prehashed(&hash, bytes).unwrap().source;
                (hash, source)
            })
            .collect::<Vec<_>>();
        lease.sync_all().unwrap();
        let packs = pack_chunk_files_with_limits(
            &root.join("packs"),
            &master,
            chunks,
            |_| false,
            1024 * 1024,
            2 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(packs.len(), 1);
        let pack = &packs[0];
        let bytes = std::fs::read(&pack.path).unwrap();
        assert_eq!(&bytes[4..8], &2u32.to_le_bytes());
        assert_eq!(pack.checksum, noland_cas::blake3_file(&pack.path).unwrap());
        for (entry, expected) in pack.entries.iter().zip(&payloads) {
            assert_eq!(
                extract_chunk(&pack.path, entry, &master).unwrap(),
                *expected
            );
        }
        let mut compressor = ChunkCompressor::default();
        for bytes in payloads.iter().skip(1).cycle().take(6) {
            let (compression, stored) = compressor.compress(bytes).unwrap();
            assert_eq!(compression, PackCompression::Zstd);
            assert_eq!(stored, zstd::bulk::compress(bytes, 1).unwrap());
        }
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_file_input_is_rejected_before_pack_publication() {
        let root = std::env::temp_dir().join(format!("noland-pack-input-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("input");
        std::fs::write(&path, b"changed").unwrap();
        let destination = root.join("packs");
        assert!(pack_chunk_files(
            &destination,
            &MasterKey::generate(),
            [(blake3_hex(b"original"), path)],
            |_| false
        )
        .is_err());
        assert!(!destination.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
