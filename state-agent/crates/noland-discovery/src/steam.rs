use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use noland_state_core::*;

#[derive(Debug, Clone)]
pub struct SteamDiscovery {
    pub root: PathBuf,
    pub libraries: Vec<(String, PathBuf)>,
    pub apps: Vec<SteamApp>,
}

#[derive(Debug, Clone)]
pub struct SteamApp {
    pub app_id: u32,
    pub name: String,
    pub install_dir: PathBuf,
    pub library_id: String,
    pub prefix: Option<PathBuf>,
}

impl SteamApp {
    pub fn to_identity(&self) -> AppIdentity {
        AppIdentity {
            steam_app_id: Some(self.app_id),
            launcher: Some(LauncherKind::Steam),
            identity_confidence: 1.0,
            canonical_executable: None,
            ..AppIdentity::new(AppId::steam(self.app_id), self.name.clone())
        }
    }
}

pub fn discover_steam(home: &Path) -> Option<SteamDiscovery> {
    let candidates = [
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        PathBuf::from("/usr/share/steam"),
    ];
    let root = candidates.into_iter().find(|p| p.exists())?;
    let mut libraries = vec![("0".into(), root.join("steamapps"))];
    let vdf = root.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = fs::read_to_string(&vdf) {
        for (id, path) in parse_libraryfolders(&text) {
            libraries.push((id, PathBuf::from(path).join("steamapps")));
        }
    }
    let mut apps = Vec::new();
    for (id, steamapps) in &libraries {
        let Ok(entries) = fs::read_dir(steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(entry.path()) {
                if let Some(app) = parse_appmanifest(&text, id, steamapps) {
                    apps.push(app);
                }
            }
        }
    }
    Some(SteamDiscovery {
        root,
        libraries,
        apps,
    })
}

pub fn parse_appmanifest(text: &str, library_id: &str, steamapps: &Path) -> Option<SteamApp> {
    let map = parse_acf(text);
    let app_id: u32 = map.get("appid")?.parse().ok()?;
    let name = map.get("name").cloned().unwrap_or_else(|| format!("Steam {app_id}"));
    let installdir = map
        .get("installdir")
        .cloned()
        .unwrap_or_else(|| name.clone());
    let install_dir = steamapps.join("common").join(&installdir);
    let prefix = steamapps
        .join("compatdata")
        .join(app_id.to_string())
        .join("pfx");
    Some(SteamApp {
        app_id,
        name,
        install_dir,
        library_id: library_id.into(),
        prefix: prefix.exists().then_some(prefix),
    })
}

/// Minimal ACF/VDF leaf parser. Enough for appmanifest and libraryfolders.
pub fn parse_acf(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split('"').filter(|p| !p.trim().is_empty());
        if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
            if !k.contains('{') && !v.contains('{') {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

pub fn parse_vdf_map(text: &str) -> BTreeMap<String, String> {
    parse_acf(text)
}

fn parse_libraryfolders(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current_id = None;
    for line in text.lines() {
        let tokens: Vec<&str> = line.split('"').filter(|p| !p.trim().is_empty()).collect();
        if tokens.len() == 1 && tokens[0].chars().all(|c| c.is_ascii_digit()) {
            current_id = Some(tokens[0].to_string());
        } else if tokens.len() >= 2 && tokens[0] == "path" {
            if let Some(id) = current_id.clone() {
                out.push((id, tokens[1].to_string()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_appmanifest() {
        let acf = r#"
        "AppState"
        {
            "appid"		"480"
            "name"		"Spacewar"
            "installdir"		"Spacewar"
        }
        "#;
        let app = parse_appmanifest(acf, "0", Path::new("/steam/steamapps")).unwrap();
        assert_eq!(app.app_id, 480);
        assert_eq!(app.name, "Spacewar");
    }
}
