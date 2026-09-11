//! Application discovery: .desktop, Steam, Proton, Wine, Bottles.

mod desktop;
mod path_policy;
mod portable;
mod portable_apps;
mod steam;
mod wine;

pub use desktop::{discover_desktop_apps, parse_desktop_entry, DesktopEntry};
pub use path_policy::{classify_observed_path, PathDisposition, PathPolicyDecision};
pub use portable::{filter_backup_candidates, is_backup_candidate, is_system_desktop_path};
pub use steam::{discover_steam, parse_acf, parse_vdf_map, SteamApp, SteamDiscovery};
pub use wine::{discover_bottles, discover_wine_prefixes, PrefixDiscovery, PrefixKind};

use std::path::{Path, PathBuf};

use noland_state_core::*;

#[derive(Debug, Clone, Default)]
pub struct DiscoveryScan {
    pub apps: Vec<AppIdentity>,
    pub steam: Option<SteamDiscovery>,
    pub wine_prefixes: Vec<PrefixDiscovery>,
    pub bottles: Vec<PrefixDiscovery>,
}

pub fn discover_all(home: &Path) -> DiscoveryScan {
    let mut scan = DiscoveryScan::default();
    scan.apps.extend(discover_desktop_apps(home));
    scan.apps
        .extend(portable_apps::discover_portable_apps(home));
    for app in &mut scan.apps {
        specialize_portable_launcher(app);
    }
    if let Some(steam) = discover_steam(home) {
        for app in &steam.apps {
            scan.apps.push(app.to_identity());
        }
        scan.steam = Some(steam);
    }
    scan.wine_prefixes = discover_wine_prefixes(home);
    scan.bottles = discover_bottles(home);
    scan.apps = normalize_identities(scan.apps);

    // Prefix identities must survive name-based normalization: a bottle or Wine prefix
    // can legitimately have the same display name as a native desktop application.
    for identity in scan
        .wine_prefixes
        .iter()
        .chain(&scan.bottles)
        .filter_map(PrefixDiscovery::to_identity)
    {
        if !scan.apps.iter().any(|app| app.app_id == identity.app_id) {
            scan.apps.push(identity);
        }
    }
    scan
}

fn specialize_portable_launcher(app: &mut AppIdentity) {
    if app.launcher != Some(LauncherKind::Native) {
        return;
    }
    let Some(extension) = app
        .canonical_executable
        .as_ref()
        .and_then(|path| path.extension())
        .and_then(|extension| extension.to_str())
    else {
        return;
    };
    if extension.eq_ignore_ascii_case("appimage") {
        app.launcher = Some(LauncherKind::AppImage);
    } else if extension.eq_ignore_ascii_case("exe") {
        app.launcher = Some(LauncherKind::Wine);
    }
}

/// Derive the narrowest durable install root represented by a discovered executable.
///
/// Only existing regular files below `home` qualify. Shared user collections are never
/// returned wholesale: an app subdirectory is selected, or the executable itself for a
/// single-file portable app.
pub fn derive_owned_install_root(home: &Path, app: &AppIdentity) -> Option<PathBuf> {
    if !matches!(
        app.launcher,
        Some(LauncherKind::Native | LauncherKind::AppImage | LauncherKind::Wine)
    ) {
        return None;
    }
    let executable = app.canonical_executable.as_ref()?;
    if !executable.is_absolute() {
        return None;
    }

    let canonical_home = std::fs::canonicalize(home).ok()?;
    let canonical_executable = std::fs::canonicalize(executable).ok()?;
    if !canonical_executable.is_file() {
        return None;
    }
    let relative = canonical_executable.strip_prefix(&canonical_home).ok()?;
    let components = relative.iter().collect::<Vec<_>>();
    if components.is_empty() {
        return None;
    }

    let first = components[0].to_str()?;
    let root_depth = if first == ".local" {
        local_install_root_depth(&components)?
    } else if first == ".var" {
        if components.get(1).and_then(|part| part.to_str()) == Some("app") && components.len() >= 4
        {
            3
        } else {
            return None;
        }
    } else if is_shared_home_collection(first) {
        if components.len() >= 3 {
            2
        } else {
            components.len()
        }
    } else if is_shared_hidden_root(first) {
        return None;
    } else if components.len() == 1 {
        components.len()
    } else if root_name_matches_app(first, app) {
        1
    } else {
        return None;
    };

    let relative_root = components.iter().take(root_depth).collect::<PathBuf>();
    let root = canonical_home.join(relative_root);
    (root != canonical_home).then_some(root)
}

