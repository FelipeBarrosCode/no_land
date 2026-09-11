use std::collections::{BTreeMap, BTreeSet};
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
        ("steam", home.join(".steam/steam")),
        ("debian", home.join(".steam/debian-installation")),
        ("local", home.join(".local/share/Steam")),
        (
            "flatpak",
            home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        ),
        ("system", PathBuf::from("/usr/share/steam")),
    ];
    let mut roots = Vec::new();
    let mut seen_roots = BTreeSet::new();
    for (root_id, root) in candidates {
        if root.exists() && seen_roots.insert(path_identity(&root)) {
            roots.push((root_id, root));
        }
    }
    let root = roots.first()?.1.clone();

    let mut libraries = Vec::new();
    let mut seen_library_paths = BTreeSet::new();
    let mut used_library_ids = BTreeSet::new();
    for (root_id, steam_root) in &roots {
        add_library(
            &mut libraries,
            &mut seen_library_paths,
            &mut used_library_ids,
            root_id,
            "0",
            steam_root.join("steamapps"),
        );

        let vdf = steam_root.join("steamapps/libraryfolders.vdf");
        if let Ok(text) = fs::read_to_string(&vdf) {
            for (id, path) in parse_libraryfolders(&text) {
                let path = PathBuf::from(path);
                let library_root = if path.is_absolute() {
                    path
                } else {
                    steam_root.join(path)
                };
                add_library(
                    &mut libraries,
                    &mut seen_library_paths,
                    &mut used_library_ids,
                    root_id,
                    &id,
                    library_root.join("steamapps"),
                );
            }
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
    apps.sort_by(|a, b| {
        (&a.app_id, &a.install_dir, &a.library_id).cmp(&(&b.app_id, &b.install_dir, &b.library_id))
    });

    Some(SteamDiscovery {
        root,
        libraries,
        apps,
    })
}

fn add_library(
    libraries: &mut Vec<(String, PathBuf)>,
    seen_paths: &mut BTreeSet<PathBuf>,
    used_ids: &mut BTreeSet<String>,
    root_id: &str,
    library_id: &str,
    steamapps: PathBuf,
) {
    let emitted_path = path_identity(&steamapps);
    if !seen_paths.insert(emitted_path.clone()) {
        return;
    }

    let id = unique_library_id(used_ids, root_id, library_id);
    libraries.push((id, emitted_path));
}

fn unique_library_id(used_ids: &mut BTreeSet<String>, root_id: &str, library_id: &str) -> String {
    if used_ids.insert(library_id.to_string()) {
        return library_id.to_string();
    }

    let qualified = format!("{root_id}:{library_id}");
    if used_ids.insert(qualified.clone()) {
        return qualified;
    }

    let mut suffix = 2;
    loop {
        let candidate = format!("{qualified}:{suffix}");
        if used_ids.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn path_identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn parse_appmanifest(text: &str, library_id: &str, steamapps: &Path) -> Option<SteamApp> {
    let map = parse_acf(text);
    let app_id: u32 = map.get("appid")?.parse().ok()?;
    let name = map
        .get("name")
        .cloned()
        .unwrap_or_else(|| format!("Steam {app_id}"));
    let installdir = map
        .get("installdir")
        .filter(|dir| !dir.trim().is_empty())
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
        prefix: Some(prefix),
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
        if tokens.is_empty() {
            continue;
        }
        if tokens[0].chars().all(|c| c.is_ascii_digit()) {
            if tokens.len() >= 2 {
                out.push((tokens[0].to_string(), tokens[1].to_string()));
                current_id = None;
            } else {
                current_id = Some(tokens[0].to_string());
            }
        } else if tokens.len() >= 2 && tokens[0] == "path" {
            if let Some(id) = current_id.take() {
                out.push((id, tokens[1].to_string()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noland-steam-{label}-{}", uuid::Uuid::new_v4()))
    }

    fn write_manifest(steamapps: &Path, app_id: u32, name: &str, install_dir: &str) {
        std::fs::create_dir_all(steamapps).unwrap();
        std::fs::write(
            steamapps.join(format!("appmanifest_{app_id}.acf")),
            format!(
                r#"
                "AppState"
                {{
                    "appid" "{app_id}"
                    "name" "{name}"
                    "installdir" "{install_dir}"
                }}
                "#
            ),
        )
        .unwrap();
    }

    #[test]
    fn discovers_debian_packaged_steam_root() {
        let home = temp_home("debian-discovery");
        let root = home.join(".steam/debian-installation");
        std::fs::create_dir_all(root.join("steamapps")).unwrap();

        let discovery = discover_steam(&home).expect("Debian Steam root should be discovered");

        assert_eq!(discovery.root, root);
        assert_eq!(
            discovery.libraries,
            vec![("0".into(), root.join("steamapps"))]
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn discovers_apps_across_all_roots_and_libraries() {
        let home = temp_home("all-roots");
        let conventional_root = home.join(".steam/steam");
        let flatpak_root = home.join(".var/app/com.valvesoftware.Steam/data/Steam");
        let external_root = home.join("Games/SteamLibrary");
        let conventional_apps = conventional_root.join("steamapps");
        let flatpak_apps = flatpak_root.join("steamapps");
        let external_apps = external_root.join("steamapps");

        write_manifest(&conventional_apps, 10, "Conventional", "Conventional");
        write_manifest(&flatpak_apps, 20, "Flatpak", "Flatpak");
        write_manifest(&external_apps, 30, "External", "External");
        std::fs::write(
            conventional_apps.join("libraryfolders.vdf"),
            format!(
                r#"
                "libraryfolders"
                {{
                    "0"
                    {{
                        "path" "{}"
                    }}
                    "1"
                    {{
                        "path" "{}"
                    }}
                }}
                "#,
                conventional_root.display(),
                external_root.display()
            ),
        )
        .unwrap();

        let discovery = discover_steam(&home).expect("Steam roots should be discovered");

        assert_eq!(discovery.root, conventional_root);
        assert_eq!(discovery.libraries.len(), 3);
        assert!(discovery
            .libraries
            .iter()
            .any(|(_, path)| path == &conventional_apps));
        assert!(discovery
            .libraries
            .iter()
            .any(|(_, path)| path == &external_apps));
        assert!(discovery
            .libraries
            .iter()
            .any(|(_, path)| path == &flatpak_apps));
        assert_eq!(
            discovery
                .libraries
                .iter()
                .map(|(id, _)| id)
                .collect::<BTreeSet<_>>()
                .len(),
            discovery.libraries.len()
        );
        assert_eq!(
            discovery
                .apps
                .iter()
                .map(|app| app.app_id)
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );

        let flatpak = discovery.apps.iter().find(|app| app.app_id == 20).unwrap();
        assert_eq!(flatpak.install_dir, flatpak_apps.join("common/Flatpak"));
        assert_eq!(
            flatpak.prefix.as_deref(),
            Some(flatpak_apps.join("compatdata/20/pfx").as_path())
        );
        assert!(!flatpak.prefix.as_ref().unwrap().exists());

        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_symlinked_library_and_app_roots() {
        let home = temp_home("symlinked-root");
        let linked_root = home.join(".steam/steam");
        let real_root = home.join(".local/share/Steam");
        let real_apps = real_root.join("steamapps");
        write_manifest(&real_apps, 40, "Linked", "Linked");
        std::fs::create_dir_all(linked_root.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_root, &linked_root).unwrap();

        let discovery = discover_steam(&home).expect("symlinked Steam root should be discovered");
        let canonical_apps = std::fs::canonicalize(&real_apps).unwrap();

        assert_eq!(discovery.root, linked_root);
        assert_eq!(
            discovery.libraries,
            vec![("0".into(), canonical_apps.clone())]
        );
        assert_eq!(discovery.apps.len(), 1);
        assert_eq!(
            discovery.apps[0].install_dir,
            canonical_apps.join("common/Linked")
        );
        assert_eq!(
            discovery.apps[0].prefix,
            Some(canonical_apps.join("compatdata/40/pfx"))
        );

        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn parses_legacy_libraryfolders_entries() {
        let vdf = r#"
        "LibraryFolders"
        {
            "1" "/mnt/games/SteamLibrary"
            "2" "/media/games/SteamLibrary"
        }
        "#;

        assert_eq!(
            parse_libraryfolders(vdf),
            vec![
                ("1".into(), "/mnt/games/SteamLibrary".into()),
                ("2".into(), "/media/games/SteamLibrary".into()),
            ]
        );
    }

    #[test]
    fn parses_appmanifest_with_derived_roots_before_they_exist() {
        let acf = r#"
        "AppState"
        {
            "appid"		"480"
            "name"		"Spacewar"
            "installdir"		"Spacewar"
        }
        "#;
        let steamapps = Path::new("/steam/steamapps");
        let app = parse_appmanifest(acf, "0", steamapps).unwrap();
        assert_eq!(app.app_id, 480);
        assert_eq!(app.name, "Spacewar");
        assert_eq!(app.install_dir, steamapps.join("common/Spacewar"));
        assert_eq!(app.prefix, Some(steamapps.join("compatdata/480/pfx")));
    }
}
