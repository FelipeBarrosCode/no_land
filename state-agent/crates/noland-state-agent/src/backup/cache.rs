//! Authenticated local metadata and index-observation receipts. These are cache
//! hints only: missing/invalid receipts always cause fresh loading or seeding.
use super::*;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

struct HashWriter<W> {
    writer: W,
    hash: blake3::Hasher,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn authenticator(master: &MasterKey, context: &str) -> blake3::Hasher {
    let mut hash = blake3::Hasher::new_keyed(&master.0);
    hash.update(b"noland-authenticated-cache-v1\0");
    hash.update(context.as_bytes());
    hash.update(&[0]);
    hash
}

pub(super) fn write_json(
    path: &Path,
    value: &impl serde::Serialize,
    master: &MasterKey,
    context: &str,
) -> Result<()> {
    let partial = path.with_extension(format!("{}.partial", Uuid::new_v4()));
    let result = (|| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        let mut writer = HashWriter {
            writer: BufWriter::new(file),
            hash: authenticator(master, context),
        };
        serde_json::to_writer(&mut writer, value)?;
        writer.flush()?;
        writer.writer.get_ref().sync_all()?;
        let tag = writer.hash.finalize().to_hex().to_string();
        // A crash between data/tag replacement invalidates this disposable cache.
        std::fs::rename(&partial, path)?;
        std::fs::write(path.with_extension("mac"), tag)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(partial);
    result
}

pub(super) fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    master: &MasterKey,
    context: &str,
) -> Option<T> {
    let expected =
        blake3::Hash::from_hex(std::fs::read_to_string(path.with_extension("mac")).ok()?).ok()?;
    let mut file = open_regular_nofollow(path).ok()?;
    let before = file.metadata().ok()?;
    let mut hash = authenticator(master, context);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if hash.finalize() != expected {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let value = serde_json::from_reader(BufReader::new(&mut file)).ok()?;
    // Parse the same descriptor that was authenticated; reject mutation in place.
    if !noland_snapshot::same_file_identity(&before, &file.metadata().ok()?) {
        return None;
    }
    Some(value)
}

pub(super) fn index_hash(index: &[PackIndexEntry]) -> Result<String> {
    let mut writer = HashWriter {
        writer: std::io::sink(),
        hash: blake3::Hasher::new(),
    };
    serde_json::to_writer(&mut writer, index)?;
    Ok(writer.hash.finalize().to_hex().to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct IndexReceipt {
    index_hash: String,
    entry: PackIndexEntry,
    tag: String,
}

fn entry_tag(
    master: &MasterKey,
    binding: &str,
    index_hash: &str,
    entry: &PackIndexEntry,
) -> Result<String> {
    let mut writer = HashWriter {
        writer: std::io::sink(),
        hash: authenticator(master, &format!("remote-index:{binding}:{index_hash}")),
    };
    serde_json::to_writer(&mut writer, entry)?;
    Ok(writer.hash.finalize().to_hex().to_string())
}

pub(super) fn index_receipt(
    master: &MasterKey,
    binding: &str,
    index_hash: &str,
    entry: &PackIndexEntry,
) -> Result<String> {
    // Serialize a borrowed entry: no duplicate nonce/hash/path allocation.
    #[derive(serde::Serialize)]
    struct Receipt<'a> {
        index_hash: &'a str,
        entry: &'a PackIndexEntry,
        tag: String,
    }
    Ok(serde_json::to_string(&Receipt {
        index_hash,
        entry,
        tag: entry_tag(master, binding, index_hash, entry)?,
    })?)
}

pub(super) fn read_index_receipt(
    master: &MasterKey,
    binding: &str,
    payload: &str,
) -> Option<PackIndexEntry> {
    let receipt: IndexReceipt = serde_json::from_str(payload).ok()?;
    let expected = blake3::Hash::from_hex(&receipt.tag).ok()?;
    let actual = blake3::Hash::from_hex(
        entry_tag(master, binding, &receipt.index_hash, &receipt.entry).ok()?,
    )
    .ok()?;
    (actual == expected).then_some(receipt.entry)
}
