//! Read ELF dependency metadata without executing the dropped binary or `ldd`.
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Requirements {
    pub needed: Vec<String>,
    pub search_paths: Vec<String>,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn read_at(file: &mut File, offset: u64, length: u64) -> Result<Vec<u8>> {
    anyhow::ensure!(
        offset
            .checked_add(length)
            .is_some_and(|end| end <= file.metadata().map(|m| m.len()).unwrap_or(0)),
        "ELF metadata extends outside the file"
    );
    anyhow::ensure!(
        length <= 16 * 1024 * 1024,
        "ELF metadata exceeds inspection limit"
    );
    let mut bytes = vec![0; length as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn inspect(path: &Path) -> Result<Requirements> {
    let mut file = File::open(path)?;
    let header = read_at(&mut file, 0, 64)?;
    anyhow::ensure!(
        &header[..6] == b"\x7fELF\x02\x01" && u16_at(&header, 18) == 62,
        "Expected a Linux x86-64 ELF file"
    );
    let count = u16_at(&header, 56) as u64;
    let stride = u16_at(&header, 54) as u64;
    anyhow::ensure!(
        count <= 4096 && (count == 0 || stride == 56),
        "Unsupported ELF program headers"
    );
    let headers = read_at(&mut file, u64_at(&header, 32), count * stride)?;
    let mut loads = Vec::new();
    let mut dynamic = None;
    for ph in headers.as_chunks::<56>().0 {
        let kind = u32::from_le_bytes(ph[..4].try_into().unwrap());
        let offset = u64_at(ph, 8);
        let address = u64_at(ph, 16);
        let size = u64_at(ph, 32);
        if kind == 1 {
            loads.push((address, offset, size));
        }
        if kind == 2 {
            dynamic = Some((offset, size));
        }
    }
    let Some((offset, size)) = dynamic else {
        return Ok(Requirements::default());
    };
    anyhow::ensure!(
        size <= 1024 * 1024 && size % 16 == 0,
        "Invalid ELF dynamic table"
    );
    let table = read_at(&mut file, offset, size)?;
    let mut strings_address = None;
    let mut strings_size = None;
    let mut wanted = Vec::new();
    for entry in table.as_chunks::<16>().0 {
        let tag = u64_at(entry, 0);
        let value = u64_at(entry, 8);
        match tag {
            0 => break,
            5 => strings_address = Some(value),
            10 => strings_size = Some(value),
            1 | 15 | 29 => wanted.push((tag, value)),
            _ => {}
        }
    }
    if wanted.is_empty() {
        return Ok(Requirements::default());
    }
    anyhow::ensure!(wanted.len() <= 1024, "Too many ELF dependencies");
    let address = strings_address.context("ELF dependency string table is missing")?;
    let size = strings_size.context("ELF string table size is missing")?;
    let offset = loads
        .into_iter()
        .find_map(|(base, offset, bytes)| {
            let relative = address.checked_sub(base)?;
            (relative.checked_add(size)? <= bytes)
                .then(|| offset.checked_add(relative))
                .flatten()
        })
        .context("ELF string table is outside loadable file data")?;
    let strings = read_at(&mut file, offset, size)?;
    let mut result = Requirements::default();
    for (tag, offset) in wanted {
        let start = usize::try_from(offset).context("Invalid ELF string offset")?;
        let tail = strings
            .get(start..)
            .context("ELF string is out of bounds")?;
        let end = tail
            .iter()
            .take(4097)
            .position(|byte| *byte == 0)
            .context("Unterminated or excessive ELF string")?;
        let text = std::str::from_utf8(&tail[..end])
            .context("ELF dependency is not UTF-8")?
            .to_owned();
        if tag == 1 {
            anyhow::ensure!(
                !text.is_empty()
                    && !text.starts_with('-')
                    && text.len() <= 255
                    && text
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c)),
                "Unsupported dependency name; only library sonames can be downloaded"
            );
            result.needed.push(text);
        } else {
            result
                .search_paths
                .extend(text.split(':').filter(|p| !p.is_empty()).map(str::to_owned));
        }
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn elf(soname: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 512];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x1000u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&512u64.to_le_bytes());
        bytes[120..124].copy_from_slice(&2u32.to_le_bytes());
        bytes[128..136].copy_from_slice(&192u64.to_le_bytes());
        bytes[152..160].copy_from_slice(&64u64.to_le_bytes());
        for (index, (tag, value)) in [(5u64, 0x1100u64), (10, 128), (1, 0), (0, 0)]
            .into_iter()
            .enumerate()
        {
            let offset = 192 + index * 16;
            bytes[offset..offset + 8].copy_from_slice(&tag.to_le_bytes());
            bytes[offset + 8..offset + 16].copy_from_slice(&value.to_le_bytes());
        }
        bytes[256..256 + soname.len()].copy_from_slice(soname);
        bytes
    }
    #[test]
    fn reads_dependencies_without_executing_the_file_and_rejects_package_injection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("not-executable");
        std::fs::write(&path, elf(b"libSDL2-2.0.so.0\0")).unwrap();
        assert_eq!(inspect(&path).unwrap().needed, ["libSDL2-2.0.so.0"]);
        for name in [
            b"../../bin/sh\0".as_slice(),
            b"--option\0",
            b"libx;touch owned\0",
        ] {
            std::fs::write(&path, elf(name)).unwrap();
            assert!(inspect(&path).is_err());
        }
        let mut bytes = elf(b"libx.so\0");
        bytes[200..208].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(inspect(&path).is_err());
        assert!(!temp.path().join("owned").exists());
    }
}
