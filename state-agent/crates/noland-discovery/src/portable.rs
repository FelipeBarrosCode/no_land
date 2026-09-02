//! Which discovered apps are worth showing or backing up.
//!
//! The current product direction is to surface all discovered user software,
//! while still hiding a small set of explicit system/Noland plumbing entries.

use noland_state_core::AppIdentity;

/// Noland / OS / streaming plumbing — never a user bundle.
const ALWAYS_IGNORE_MARKERS: &[&str] = &[
    "sunshine",
    "noland",
    "systemd",
    "startplasma",
    "kdeinit",
    "kioslave",
    "xsettingsd",
    "dbus-run-session",
    "xdg-desktop-portal",
    "xdg-document-portal",
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
    !is_always_ignored(app)
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
    fn keeps_discovered_apps_but_still_drops_explicitly_ignored_tools() {
        let steam = AppIdentity::new(AppId::steam(480), "Spacewar");
        let pcsx2 = AppIdentity::new(AppId::desktop("net.pcsx2.PCSX2"), "PCSX2");
        let vice_city = AppIdentity::new(AppId::desktop("vice-city"), "Vice City");
        let kate = AppIdentity::new(AppId::desktop("org.kde.kate"), "Kate");
        let dolphin = AppIdentity::new(AppId::desktop("org.kde.dolphin"), "Dolphin");
        let plasma = AppIdentity::new(AppId("exe:startplasma-x11:abc".into()), "startplasma-x11");
        let portal = AppIdentity::new(
            AppId("exe:xdg-desktop-portal:abc".into()),
            "xdg-desktop-portal",
        );
        assert!(is_backup_candidate(&steam));
        assert!(is_backup_candidate(&pcsx2));
        assert!(is_backup_candidate(&vice_city));
        assert!(!is_backup_candidate(&kate));
        assert!(!is_backup_candidate(&dolphin));
        assert!(!is_backup_candidate(&plasma));
        assert!(!is_backup_candidate(&portal));
    }
}
