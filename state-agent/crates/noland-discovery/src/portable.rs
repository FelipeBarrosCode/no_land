//! Which discovered apps are worth showing or backing up.
//!
//! Image `.desktop` entries (KDE, Bluetooth, system settings, …) are not
//! user state. Stores and game launchers are.

use noland_state_core::{AppIdentity, LauncherKind};

/// Steam, other stores, Wine/Proton frontends, and similar game tooling.
const GAMING_OR_STORE_MARKERS: &[&str] = &[
    "steam",
    "steamos",
    "lutris",
    "heroic",
    "bottles",
    "playonlinux",
    "q4wine",
    "winetricks",
    "proton",
    "wine",
    "legendary",
    "rare",
    "itch",
    "minecraft",
    "prismlauncher",
    "multimc",
    "curseforge",
    "modrinth",
    "gdlauncher",
    "atlauncher",
    "overwolf",
    "battle.net",
    "battlenet",
    "blizzard",
    "epic",
    "gog",
    "galaxy",
    "ubisoft",
    "uplay",
    "origin",
    "eaapp",
    "ea-app",
    "xbox",
    "gamepass",
    "riot",
    "league",
    "valorant",
    "pinokio",
];

/// Noland / OS / streaming plumbing — never a user bundle.
const ALWAYS_IGNORE_MARKERS: &[&str] = &[
    "sunshine",
    "noland",
    "systemd",
    "htop",
    "konsole",
    "kate",
    "kwrite",
    "dolphin",
    "spectacle",
    "systemsettings",
    "nvidia-settings",
];

pub fn is_backup_candidate(app: &AppIdentity) -> bool {
    if is_always_ignored(app) {
        return false;
    }
    if app.steam_app_id.is_some() {
        return true;
    }
    let id = app.app_id.as_str();
    if id.starts_with("steam:")
        || id.starts_with("lutris:")
        || id.starts_with("heroic:")
        || id.starts_with("bottles:")
        || id.starts_with("wine:")
        || id.starts_with("proton:")
    {
        return true;
    }
    match app.launcher {
        Some(
            LauncherKind::Steam
            | LauncherKind::Proton
            | LauncherKind::Wine
            | LauncherKind::Bottles
            | LauncherKind::Heroic
            | LauncherKind::Lutris,
        ) => return true,
        _ => {}
    }
    // Learned from a real process/session — keep. Image .desktop spam is desktop:.
    if id.starts_with("learned:") || id.starts_with("exe:") || id.starts_with("noland:") {
        return true;
    }
    looks_like_gaming_or_store(app)
}

pub fn is_system_desktop_path(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    (text.starts_with("/usr/share/applications")
        || text.starts_with("/usr/local/share/applications")
        || text.contains("/share/applications/"))
        && !text.contains("/.local/share/applications/")
}

fn is_always_ignored(app: &AppIdentity) -> bool {
    let hay = haystack(app);
    ALWAYS_IGNORE_MARKERS.iter().any(|m| hay.contains(m))
}

fn looks_like_gaming_or_store(app: &AppIdentity) -> bool {
    let hay = haystack(app);
    GAMING_OR_STORE_MARKERS.iter().any(|m| hay.contains(m))
}

fn haystack(app: &AppIdentity) -> String {
    let mut parts = vec![
        app.app_id.as_str().to_ascii_lowercase(),
        app.display_name.to_ascii_lowercase(),
    ];
    if let Some(id) = &app.desktop_entry_id {
        parts.push(id.to_ascii_lowercase());
    }
    for alias in &app.aliases {
        parts.push(alias.to_ascii_lowercase());
    }
    parts.join(" ")
}

pub fn filter_backup_candidates(apps: Vec<AppIdentity>) -> Vec<AppIdentity> {
    apps.into_iter().filter(is_backup_candidate).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_state_core::AppId;

    #[test]
    fn keeps_steam_drops_kate() {
        let steam = AppIdentity::new(AppId::steam(480), "Spacewar");
        let mut steam_launcher = AppIdentity::new(AppId::desktop("steam"), "Steam");
        steam_launcher.desktop_entry_id = Some("steam".into());
        steam_launcher.launcher = Some(LauncherKind::Steam);
        let kate = AppIdentity::new(AppId::desktop("org.kde.kate"), "Kate");
        let dolphin = AppIdentity::new(AppId::desktop("org.kde.dolphin"), "Dolphin");
        assert!(is_backup_candidate(&steam));
        assert!(is_backup_candidate(&steam_launcher));
        assert!(!is_backup_candidate(&kate));
        assert!(!is_backup_candidate(&dolphin));
    }
}
