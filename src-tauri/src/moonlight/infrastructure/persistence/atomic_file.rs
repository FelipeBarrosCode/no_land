use std::path::Path;

use crate::{moonlight::domain::MoonlightError, utils::atomic_file};

pub fn write_atomically(path: &Path, contents: &[u8]) -> Result<(), MoonlightError> {
    atomic_file::write_atomically(path, contents).map_err(MoonlightError::from)
}
