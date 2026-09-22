//! Which discovered apps are worth showing or backing up.
//!
//! The current product direction is to surface all discovered user software
//! while hiding explicit system/Noland plumbing entries and process-derived
//! identities whose executables belong to the operating system.

use noland_state_core::AppIdentity;

/// Noland / OS / streaming plumbing — never a user bundle.
const ALWAYS_IGNORE_MARKERS: &[&str] = &[
    "sunshine",
    "noland",
    "systemd",
    "startplasma",
    "at-spi",
    "plasmashell",
    "plasma_session",
    "kdeinit",
    "kioslave",
    "xsettingsd",
    "dbus-run-session",
    "geoclue",
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

const ALWAYS_IGNORE_EXECUTABLES: &[&str] = &[
    "accounts-daemon",
    "agetty",
    "avahi-daemon",
    // Interactive shells are never applications to back up; their fallback
    // executable identities would otherwise sweep the entire home directory.
    "bash",
    "csh",
    "dash",
    "fish",
    "ksh",
    "sh",
    "tcsh",
    "zsh",
    "cron",
    "dbus-broker",
    "dbus-daemon",
    // Instance infrastructure, not user software.
    "caddy",
    "guacd",
    "vast-caddy",
    "kdeinit",
    "kioslave",
    "networkmanager",
    "noland-lifecycle-agent",
    "noland-state-agent",
    "packagekitd",
    "pipewire",
    "polkitd",
    "pulseaudio",
    "rtkit-daemon",
    "sshd",
    "startplasma",
    "sunshine",
    "systemd",
    "udisksd",
    "upowerd",
    "wireplumber",
    "xorg",
    "xsettingsd",
];

const ALWAYS_IGNORE_EXECUTABLE_PREFIXES: &[&str] = &["xdg-desktop-portal", "systemd-"];

/// Executable locations that belong to the operating system, not to software a
/// user installed. Process-derived identities whose executables live here must
/// never become shared-storage backup candidates.
const SYSTEM_EXECUTABLE_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/usr/bin",
    "/usr/sbin",
    "/usr/lib",
    "/usr/lib32",
    "/usr/lib64",
    "/usr/libexec",
];

pub fn is_backup_candidate(app: &AppIdentity) -> bool {
    // Process-derived fallback identities (exe:*) are only interesting when
    // they point at software the user installed. System processes must never
    // enter the ranker or sweep other applications' state.
    if is_process_derived_identity(app) && !is_user_software_executable(app) {
        return false;
    }
    !is_always_ignored(app) && !is_steam_runtime(app)
}

fn is_process_derived_identity(app: &AppIdentity) -> bool {
    app.app_id.as_str().starts_with("exe:")
}

fn is_user_software_executable(app: &AppIdentity) -> bool {
    let Some(path) = app.canonical_executable.as_deref() else {
        return false;
    };
    if is_system_executable_path(path) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let normalized = name.to_ascii_lowercase();
    // Chromium-family helper processes share the browser's install directory
    // but are not software the user launches.
    if normalized.contains("crashpad") {
        return false;
    }
    true
}

/// Returns whether an executable path belongs to the operating system rather
/// than user-installed software. `/usr/local` and `/opt` are deliberately not
/// system locations: software users install often lands there.
pub fn is_system_executable_path(path: &std::path::Path) -> bool {
    let candidate = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let candidate = candidate.to_string_lossy();
    SYSTEM_EXECUTABLE_ROOTS
        .iter()
        .any(|root| candidate.as_ref() == *root || candidate.starts_with(&format!("{root}/")))
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
        || std::iter::once(app.display_name.as_str())
            .chain(app.aliases.iter().map(String::as_str))
            .any(is_always_ignored_executable_name)
        || app
            .canonical_executable
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(is_always_ignored_executable_name)
}

/// Returns whether an executable belongs to OS/Noland plumbing that must never
/// become a backup candidate. Callers that only have a process executable use
/// this entry point so they share the same policy as application discovery.
pub fn is_always_ignored_executable_name(name: &str) -> bool {
    let normalized = name
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    ALWAYS_IGNORE_EXECUTABLES.contains(&normalized.as_str())
        || ALWAYS_IGNORE_EXECUTABLE_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
        || ALWAYS_IGNORE_MARKERS
            .iter()
            .any(|marker| normalized.contains(marker))
}

