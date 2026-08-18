use std::fs;
use std::path::{Path, PathBuf};

use noland_state_core::*;

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: Option<PathBuf>,
    pub icon: Option<PathBuf>,
    pub path: PathBuf,
}

impl DesktopEntry {
    pub fn to_identity(&self) -> AppIdentity {
        AppIdentity {
            desktop_entry_id: Some(self.id.clone()),
            canonical_executable: self.exec.clone(),
            launcher: Some(LauncherKind::Native),
            identity_confidence: 0.85,
            icon_path: self.icon.clone(),
            ..AppIdentity::new(AppId::desktop(&self.id), self.name.clone())
        }
    }
}

pub fn discover_desktop_apps(home: &Path) -> Vec<AppIdentity> {
    let mut dirs = vec![
        home.join(".local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Ok(xdg) = std::env::var("XDG_DATA_DIRS") {
        for part in xdg.split(':') {
            dirs.push(PathBuf::from(part).join("applications"));
        }
    }
    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(parsed) = parse_desktop_entry(&path) {
                let identity = parsed.to_identity();
                let user_installed = path.starts_with(home);
                if user_installed || crate::portable::is_backup_candidate(&identity) {
                    apps.push(identity);
                }
            }
        }
    }
    apps
}

pub fn parse_desktop_entry(path: &Path) -> Option<DesktopEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut in_desktop_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Name" => name = Some(v.trim().to_string()),
                "Exec" => exec = Some(first_exec_path(v.trim())),
                "Icon" => icon = Some(PathBuf::from(v.trim())),
                "NoDisplay" if v.trim().eq_ignore_ascii_case("true") => return None,
                "Hidden" if v.trim().eq_ignore_ascii_case("true") => return None,
                _ => {}
            }
        }
    }
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    Some(DesktopEntry {
        name: name.unwrap_or_else(|| id.clone()),
        id,
        exec,
        icon,
        path: path.to_path_buf(),
    })
}

fn first_exec_path(exec: &str) -> PathBuf {
    let token = exec
        .split_whitespace()
        .find(|t| !t.starts_with('%') && !t.starts_with('-'))
        .unwrap_or(exec);
    PathBuf::from(token.trim_matches('"'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_desktop_file() {
        let dir = std::env::temp_dir().join(format!("noland-desktop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("example-game.desktop");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "[Desktop Entry]\nName=Example Game\nExec=/opt/example-game/bin/game %u\nIcon=example\n"
        )
        .unwrap();
        let parsed = parse_desktop_entry(&path).unwrap();
        assert_eq!(parsed.name, "Example Game");
        assert_eq!(parsed.exec.unwrap(), PathBuf::from("/opt/example-game/bin/game"));
        std::fs::remove_dir_all(dir).ok();
    }
}
