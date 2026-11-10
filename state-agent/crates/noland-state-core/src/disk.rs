//! Per-filesystem admission for storage work; reservations never delete user data.
use crate::{Result, StateError};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

static RESERVED: OnceLock<Mutex<BTreeMap<u64, u64>>> = OnceLock::new();

pub fn safety_margin() -> u64 {
    std::env::var("NOLAND_STORAGE_MIN_FREE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64 * 1024 * 1024)
}

#[cfg(unix)]
pub fn available(path: &Path) -> Result<(u64, u64)> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
    };
    let ancestor = path.ancestors().find(|path| path.exists()).ok_or_else(|| {
        StateError::Storage(format!("no existing filesystem for {}", path.display()))
    })?;
    let name = CString::new(ancestor.as_os_str().as_bytes())
        .map_err(|_| StateError::UnsafePath("NUL in storage path".into()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: name is NUL-terminated and stat points to writable statvfs storage.
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((
        std::fs::metadata(ancestor)?.dev(),
        (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64),
    ))
}

#[cfg(not(unix))]
pub fn available(_path: &Path) -> Result<(u64, u64)> {
    Err(StateError::Storage(
        "storage disk admission requires a Unix agent".into(),
    ))
}

pub fn ensure_space(path: &Path, bytes: u64) -> Result<()> {
    let (_, free) = available(path)?;
    require(free, bytes, safety_margin(), path)
}

fn require(free: u64, required: u64, margin: u64, path: &Path) -> Result<()> {
    if free < required.saturating_add(margin) {
        return Err(StateError::Storage(format!(
            "insufficient storage at {}: {free} bytes available, {required} required plus {margin} safety margin", path.display()
        )));
    }
    Ok(())
}

pub struct DiskReservation(Vec<(u64, u64)>);

impl DiskReservation {
    pub fn reserve(requests: &[(PathBuf, u64)]) -> Result<Self> {
        let mut devices = BTreeMap::<u64, (u64, u64, &Path)>::new();
        for (path, bytes) in requests {
            let (device, free) = available(path)?;
            let entry = devices.entry(device).or_insert((free, 0, path));
            entry.0 = entry.0.min(free);
            entry.1 = entry
                .1
                .checked_add(*bytes)
                .ok_or_else(|| StateError::Storage("storage reservation overflow".into()))?;
        }
        let mut reserved = RESERVED
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| StateError::Storage("storage reservation lock poisoned".into()))?;
        for (device, (free, bytes, path)) in &devices {
            require(
                *free,
                bytes.saturating_add(*reserved.get(device).unwrap_or(&0)),
                safety_margin(),
                path,
            )?;
        }
        let mut owned = Vec::new();
        for (device, (_, bytes, _)) in devices {
            *reserved.entry(device).or_default() += bytes;
            owned.push((device, bytes));
        }
        Ok(Self(owned))
    }
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        if let Ok(mut reserved) = RESERVED.get_or_init(Mutex::default).lock() {
            for (device, bytes) in &self.0 {
                if let Some(total) = reserved.get_mut(device) {
                    *total = total.saturating_sub(*bytes);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_includes_margin_and_overflow_fails_closed() {
        assert!(require(100, 80, 20, Path::new("fixture")).is_ok());
        assert!(require(99, 80, 20, Path::new("fixture")).is_err());
        assert!(require(100, u64::MAX, 20, Path::new("fixture")).is_err());
    }
}
