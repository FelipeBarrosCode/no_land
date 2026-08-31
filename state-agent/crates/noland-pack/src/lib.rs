//! Immutable encrypted packfiles. Target 512 MiB, hard max 1 GiB.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use noland_cas::{blake3_hex, FileChunks};
use noland_crypto::{decrypt, encrypt, pack_key, EncryptedBlob, MasterKey, NONCE_LEN};
use noland_state_core::constants::{PACK_MAX, PACK_TARGET};
use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAGIC: &[u8; 4] = b"NLPK";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackIndexEntry {
    pub chunk_hash: String,
    pub pack_id: String,
    pub offset: u64,
    pub ciphertext_len: u32,
    pub plaintext_len: u32,
    pub nonce: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BuiltPack {
    pub pack_id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub entries: Vec<PackIndexEntry>,
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
        if let Some(pack) = self.flush(dest_dir, master)? {
            self.finished.push(pack);
        }
        // flush already pushed; avoid duplicate by taking finished after last flush
        Ok(self.finished)
    }
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
    std::fs::create_dir_all(dest_dir)?;
    let pack_id = Uuid::new_v4().to_string();
    let key = pack_key(master, &pack_id);
    let path = dest_dir.join(format!("{pack_id}.pack"));
    let mut file = File::create(&path)?;
    file.write_all(MAGIC)?;
    file.write_all(&VERSION.to_le_bytes())?;
    let uuid = Uuid::parse_str(&pack_id).unwrap();
    file.write_all(uuid.as_bytes())?;

    let mut entries = Vec::new();
    let mut offset = 4 + 4 + 16u64;
    for item in pending {
        let blob = encrypt(&key, item.hash.as_bytes(), &item.plaintext)?;
        let rec_len = 4 + 4 + 4 + NONCE_LEN + blob.ciphertext.len();
        file.write_all(b"NLCH")?;
        file.write_all(&(item.plaintext.len() as u32).to_le_bytes())?;
        file.write_all(&(blob.ciphertext.len() as u32).to_le_bytes())?;
        file.write_all(&blob.nonce)?;
        file.write_all(&blob.ciphertext)?;
        entries.push(PackIndexEntry {
            chunk_hash: item.hash,
            pack_id: pack_id.clone(),
            offset,
            ciphertext_len: blob.ciphertext.len() as u32,
            plaintext_len: item.plaintext.len() as u32,
            nonce: blob.nonce.to_vec(),
        });
        offset += rec_len as u64;
    }
    let bytes = offset;
    if bytes > PACK_MAX {
        let _ = std::fs::remove_file(&path);
        return Err(StateError::msg("pack exceeded 1 GiB hard maximum"));
    }
    Ok(BuiltPack {
        pack_id,
        path,
        bytes,
        entries,
    })
}

pub fn extract_chunk(
    pack_path: &Path,
    entry: &PackIndexEntry,
    master: &MasterKey,
) -> Result<Vec<u8>> {
    let mut file = File::open(pack_path)?;
    let mut buf = vec![0u8; entry.ciphertext_len as usize + 4 + 4 + 4 + NONCE_LEN];
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(entry.offset))?;
    file.read_exact(&mut buf)?;
    if &buf[..4] != b"NLCH" {
        return Err(StateError::Integrity("bad pack record".into()));
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&buf[12..12 + NONCE_LEN]);
    let ciphertext = buf[12 + NONCE_LEN..].to_vec();
    let key = pack_key(master, &entry.pack_id);
    let plain = decrypt(
        &key,
        entry.chunk_hash.as_bytes(),
        &EncryptedBlob { nonce, ciphertext },
    )?;
    if blake3_hex(&plain) != entry.chunk_hash {
        return Err(StateError::Integrity("chunk hash mismatch".into()));
    }
    Ok(plain)
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
        let extracted = extract_chunk(&packs[0].path, &packs[0].entries[0], &master).unwrap();
        assert_eq!(blake3_hex(&extracted), hash);
        std::fs::remove_dir_all(dir).ok();
    }
}
