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
    scan.apps.extend(portable_apps::discover_portable_apps(home));
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
                if merged.canonical_executable.is_none() {
                    merged.canonical_executable = existing.canonical_executable.clone();
                }
                *existing = merged;
            } else {
                existing.merge_alias(app.display_name);
                for alias in app.aliases {
                    existing.merge_alias(alias);
                }
                if existing.canonical_executable.is_none() {
                    existing.canonical_executable = app.canonical_executable;
                }
            }
        } else {
            out.push(app);
        }
    }
    out
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
        || a.aliases.iter().any(|alias| names_equivalent(alias, &b.display_name))
        || b.aliases.iter().any(|alias| names_equivalent(alias, &a.display_name))
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
    fn merges_minecraft_aliases() {
        let desktop = AppIdentity::new(AppId::desktop("minecraft-launcher"), "Minecraft Launcher");
        let mut learned = AppIdentity::new(AppId::learned(uuid::Uuid::nil()), "minecraft");
        learned.merge_alias(".minecraft");
        let merged = normalize_identities(vec![desktop, learned]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].app_id.as_str().starts_with("desktop:"));
    }
}
