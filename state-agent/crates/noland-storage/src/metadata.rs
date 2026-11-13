//! Compact JSON -> optional NLZ1 -> authenticated NLNE, without whole JSON copies.

use std::io::{self, BufReader, BufWriter, Read, Write};

use noland_crypto::{seal_envelope_in_place, unwrap_envelope_in_place, ENVELOPE_HEADER_LEN};
use noland_state_core::{Result, StateError};
use serde::{de::DeserializeOwned, Serialize};

const COMPRESSED_DOCUMENT_MAGIC: &[u8; 4] = b"NLZ1";
const COMPRESSION_MIN_BYTES: usize = 4 * 1024;
/// Hard decoded-document limit, also enforced while serializing new documents.
pub const MAX_METADATA_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_METADATA_ENVELOPE_BYTES: u64 = MAX_METADATA_BYTES + 1024 * 1024;

pub struct SealedMetadata {
    pub envelope: Vec<u8>,
    /// BLAKE3 of the exact compact JSON emitted by serde_json, before compression.
    pub json_hash: String,
}

/// Small documents stay raw. Larger documents are serialized directly into the
/// existing NLZ1 format; only the small pre-compression prefix is buffered raw.
pub fn seal_metadata<T: Serialize + ?Sized>(
    key: &[u8; 32],
    aad: &[u8],
    value: &T,
) -> Result<SealedMetadata> {
    let mut writer = DocumentWriter::new();
    {
        let mut buffered = BufWriter::with_capacity(64 * 1024, &mut writer);
        serde_json::to_writer(&mut buffered, value)?;
        buffered.flush()?;
    }
    writer.seal(key, aad)
}

/// Compatibility path for callers that already have JSON. New callers should
/// pass their value to seal_metadata instead of allocating serialized JSON.
pub fn seal_metadata_json(key: &[u8; 32], aad: &[u8], json: &[u8]) -> Result<SealedMetadata> {
    let mut writer = DocumentWriter::new();
    writer.write_all(json)?;
    writer.seal(key, aad)
}

pub fn compact_json_hash<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let mut writer = HashWriter(blake3::Hasher::new());
    {
        let mut buffered = BufWriter::with_capacity(64 * 1024, &mut writer);
        serde_json::to_writer(&mut buffered, value)?;
        buffered.flush()?;
    }
    Ok(format!("blake3:{}", writer.0.finalize().to_hex()))
}

struct HashWriter(blake3::Hasher);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct DocumentWriter {
    raw: Vec<u8>,
    compressed: Option<zstd::stream::write::Encoder<'static, Vec<u8>>>,
    hash: blake3::Hasher,
    length: u64,
}

impl DocumentWriter {
    fn new() -> Self {
        let mut raw = Vec::with_capacity(ENVELOPE_HEADER_LEN + COMPRESSION_MIN_BYTES);
        raw.resize(ENVELOPE_HEADER_LEN, 0);
        Self {
            raw,
            compressed: None,
            hash: blake3::Hasher::new(),
            length: 0,
        }
    }

    fn seal(self, key: &[u8; 32], aad: &[u8]) -> Result<SealedMetadata> {
        let mut envelope = match self.compressed {
            Some(encoder) => encoder.finish()?,
            None => self.raw,
        };
        seal_envelope_in_place(key, aad, &mut envelope)?;
        Ok(SealedMetadata {
            envelope,
            json_hash: format!("blake3:{}", self.hash.finalize().to_hex()),
        })
    }
}

impl Write for DocumentWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = self
            .length
            .checked_add(bytes.len() as u64)
            .filter(|length| *length <= MAX_METADATA_BYTES)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "metadata exceeds decoded size limit",
                )
            })?;
        if self.compressed.is_none() && length >= COMPRESSION_MIN_BYTES as u64 {
            let mut output = vec![0; ENVELOPE_HEADER_LEN];
            output.extend_from_slice(COMPRESSED_DOCUMENT_MAGIC);
            let mut encoder = zstd::stream::write::Encoder::new(output, 3)?;
            encoder.write_all(&self.raw[ENVELOPE_HEADER_LEN..])?;
            self.raw = Vec::new(); // Drop the raw prefix promptly, not at the end of serialization.
            self.compressed = Some(encoder);
        }
        match &mut self.compressed {
            Some(encoder) => encoder.write_all(bytes)?,
            None => self.raw.extend_from_slice(bytes),
        }
        self.hash.update(bytes);
        self.length = length;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Consumes the encrypted allocation and authenticates it completely before any
/// decompression/deserialization. Accepts legacy raw JSON and NLZ1 documents.
pub fn open_metadata<T: DeserializeOwned>(key: &[u8; 32], aad: &[u8], bytes: Vec<u8>) -> Result<T> {
    open_metadata_with_limit(key, aad, bytes, MAX_METADATA_BYTES)
}

