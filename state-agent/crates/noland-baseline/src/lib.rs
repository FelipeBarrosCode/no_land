//! Image baseline and package-ownership lookups.

use std::fs;
use std::path::Path;

use noland_state_core::*;
use noland_state_db::StateDb;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BaselineEntry {
    pub path: String,
    pub file_type: Option<String>,
    pub size: Option<i64>,
    pub mode: Option<i64>,
    pub package_owner: Option<String>,
    pub baseline_hash: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BaselineManifest {
    pub image_id: String,
    pub entries: Vec<BaselineEntry>,
}

pub fn load_baseline_file(db: &StateDb, path: &Path) -> Result<String> {
    let text = fs::read_to_string(path)?;
    let manifest: BaselineManifest = serde_json::from_str(&text)?;
    load_baseline(db, &manifest)?;
    Ok(manifest.image_id)
}

pub fn load_baseline(db: &StateDb, manifest: &BaselineManifest) -> Result<()> {
    for entry in &manifest.entries {
        db.insert_baseline(
            &manifest.image_id,
            &entry.path,
            entry.file_type.as_deref(),
            entry.size,
            entry.mode,
            entry.package_owner.as_deref(),
            entry.baseline_hash.as_deref(),
        )?;
    }
    Ok(())
}

pub fn matches_baseline(db: &StateDb, image_id: &str, path: &Path, size: Option<u64>) -> Result<bool> {
    let Some((baseline_size, _, _)) = db.baseline_entry(image_id, &path.to_string_lossy())? else {
        return Ok(false);
    };
    if let (Some(expected), Some(actual)) = (baseline_size, size) {
        return Ok(expected == actual as i64);
    }
    Ok(true)
}

pub fn package_owner(db: &StateDb, image_id: &str, path: &Path) -> Result<Option<String>> {
    Ok(db
        .baseline_entry(image_id, &path.to_string_lossy())?
        .and_then(|(_, owner, _)| owner))
}

/// Best-effort dpkg lookup on Debian-like images. Never required.
pub fn query_dpkg_owner(path: &Path) -> Option<String> {
    let output = std::process::Command::new("dpkg")
        .args(["-S", &path.to_string_lossy()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split(':').next().map(|s| s.trim().to_string())
}

pub fn current_image_id() -> String {
    if let Ok(id) = std::env::var("NOLAND_IMAGE_ID") {
        if !id.is_empty() {
            return id;
        }
    }
    std::fs::read_to_string("/etc/noland/image-id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-image".into())
}
