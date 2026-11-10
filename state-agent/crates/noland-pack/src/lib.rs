//! Immutable encrypted packfiles. Target 512 MiB, hard max 1 GiB.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use noland_cas::{blake3_hex, compression_hint, FileChunks};
use noland_crypto::{decrypt, encrypt, pack_key, EncryptedBlob, MasterKey, NONCE_LEN};
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
    file: Option<File>,
    key: [u8; 32],
    bytes: u64,
    entries: Vec<PackIndexEntry>,
    committed: bool,
    hasher: blake3::Hasher,
}

impl PackWriter {
    fn create(dest_dir: &Path, master: &MasterKey) -> Result<Self> {
        std::fs::create_dir_all(dest_dir)?;
        let pack_id = Uuid::new_v4().to_string();
        let key = pack_key(master, &pack_id);
        let path = dest_dir.join(format!("{pack_id}.pack"));
        let mut file = File::create(&path)?;
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
        })
    }

    fn write_chunk(&mut self, hash: String, plaintext: &[u8]) -> Result<()> {
        let plaintext_len = u32::try_from(plaintext.len())
            .map_err(|_| StateError::msg("chunk plaintext length exceeds pack format"))?;
        let (compression, stored) = compress_chunk(plaintext)?;
        noland_state_core::disk::ensure_space(&self.path, stored.len() as u64 + 64)?;
        let blob = encrypt(&self.key, hash.as_bytes(), stored.as_ref())?;
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

    fn finish(mut self) -> Result<BuiltPack> {
        self.file
            .as_mut()
            .expect("unfinished pack writer")
            .flush()?;
        self.file
            .as_ref()
            .expect("unfinished pack writer")
            .sync_all()?;
        self.file.take();
        File::open(self.path.parent().expect("pack parent"))?.sync_all()?;
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

fn compress_chunk(plaintext: &[u8]) -> Result<(PackCompression, Cow<'_, [u8]>)> {
    if !compression_hint(plaintext).should_compress {
        return Ok((PackCompression::None, Cow::Borrowed(plaintext)));
    }
    let compressed = zstd::bulk::compress(plaintext, 1)
        .map_err(|error| StateError::msg(format!("chunk compression failed: {error}")))?;
    if compressed.len() >= plaintext.len() {
        Ok((PackCompression::None, Cow::Borrowed(plaintext)))
    } else {
        Ok((PackCompression::Zstd, Cow::Owned(compressed)))
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

pub fn pack_chunk_files(
    dest_dir: &Path,
    master: &MasterKey,
    chunks: impl IntoIterator<Item = (String, PathBuf)>,
    known: impl FnMut(&str) -> bool,
) -> Result<Vec<BuiltPack>> {
    pack_chunk_files_with_limits(dest_dir, master, chunks, known, PACK_TARGET, PACK_MAX)
}

pub fn pack_chunk_files_with_limits(
    dest_dir: &Path,
    master: &MasterKey,
    chunks: impl IntoIterator<Item = (String, PathBuf)>,
    mut known: impl FnMut(&str) -> bool,
    target: u64,
    max: u64,
) -> Result<Vec<BuiltPack>> {
    if target == 0 || target > max || max < PACK_HEADER_LEN {
        return Err(StateError::msg("invalid pack size limits"));
    }

    let mut current: Option<PackWriter> = None;
    let mut out = Vec::new();
    for (hash, path) in chunks {
        if known(&hash) {
            continue;
        }

        let payload = std::fs::read(path)?;
        let ciphertext_len = u64::try_from(payload.len())
            .ok()
            .and_then(|len| len.checked_add(AEAD_TAG_LEN))
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
    let mut file = File::open(pack_path)?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    if &header[..4] != MAGIC {
        return Err(StateError::Integrity("bad pack header".into()));
    }
    let version = u32::from_le_bytes(header[4..8].try_into().expect("four version bytes"));
    if !(1..=VERSION).contains(&version) {
        return Err(StateError::Integrity(format!(
            "unsupported pack version {version}"
        )));
    }
    let mut buf = vec![0u8; entry.ciphertext_len as usize + 4 + 4 + 4 + NONCE_LEN];
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(entry.offset))?;
    file.read_exact(&mut buf)?;
    decode_record(&buf, entry, master)
}

fn decode_record(buf: &[u8], entry: &PackIndexEntry, master: &MasterKey) -> Result<Vec<u8>> {
    if buf.len() != RECORD_HEADER_LEN as usize + entry.ciphertext_len as usize
        || &buf[..4] != b"NLCH"
        || u32::from_le_bytes(buf[4..8].try_into().expect("four bytes")) != entry.plaintext_len
        || u32::from_le_bytes(buf[8..12].try_into().expect("four bytes")) != entry.ciphertext_len
        || buf[12..12 + NONCE_LEN] != entry.nonce
    {
        return Err(StateError::Integrity("bad pack record".into()));
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&buf[12..12 + NONCE_LEN]);
    let ciphertext = buf[12 + NONCE_LEN..].to_vec();
    let key = pack_key(master, &entry.pack_id);
    let stored = decrypt(
        &key,
        entry.chunk_hash.as_bytes(),
        &EncryptedBlob { nonce, ciphertext },
    )?;
    let plain = match entry.compression {
        PackCompression::None => stored,
        PackCompression::Zstd => zstd::bulk::decompress(&stored, entry.plaintext_len as usize)
            .map_err(|error| {
                StateError::Integrity(format!("chunk decompression failed: {error}"))
            })?,
    };
    if plain.len() != entry.plaintext_len as usize || blake3_hex(&plain) != entry.chunk_hash {
        return Err(StateError::Integrity(
            "chunk hash or length mismatch".into(),
        ));
    }
    Ok(plain)
}

/// Incremental decoder for immutable v1/v2 packs. Retains only the current
/// selected encrypted record, never a whole pack or a decrypted on-disk copy.
pub struct PackStreamDecoder {
    pack_id: String,
    entries: Vec<PackIndexEntry>,
    cursor: u64,
    header: Vec<u8>,
    next: usize,
    record: Vec<u8>,
}

impl PackStreamDecoder {
    pub fn new(pack_id: &str, mut entries: Vec<PackIndexEntry>) -> Result<Self> {
        entries.sort_by_key(|entry| entry.offset);
        let mut end = PACK_HEADER_LEN;
        for entry in &entries {
            // Bound allocations using the authenticated index before reading any network length.
            if entry.pack_id != pack_id
                || entry.offset < end
                || entry.plaintext_len as u64 > noland_state_core::constants::FASTCDC_MAX
                || entry.ciphertext_len as u64 > noland_state_core::constants::FASTCDC_MAX + 1024
                || entry.nonce.len() != NONCE_LEN
            {
                return Err(StateError::Integrity("invalid streaming pack index".into()));
            }
            end = entry
                .offset
                .checked_add(RECORD_HEADER_LEN + u64::from(entry.ciphertext_len))
                .ok_or_else(|| StateError::Integrity("pack offset overflow".into()))?;
            if end > PACK_MAX {
                return Err(StateError::Integrity("pack exceeds format limit".into()));
            }
        }
        Ok(Self {
            pack_id: pack_id.into(),
            entries,
            cursor: 0,
            header: Vec::new(),
            next: 0,
            record: Vec::new(),
        })
    }

    pub fn received_bytes(&self) -> u64 {
        self.cursor
    }

    /// Limit one async consumer turn to one authenticated record, including any
    /// skipped prefix. Compressed input must not inflate many queued records at once.
    pub fn next_record_input_limit(&self) -> usize {
        self.entries.get(self.next).map_or(usize::MAX, |entry| {
            (entry.offset + RECORD_HEADER_LEN + u64::from(entry.ciphertext_len) - self.cursor)
                as usize
        })
    }

    pub fn push(
        &mut self,
        mut bytes: &[u8],
        master: &MasterKey,
        mut consume: impl FnMut(&PackIndexEntry, &[u8]) -> Result<()>,
    ) -> Result<()> {
        while !bytes.is_empty() {
            if self.header.len() < PACK_HEADER_LEN as usize {
                let count = bytes
                    .len()
                    .min(PACK_HEADER_LEN as usize - self.header.len());
                self.header.extend_from_slice(&bytes[..count]);
                self.cursor += count as u64;
                bytes = &bytes[count..];
                if self.header.len() == PACK_HEADER_LEN as usize {
                    let version =
                        u32::from_le_bytes(self.header[4..8].try_into().expect("four bytes"));
                    let id = Uuid::from_slice(&self.header[8..24])
                        .map_err(|_| StateError::Integrity("bad pack ID".into()))?;
                    if &self.header[..4] != MAGIC
                        || !(1..=VERSION).contains(&version)
                        || id.to_string() != self.pack_id
                    {
                        return Err(StateError::Integrity("bad streaming pack header".into()));
                    }
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
            let length = RECORD_HEADER_LEN as usize + entry.ciphertext_len as usize;
            let count = bytes.len().min(length - self.record.len());
            self.record.extend_from_slice(&bytes[..count]);
            self.cursor += count as u64;
            bytes = &bytes[count..];
            if self.record.len() == length {
                let plain = decode_record(&self.record, entry, master)?;
                consume(entry, &plain)?;
                self.record.clear();
                self.next += 1;
            }
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        if self.header.len() != PACK_HEADER_LEN as usize
            || self.next != self.entries.len()
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
}
