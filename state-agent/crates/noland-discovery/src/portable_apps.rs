use std::path::{Path, PathBuf};
use noland_state_core::{AppId, AppIdentity, LauncherKind};

pub fn discover_portable_apps(home: &Path) -> Vec<AppIdentity> {
    let mut apps = Vec::new();
    let dirs = vec![home.join("Downloads"), home.join("Desktop")];
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue; };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("jar") || ext.eq_ignore_ascii_case("appimage") || ext.eq_ignore_ascii_case("exe") {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    if stem.trim().is_empty() { continue; }
                    let app_id = AppId(format!("desktop:{}", stem.to_lowercase()));
                    let mut identity = AppIdentity::new(app_id, stem.clone());
                    identity.canonical_executable = Some(path.clone());
                    identity.launcher = Some(LauncherKind::Native);
                    apps.push(identity);
                }
            }
        }
    }
    apps
}
