use std::path::{Path, PathBuf};

use crate::constants;

#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub state_root: PathBuf,
    pub run_root: PathBuf,
    pub db_path: PathBuf,
    pub staging: PathBuf,
    pub snapshots: PathBuf,
    pub packs: PathBuf,
    pub restore: PathBuf,
    pub checkpoints: PathBuf,
    pub cache: PathBuf,
    pub rpc_socket: PathBuf,
}

impl AgentPaths {
    pub fn production() -> Self {
        Self::from_roots(
            PathBuf::from(constants::STATE_ROOT),
            PathBuf::from(constants::RUN_ROOT),
        )
    }

    pub fn from_roots(state_root: PathBuf, run_root: PathBuf) -> Self {
        Self {
            db_path: state_root.join("state.db"),
            staging: state_root.join("staging"),
            snapshots: state_root.join("snapshots"),
            packs: state_root.join("packs"),
            restore: state_root.join("restore"),
            checkpoints: state_root.join("checkpoints"),
            cache: state_root.join("cache"),
            rpc_socket: run_root.join("state-agent.sock"),
            state_root,
            run_root,
        }
    }

    pub fn ephemeral_storage(&self, operation_id: &str) -> PathBuf {
        self.run_root.join("storage").join(operation_id)
    }

    pub fn restore_dir(&self, restore_id: &str) -> PathBuf {
        self.restore.join(restore_id)
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            &self.state_root,
            &self.run_root,
            &self.staging,
            &self.snapshots,
            &self.packs,
            &self.restore,
            &self.checkpoints,
            &self.cache,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn is_internal(&self, path: &Path) -> bool {
        is_noland_internal(path)
            || path.starts_with(&self.state_root)
            || path.starts_with(&self.run_root)
    }
}

fn is_at_or_below(path: &Path, root: &str) -> bool {
    path.starts_with(Path::new(root))
}

fn normal_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(component) => component.to_str(),
            _ => None,
        })
        .collect()
}

fn has_component_sequence(path: &Path, sequence: &[&str]) -> bool {
    let components = normal_components(path);
    components
        .windows(sequence.len())
        .any(|window| window == sequence)
}

fn is_noland_systemd_unit(path: &Path) -> bool {
    const UNIT_ROOTS: &[&str] = &[
        "/etc/systemd/system",
        "/usr/lib/systemd/system",
        "/usr/local/lib/systemd/system",
        "/lib/systemd/system",
    ];
    const UNIT_SUFFIXES: &[&str] = &[
        ".service", ".socket", ".timer", ".path", ".target", ".mount", ".slice",
    ];

    UNIT_ROOTS.iter().any(|root| {
        path.strip_prefix(root).ok().is_some_and(|relative| {
            normal_components(relative).iter().any(|component| {
                let has_noland_boundary = *component == "noland"
                    || component.starts_with("noland-")
                    || component.starts_with("noland@")
                    || component.starts_with("noland.");
                has_noland_boundary
                    && UNIT_SUFFIXES.iter().any(|suffix| {
                        component.ends_with(suffix) || component.ends_with(&format!("{suffix}.d"))
                    })
            })
        })
    })
}

fn is_standard_user_sunshine(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let components = normal_components(path);
    matches!(
        components.as_slice(),
        ["home", _, ".config", "sunshine", ..]
    ) || matches!(
        components.as_slice(),
        ["var", "home", _, ".config", "sunshine", ..]
    ) || matches!(components.as_slice(), ["root", ".config", "sunshine", ..])
}

/// Paths installed or managed by Noland itself. These are never application state,
/// even when a broad application root happens to contain them.
pub fn is_noland_internal(path: &Path) -> bool {
    is_at_or_below(path, "/var/lib/noland")
        || is_at_or_below(path, "/run/noland")
        || is_at_or_below(path, "/opt/noland")
        || path == Path::new("/usr/local/bin/noland-state-agent")
        || is_at_or_below(path, "/usr/local/lib/noland")
        || is_at_or_below(path, "/etc/noland")
        || is_at_or_below(path, "/etc/sunshine")
        || is_noland_systemd_unit(path)
        || is_standard_user_sunshine(path)
        || has_component_sequence(path, &["noland", "state"])
        || normal_components(path).contains(&".noland")
}

/// Extends static Noland exclusions with the configured target user's Sunshine state.
pub fn is_noland_internal_for_home(path: &Path, target_home: Option<&Path>) -> bool {
    is_noland_internal(path)
        || target_home.is_some_and(|home| path.starts_with(home.join(".config/sunshine")))
}

pub fn is_hard_volatile_root(path: &Path) -> bool {
    [
        "/tmp", "/var/tmp", "/run", "/proc", "/sys", "/dev", "/var/run",
    ]
    .iter()
    .any(|root| is_at_or_below(path, root))
}

/// Base-image namespaces which should not be indexed unless an explicit app root owns the path.
pub fn is_base_system_path(path: &Path) -> bool {
    [
        "/usr",
        "/lib",
        "/bin",
        "/sbin",
        "/boot",
        "/etc",
        "/root",
        "/opt/nvidia",
    ]
    .iter()
    .any(|root| is_at_or_below(path, root))
}