fn local_install_root_depth(components: &[&std::ffi::OsStr]) -> Option<usize> {
    let second = components.get(1)?.to_str()?;
    match second {
        "share" | "opt" => {
            if components.len() < 3 {
                return None;
            }
            let app_dir = components[2].to_str()?;
            if is_shared_local_data_root(app_dir) {
                return None;
            }
            Some(if components.len() >= 4 {
                3
            } else {
                components.len()
            })
        }
        "bin" => Some(if components.len() >= 4 {
            3
        } else {
            components.len()
        }),
        _ => None,
    }
}

fn root_name_matches_app(root_name: &str, app: &AppIdentity) -> bool {
    let root_name = normalize_name(root_name);
    if root_name.is_empty() {
        return false;
    }
    std::iter::once(app.display_name.as_str())
        .chain(app.aliases.iter().map(String::as_str))
        .any(|name| normalize_name(name) == root_name)
        || app
            .canonical_executable
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|name| name.to_str())
            .is_some_and(|name| normalize_name(name) == root_name)
}

fn is_shared_home_collection(name: &str) -> bool {
    [
        "Applications",
        "AppImages",
        "apps",
        "bin",
        "Desktop",
        "Documents",
        "Downloads",
        "Games",
        "Music",
        "opt",
        "Pictures",
        "Public",
        "Templates",
        "Videos",
    ]
    .iter()
    .any(|shared| name.eq_ignore_ascii_case(shared))
}

fn is_shared_hidden_root(name: &str) -> bool {
    [
        ".cache",
        ".config",
        ".local",
        ".mozilla",
        ".steam",
        ".var",
        ".wine",
        ".wine-prefixes",
    ]
    .iter()
    .any(|shared| name.eq_ignore_ascii_case(shared))
}

fn is_shared_local_data_root(name: &str) -> bool {
    [
        "applications",
        "bottles",
        "flatpak",
        "fonts",
        "icons",
        "lutris",
        "mime",
        "sounds",
        "Steam",
        "themes",
        "Trash",
        "wineprefixes",
    ]
    .iter()
    .any(|shared| name.eq_ignore_ascii_case(shared))
}

/// Collapse duplicate identities. Steam wins over desktop/exe/learned.
pub fn normalize_identities(apps: Vec<AppIdentity>) -> Vec<AppIdentity> {
    let mut out: Vec<AppIdentity> = Vec::new();
    for app in apps {
        if let Some(existing) = out.iter_mut().find(|other| same_logical_app(other, &app)) {
            if identity_priority(&app.app_id) > identity_priority(&existing.app_id) {
                let mut merged = app;
                merged.merge_alias(existing.display_name.clone());
                for alias in existing.aliases.drain(..) {
                    merged.merge_alias(alias);
                }
                merge_missing_metadata(&mut merged, existing);
                *existing = merged;
            } else {
                merge_missing_metadata(existing, &app);
                existing.merge_alias(app.display_name);
                for alias in app.aliases {
                    existing.merge_alias(alias);
                }
            }
        } else {
            out.push(app);
        }
    }
    out
}

fn merge_missing_metadata(target: &mut AppIdentity, source: &AppIdentity) {
    if target.desktop_entry_id.is_none() {
        target.desktop_entry_id = source.desktop_entry_id.clone();
    }
    if target.steam_app_id.is_none() {
        target.steam_app_id = source.steam_app_id;
    }
    if target.launcher.is_none() {
        target.launcher = source.launcher;
    }
    if target.icon_path.is_none() {
        target.icon_path = source.icon_path.clone();
    }
    if target.canonical_executable.is_none() {
        target.canonical_executable = source.canonical_executable.clone();
    }
}

