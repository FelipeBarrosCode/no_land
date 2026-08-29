use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(1);

use crate::moonlight::domain::MoonlightError;

pub fn write_atomically(path: &Path, contents: &[u8]) -> Result<(), MoonlightError> {
    let parent = path.parent().ok_or_else(|| {
        MoonlightError::Persistence(format!("missing parent directory for {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;

    let tmp_path = temp_path(path);
    let mut file = File::create(&tmp_path)?;
    file.write_all(contents)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    replace_file(&tmp_path, path)?;

    #[cfg(unix)]
    File::open(parent)?.sync_all()?;

    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
    os.push(format!(".tmp.{}.{id}", std::process::id()));
    PathBuf::from(os)
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomically_replaces_existing_file() {
        let root = std::env::temp_dir().join(format!(
            "noland-atomic-file-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("cache.json");

        write_atomically(&path, b"first").unwrap();
        write_atomically(&path, b"second").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert!(!root.join("cache.json.tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