pub fn open_metadata_with_limit<T: DeserializeOwned>(
    key: &[u8; 32],
    aad: &[u8],
    mut bytes: Vec<u8>,
    max_decoded_bytes: u64,
) -> Result<T> {
    if bytes.len() as u64 > MAX_METADATA_ENVELOPE_BYTES {
        return Err(StateError::Integrity(
            "metadata envelope exceeds size limit".into(),
        ));
    }
    let plain = unwrap_envelope_in_place(key, aad, &mut bytes)?;
    let limit = max_decoded_bytes.min(MAX_METADATA_BYTES);
    if let Some(compressed) = plain.strip_prefix(COMPRESSED_DOCUMENT_MAGIC) {
        let mut decoder = zstd::stream::read::Decoder::with_buffer(compressed)
            .map_err(|e| StateError::Integrity(format!("metadata decompression failed: {e}")))?;
        // Bound decoder workspace independently of the frame's untrusted window.
        decoder
            .window_log_max(27)
            .map_err(|e| StateError::Integrity(format!("metadata decompression failed: {e}")))?;
        let reader = BufReader::with_capacity(
            64 * 1024,
            LimitedReader {
                inner: decoder,
                remaining: limit,
            },
        );
        // from_reader consumes through EOF, so trailing JSON, malformed frames,
        // and bytes beyond the limit cannot hide after a valid JSON prefix.
        serde_json::from_reader(reader)
            .map_err(|e| StateError::Integrity(format!("invalid metadata: {e}")))
    } else {
        if plain.len() as u64 > limit {
            return Err(StateError::Integrity(
                "metadata exceeds decoded size limit".into(),
            ));
        }
        serde_json::from_slice(plain)
            .map_err(|e| StateError::Integrity(format!("invalid metadata: {e}")))
    }
}

struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}
impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "metadata exceeds decoded size limit",
            ));
        }
        let count = buffer
            .len()
            .min(self.remaining.min(usize::MAX as u64) as usize);
        let read = self.inner.read(&mut buffer[..count])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_crypto::{unwrap_envelope, wrap_envelope};
    use serde_json::{json, Value};

    #[test]
    fn compact_hash_and_raw_or_compressed_documents_match_legacy_json() {
        let key = [3; 32];
        for value in [
            json!({"unicode": "é\n🦀", "n": 1.25}),
            json!({"records": vec!["repeated"; 5000]}),
        ] {
            let expected = serde_json::to_vec(&value).unwrap();
            let sealed = seal_metadata(&key, b"document", &value).unwrap();
            let expected_hash = format!("blake3:{}", blake3::hash(&expected).to_hex());
            assert_eq!(sealed.json_hash, expected_hash);
            assert_eq!(compact_json_hash(&value).unwrap(), expected_hash);
            let encoded = unwrap_envelope(&key, b"document", &sealed.envelope).unwrap();
            if expected.len() >= COMPRESSION_MIN_BYTES {
                assert!(encoded.starts_with(COMPRESSED_DOCUMENT_MAGIC));
                assert_eq!(zstd::stream::decode_all(&encoded[4..]).unwrap(), expected);
            } else {
                assert_eq!(encoded, expected);
            }
            assert_eq!(
                open_metadata::<Value>(&key, b"document", sealed.envelope).unwrap(),
                value
            );
            // Old writers used pretty/raw JSON or bulk-compressed level-three JSON.
            let raw = serde_json::to_vec_pretty(&value).unwrap();
            let compressed = [b"NLZ1".as_slice(), &zstd::bulk::compress(&raw, 3).unwrap()].concat();
            for legacy in [raw, compressed] {
                let envelope = wrap_envelope(&key, b"document", &legacy).unwrap();
                assert_eq!(
                    open_metadata::<Value>(&key, b"document", envelope).unwrap(),
                    value
                );
            }
        }
    }

    #[test]
    fn authentication_corruption_truncation_and_decoded_limits_are_enforced() {
        let key = [4; 32];
        let value = json!({"text": "x".repeat(32 * 1024)});
        let expected_len = serde_json::to_vec(&value).unwrap().len() as u64;
        let sealed = seal_metadata(&key, b"document", &value).unwrap().envelope;
        assert!(open_metadata::<Value>(&[5; 32], b"document", sealed.clone()).is_err());
        assert!(open_metadata::<Value>(&key, b"wrong-aad", sealed.clone()).is_err());
        let mut corrupt = sealed.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(open_metadata::<Value>(&key, b"document", corrupt).is_err());
        assert!(open_metadata_with_limit::<Value>(
            &key,
            b"document",
            sealed.clone(),
            expected_len - 1
        )
        .is_err());
        assert_eq!(
            open_metadata_with_limit::<Value>(&key, b"document", sealed, expected_len).unwrap(),
            value
        );
        let compressed = zstd::bulk::compress(&serde_json::to_vec(&value).unwrap(), 3).unwrap();
        for invalid in [
            [b"NLZ1".as_slice(), &compressed[..compressed.len() - 1]].concat(),
            [b"NLZ1".as_slice(), &compressed, b"junk"].concat(),
            b"{} {}".to_vec(),
            b"NLZ1garbage".to_vec(),
        ] {
            let envelope = wrap_envelope(&key, b"document", &invalid).unwrap();
            assert!(open_metadata::<Value>(&key, b"document", envelope).is_err());
        }
        // A take(limit) reader alone could incorrectly accept this valid prefix.
        let trailing = [b"{}".as_slice(), &vec![b' '; 5000]].concat();
        let envelope = seal_metadata_json(&key, b"document", &trailing)
            .unwrap()
            .envelope;
        assert!(open_metadata_with_limit::<Value>(&key, b"document", envelope, 2).is_err());
    }
}