fn same_logical_app(a: &AppIdentity, b: &AppIdentity) -> bool {
    if a.app_id == b.app_id {
        return true;
    }
    if let (Some(sa), Some(sb)) = (a.steam_app_id, b.steam_app_id) {
        if sa == sb {
            return true;
        }
    }
    if let (Some(da), Some(db)) = (&a.desktop_entry_id, &b.desktop_entry_id) {
        if normalize_desktop_id(da) == normalize_desktop_id(db) {
            return true;
        }
    }
    names_equivalent(&a.display_name, &b.display_name)
        || a.aliases
            .iter()
            .any(|alias| names_equivalent(alias, &b.display_name))
        || b.aliases
            .iter()
            .any(|alias| names_equivalent(alias, &a.display_name))
}

pub fn names_equivalent(a: &str, b: &str) -> bool {
    normalize_name(a) == normalize_name(b)
}

pub fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace("launcher", "")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

pub fn resolve_identity_for_executable(
    apps: &[AppIdentity],
    executable: &Path,
) -> Option<AppIdentity> {
    let exe_name = executable
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    apps.iter()
        .find(|app| {
            app.canonical_executable
                .as_ref()
                .is_some_and(|p| p == executable)
                || app
                    .canonical_executable
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&exe_name))
        })
        .cloned()
}