/// Central tracking/index exclusion policy. Known application roots may opt into base-system
/// namespaces, but can never opt into Noland-managed or volatile paths.
pub fn is_tracking_excluded(
    path: &Path,
    in_known_app_root: bool,
    target_home: Option<&Path>,
) -> bool {
    let path_text = path.to_string_lossy();
    let pseudo_path = path_text.trim_start_matches('/').starts_with("anon_inode:")
        || path_text.trim_start_matches('/').starts_with("pipe:")
        || path_text.trim_start_matches('/').starts_with("socket:");
    pseudo_path
        || is_noland_internal_for_home(path, target_home)
        || is_hard_volatile_root(path)
        || (is_base_system_path(path) && !in_known_app_root)
}

pub fn looks_like_lock_or_socket(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some("lock" | "pid" | "sock" | "socket") => true,
        _ => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            name.ends_with(".lock")
                || name.ends_with(".pid")
                || name.starts_with('.') && name.ends_with(".swp")
        }
    }
}

pub fn looks_like_cache(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("/.cache/")
        || text.contains("/cache/")
        || text.contains("/shadercache/")
        || text.contains("/gpu-cache/")
        || text.contains("/code cache/")
        || text.contains("/tmp/")
        || text.ends_with(".tmp")
        || text.ends_with(".log")
}

pub fn looks_like_secret(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("/.ssh/")
        || text.contains("login.keychain")
        || text.contains("cookies")
        || text.contains("secret")
        || text.ends_with(".pem")
        || text.ends_with(".key")
        || text.contains("refresh_token")
        || text.contains("credentials")
}

pub fn looks_like_user_state(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("/saves/")
        || text.contains("/save/")
        || text.contains("/worlds/")
        || text.contains("/saves")
        || text.contains("/mods/")
        || text.contains("/config/")
        || text.contains("/.config/")
        || text.contains("/.local/share/")
        || text.ends_with(".sav")
        || text.ends_with(".save")
        || text.contains("userdata")
}

pub fn looks_like_os_or_lib(path: &Path) -> bool {
    is_base_system_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_system_roots_are_component_aware() {
        for path in [
            "/usr",
            "/usr/lib/libc.so",
            "/lib",
            "/bin/sh",
            "/sbin/init",
            "/boot/vmlinuz",
            "/etc/hosts",
            "/root/.config/app/state",
            "/opt/nvidia/lib/libcuda.so",
        ] {
            assert!(is_base_system_path(Path::new(path)), "{path}");
        }
        for path in [
            "/home/gamer/.config/app/state",
            "/home/gamer/.steam/steam/steamapps/common/Game/content.pak",
            "/home/gamer/.wine/drive_c/game/save.dat",
            "/usr-local/app",
            "/binocular/data",
            "/opt/game",
        ] {
            assert!(!is_base_system_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn noland_managed_paths_are_self_excluded_without_broad_user_exclusions() {
        for path in [
            "/var/lib/noland/state/state.db",
            "/run/noland/state-agent.sock",
            "/opt/noland/state-agent/Cargo.toml",
            "/usr/local/bin/noland-state-agent",
            "/usr/local/lib/noland/noland_observer.bpf.o",
            "/etc/noland/image-id",
            "/etc/systemd/system/noland-state-agent.service",
            "/etc/systemd/system/multi-user.target.wants/noland-state-agent.service",
            "/usr/lib/systemd/system/noland-app@.service.d/override.conf",
            "/etc/sunshine/sunshine.conf",
            "/home/gamer/.config/sunshine/sunshine.conf",
        ] {
            assert!(is_noland_internal(Path::new(path)), "{path}");
        }
        for path in [
            "/usr/local/bin/noland-state-agent-helper",
            "/etc/systemd/system/nolandscape.service",
            "/home/gamer/.config/example/settings.json",
            "/home/gamer/.steam/steam/userdata/1/config/localconfig.vdf",
            "/home/gamer/.wine/drive_c/users/gamer/save.dat",
        ] {
            assert!(!is_noland_internal(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn known_roots_only_override_base_system_exclusion() {
        assert!(is_tracking_excluded(
            Path::new("/usr/lib/libc.so"),
            false,
            None
        ));
        assert!(!is_tracking_excluded(
            Path::new("/usr/local/share/user-app/content.pak"),
            true,
            None
        ));
        assert!(is_tracking_excluded(
            Path::new("/opt/noland/state-agent"),
            true,
            None
        ));
        assert!(is_tracking_excluded(
            Path::new("/run/user-app/socket"),
            true,
            None
        ));
    }

    #[test]
    fn pseudo_files_are_never_tracking_candidates() {
        for path in [
            "anon_inode:[eventpoll]",
            "pipe:[12345]",
            "socket:[67890]",
            "/anon_inode:[eventpoll]",
        ] {
            assert!(is_tracking_excluded(Path::new(path), true, None), "{path}");
        }
    }

    #[test]
    fn configured_home_safely_excludes_sunshine() {
        let home = Path::new("/srv/users/gamer");
        assert!(is_noland_internal_for_home(
            Path::new("/srv/users/gamer/.config/sunshine/sunshine.conf"),
            Some(home)
        ));
        assert!(!is_noland_internal_for_home(
            Path::new("/srv/users/gamer/.config/sunshine-client/settings.json"),
            Some(home)
        ));
    }
}