fn is_steam_runtime(app: &AppIdentity) -> bool {
    if app.steam_app_id.is_none() && !app.app_id.as_str().starts_with("steam:") {
        return false;
    }
    std::iter::once(app.display_name.as_str())
        .chain(app.aliases.iter().map(String::as_str))
        .any(|name| {
            let name = name.trim().to_ascii_lowercase();
            name == "proton"
                || name.starts_with("proton ")
                || name.starts_with("steam linux runtime")
                || name.starts_with("steamlinuxruntime")
                || name == "steamworks common redistributables"
                || name == "steamworks shared"
        })
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
        let plasma_shell = AppIdentity::new(AppId("exe:plasmashell:abc".into()), "plasmashell");
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
        assert!(!is_backup_candidate(&plasma_shell));
        assert!(!is_backup_candidate(&portal));
    }

    #[test]
    fn ignores_plasma_executable_variants() {
        for executable in [
            "startplasma",
            "startplasma-x11",
            "startplasma-wayland",
            "plasmashell",
            "plasma_session",
        ] {
            assert!(
                is_always_ignored_executable_name(executable),
                "{executable}"
            );
        }
        assert!(is_always_ignored_executable_name("systemd-journald"));
        assert!(is_always_ignored_executable_name("dbus-daemon"));
        assert!(is_always_ignored_executable_name("agetty"));
        assert!(is_always_ignored_executable_name("avahi-daemon"));
        for shell in ["bash", "sh", "dash", "zsh", "fish", "csh", "ksh", "tcsh"] {
            assert!(is_always_ignored_executable_name(shell), "{shell}");
        }
        assert!(!is_always_ignored_executable_name("necronator-game"));
    }

    #[test]
    fn ignores_system_helpers_identified_by_their_full_executable_path() {
        let mut geoclue = AppIdentity::new(AppId("exe:agent:test".into()), "agent");
        geoclue.canonical_executable = Some("/usr/libexec/geoclue-2.0/demos/agent".into());
        let at_spi = AppIdentity::new(
            AppId("exe:at-spi-bus-launcher:test".into()),
            "at-spi-bus-launcher",
        );

        assert!(!is_backup_candidate(&geoclue));
        assert!(!is_backup_candidate(&at_spi));
    }

    #[test]
    fn system_processes_are_never_backup_candidates_but_user_software_is() {
        // Operating-system executable locations are excluded regardless of the
        // explicit name blocklist.
        for system_exe in [
            "/usr/bin/bash",
            "/usr/sbin/agetty",
            "/usr/libexec/geoclue-2.0/demos/agent",
            "/bin/sh",
        ] {
            let mut app = AppIdentity::new(AppId("exe:system:test".into()), "system");
            app.canonical_executable = Some(system_exe.into());
            assert!(!is_backup_candidate(&app), "{system_exe}");
        }

        // A process-derived identity without any executable cannot be user software.
        let anonymous = AppIdentity::new(AppId("exe:mystery:test".into()), "mystery");
        assert!(!is_backup_candidate(&anonymous));

        // Helper processes that share a user app's install directory are not
        // software the user launches.
        let mut crashpad =
            AppIdentity::new(AppId("exe:chrome_crashpad_handler:test".into()), "crashpad");
        crashpad.canonical_executable = Some("/opt/google/chrome/chrome_crashpad_handler".into());
        assert!(!is_backup_candidate(&crashpad));

        // Instance infrastructure is excluded by name.
        let mut guacd = AppIdentity::new(AppId("exe:guacd:test".into()), "guacd");
        guacd.canonical_executable = Some("/usr/local/bin/guacd".into());
        assert!(!is_backup_candidate(&guacd));

        // Software users actually install lives outside the system roots.
        for user_exe in [
            "/home/user/bin/my-game",
            "/opt/games/my-game",
            "/usr/local/bin/my-game",
            "/opt/google/chrome/chrome",
        ] {
            let mut app = AppIdentity::new(AppId("exe:user:test".into()), "my-game");
            app.canonical_executable = Some(user_exe.into());
            assert!(is_backup_candidate(&app), "{user_exe}");
        }

        // The rule only constrains process-derived identities: desktop entries
        // and Steam apps keep their existing classification.
        let desktop = AppIdentity::new(AppId::desktop("example-game"), "Example Game");
        assert!(is_backup_candidate(&desktop));
        let steam = AppIdentity::new(AppId::steam(480), "Spacewar");
        assert!(is_backup_candidate(&steam));
    }

    #[test]
    fn hides_steam_runtimes_without_changing_non_steam_candidates() {
        for (app_id, name) in [
            (AppId::steam(1493710), "Proton Experimental"),
            (AppId::steam(1628350), "Steam Linux Runtime 3.0 (sniper)"),
            (AppId::steam(228980), "Steamworks Common Redistributables"),
        ] {
            assert!(
                !is_backup_candidate(&AppIdentity::new(app_id, name)),
                "{name}"
            );
        }

        let non_steam = AppIdentity::new(
            AppId::desktop("proton-game-launcher"),
            "Proton Game Launcher",
        );
        let similarly_named_steam_game = AppIdentity::new(AppId::steam(42), "Protonium: The Game");
        assert!(is_backup_candidate(&non_steam));
        assert!(is_backup_candidate(&similarly_named_steam_game));
    }
}
