//! Application discovery: .desktop, Steam, Proton, Wine, Bottles.

mod desktop;
mod portable;
mod portable_apps;
mod steam;
mod wine;

pub use desktop::{discover_desktop_apps, parse_desktop_entry, DesktopEntry};
pub use portable::{filter_backup_candidates, is_backup_candidate, is_system_desktop_path};
pub use steam::{discover_steam, parse_acf, parse_vdf_map, SteamApp, SteamDiscovery};
pub use wine::{discover_bottles, discover_wine_prefixes, PrefixDiscovery};

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
    if let Some(steam) = discover_steam(home) {
        for app in &steam.apps {
            scan.apps.push(app.to_identity());
        }
        scan.steam = Some(steam);
    }
    scan.wine_prefixes = discover_wine_prefixes(home);
    scan.bottles = discover_bottles(home);
    scan.apps = normalize_identities(scan.apps);
    scan
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
}
