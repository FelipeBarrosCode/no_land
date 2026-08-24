//! BLAKE3 content addressing and FastCDC 1/4/8 MiB chunking.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use noland_state_core::constants::{FASTCDC_AVG, FASTCDC_MAX, FASTCDC_MIN};
use noland_state_core::{ChunkRef, Result};

pub fn blake3_hex(data: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(data).to_hex())
}

pub fn blake3_file(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

#[derive(Debug, Clone)]
pub struct FileChunks {
    pub file_hash: String,
    pub size: u64,
    pub chunks: Vec<ChunkRef>,
    pub payloads: Vec<Vec<u8>>,
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

pub fn chunk_file(path: &Path) -> Result<FileChunks> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    Ok(chunk_bytes(&data))
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
    std::fs::create_dir_all(dir)?;
    for (chunk, payload) in chunks.chunks.iter().zip(chunks.payloads.iter()) {
        let name = chunk.hash.trim_start_matches("blake3:");
        let path = dir.join(name);
        if !path.exists() {
            let mut f = File::create(path)?;
            f.write_all(payload)?;
        }
    }
    Ok(())
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
}
