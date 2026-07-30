use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

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
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}