pub fn fallback_exe_identity(executable: &Path) -> AppIdentity {
    let name = executable
        .file_stem()
        .or_else(|| executable.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    let fingerprint = simple_fingerprint(executable);
    AppIdentity {
        canonical_executable: Some(executable.to_path_buf()),
        launcher: Some(LauncherKind::Native),
        identity_confidence: 0.6,
        ..AppIdentity::new(AppId::exe(&name, &fingerprint), name)
    }
}

fn simple_fingerprint(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{size:x}-{mtime:x}")
}

pub fn default_search_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config"),
        home.join(".local/share"),
        home.join(".steam"),
        home.join(".local/share/Steam"),
        home.join(".wine"),
        home.join(".local/share/wineprefixes"),
        home.join(".local/share/bottles"),
        home.join("Games"),
        home.join("Downloads"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_launch_metadata_when_higher_priority_identity_wins() {
        let mut desktop = AppIdentity::new(AppId::desktop("spacewar"), "Spacewar");
        desktop.desktop_entry_id = Some("spacewar.desktop".into());
        desktop.canonical_executable = Some(PathBuf::from("/opt/spacewar/game"));
        desktop.icon_path = Some(PathBuf::from("spacewar"));
        desktop.launcher = Some(LauncherKind::Native);

        let mut steam = AppIdentity::new(AppId::steam(480), "Spacewar");
        steam.steam_app_id = Some(480);
        steam.launcher = Some(LauncherKind::Steam);

        let merged = normalize_identities(vec![desktop, steam]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].app_id, AppId::steam(480));
        assert_eq!(
            merged[0].desktop_entry_id.as_deref(),
            Some("spacewar.desktop")
        );
        assert_eq!(merged[0].steam_app_id, Some(480));
        assert_eq!(merged[0].launcher, Some(LauncherKind::Steam));
        assert_eq!(merged[0].icon_path, Some(PathBuf::from("spacewar")));
        assert_eq!(
            merged[0].canonical_executable,
            Some(PathBuf::from("/opt/spacewar/game"))
        );
    }

    #[test]
    fn fills_missing_metadata_on_existing_identity() {
        let existing = AppIdentity::new(AppId::desktop("example"), "Example");
        let mut duplicate = AppIdentity::new(AppId::desktop("example"), "Example App");
        duplicate.desktop_entry_id = Some("example.desktop".into());
        duplicate.steam_app_id = Some(42);
        duplicate.launcher = Some(LauncherKind::Flatpak);
        duplicate.icon_path = Some(PathBuf::from("example-icon"));
        duplicate.canonical_executable = Some(PathBuf::from("/opt/example/app"));

        let merged = normalize_identities(vec![existing, duplicate]);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].desktop_entry_id.as_deref(),
            Some("example.desktop")
        );
        assert_eq!(merged[0].steam_app_id, Some(42));
        assert_eq!(merged[0].launcher, Some(LauncherKind::Flatpak));
        assert_eq!(merged[0].icon_path, Some(PathBuf::from("example-icon")));
        assert_eq!(
            merged[0].canonical_executable,
            Some(PathBuf::from("/opt/example/app"))
        );
    }

    #[test]
    fn merges_minecraft_aliases() {
        let desktop = AppIdentity::new(AppId::desktop("minecraft-launcher"), "Minecraft Launcher");
        let mut learned = AppIdentity::new(AppId::learned(uuid::Uuid::nil()), "minecraft");
        learned.merge_alias(".minecraft");
        let merged = normalize_identities(vec![desktop, learned]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].app_id.as_str().starts_with("desktop:"));
    }

    fn test_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noland-discovery-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    fn native_identity(executable: PathBuf) -> AppIdentity {
        AppIdentity {
            canonical_executable: Some(executable),
            launcher: Some(LauncherKind::Native),
            ..AppIdentity::new(AppId::desktop("owned-root-test"), "Owned Root Test")
        }
    }

    #[test]
    fn derives_narrow_user_owned_install_roots() {
        let home = test_home("owned-roots");
        let installed = home.join(".local/share/Example Game/bin/game");
        let portable = home.join("Downloads/Example.AppImage");
        let project_tool = home.join("Documents/Example Tool/bin/tool");
        let matched_top_level = home.join("Owned Root Test/bin/tool");
        for executable in [&installed, &portable, &project_tool, &matched_top_level] {
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::write(executable, b"app").unwrap();
        }

        assert_eq!(
            derive_owned_install_root(&home, &native_identity(installed)),
            Some(home.join(".local/share/Example Game"))
        );
        assert_eq!(
            derive_owned_install_root(&home, &native_identity(portable.clone())),
            Some(portable)
        );
        assert_eq!(
            derive_owned_install_root(&home, &native_identity(project_tool)),
            Some(home.join("Documents/Example Tool"))
        );
        assert_eq!(
            derive_owned_install_root(&home, &native_identity(matched_top_level)),
            Some(home.join("Owned Root Test"))
        );

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn refuses_shared_or_out_of_home_executable_roots() {
        let home = test_home("unsafe-roots");
        let shared = home.join(".local/share/applications/helper");
        let workspace = home.join("work/unrelated-project/bin/helper");
        std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
        std::fs::create_dir_all(workspace.parent().unwrap()).unwrap();
        std::fs::write(&shared, b"helper").unwrap();
        std::fs::write(&workspace, b"helper").unwrap();

        assert_eq!(
            derive_owned_install_root(&home, &native_identity(shared)),
            None
        );
        assert_eq!(
            derive_owned_install_root(&home, &native_identity(workspace)),
            None
        );
        assert_eq!(
            derive_owned_install_root(&home, &native_identity(PathBuf::from("/usr/bin/env"))),
            None
        );
        assert_eq!(
            derive_owned_install_root(&home, &native_identity(home.clone())),
            None
        );

        #[cfg(unix)]
        {
            let linked = home.join("Applications/Linked/bin/tool");
            std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink("/usr/bin/env", &linked).unwrap();
            assert_eq!(
                derive_owned_install_root(&home, &native_identity(linked)),
                None
            );
        }

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn discovery_emits_prefix_identities_and_specialized_portable_launchers() {
        let home = test_home("identity-kinds");
        let downloads = home.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("Viewer.AppImage"), b"appimage").unwrap();
        std::fs::write(downloads.join("Setup.EXE"), b"exe").unwrap();
        std::fs::create_dir_all(home.join(".wine/drive_c")).unwrap();
        std::fs::create_dir_all(home.join(".local/share/bottles/bottles/arcade")).unwrap();

        let scan = discover_all(&home);
        assert!(scan.apps.iter().any(|app| {
            app.canonical_executable
                .as_ref()
                .is_some_and(|path| path == &downloads.join("Viewer.AppImage"))
                && app.launcher == Some(LauncherKind::AppImage)
        }));
        assert!(scan.apps.iter().any(|app| {
            app.canonical_executable
                .as_ref()
                .is_some_and(|path| path == &downloads.join("Setup.EXE"))
                && app.launcher == Some(LauncherKind::Wine)
        }));
        assert!(scan.apps.iter().any(|app| {
            app.app_id == AppId::launcher("wine", "default")
                && app.launcher == Some(LauncherKind::Wine)
        }));
        assert!(scan.apps.iter().any(|app| {
            app.app_id == AppId::launcher("bottles", "arcade")
                && app.launcher == Some(LauncherKind::Bottles)
        }));

        std::fs::remove_dir_all(home).ok();
    }
}
