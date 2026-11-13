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
    looks_like_cache_tree(path)
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
        || looks_like_diagnostic_log(path)
}

/// Inherited cache exclusions, safe to prune before metadata/read_dir. Terminal
/// filename heuristics (.log, .tmp) do not imply that a directory's contents are
/// disposable and belong in `looks_like_cache` only.
pub fn looks_like_cache_tree(path: &Path) -> bool {
    // Match the directory itself as well as its children, so walkers can prune it
    // before opening it. Component boundaries avoid excluding e.g. cache-assets.
    normal_components(path).iter().any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            ".cache" | "cache" | "shadercache" | "gpu-cache" | "code cache" | "tmp"
        )
    })
}

/// Lexically safe, lossless index keys. Never normalize `..` across symlinks or
/// turn an unrepresentable filename into another file's UTF-8 database key.
pub fn is_valid_index_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|text| {
            !text.contains('\0') && !text.split('/').any(|part| matches!(part, "." | ".."))
        })
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

/// Opt-in warm-cache candidates, deliberately separate from `looks_like_cache`.
/// Only audited Hookshot top-level filenames are regenerable; arbitrary .bin
/// files and user/romfs trees are never candidates. Complete backups retain
/// these unless policy explicitly opts out.
pub fn is_regenerable_game_cache(path: &Path) -> bool {
    if !is_valid_index_path(path)
        || !path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("Hookshot"))
    {
        return false;
    }
    if normal_components(path).iter().any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "save"
                | "saves"
                | "saved games"
                | "config"
                | ".config"
                | "configs"
                | "mods"
                | "romfs"
                | "userdata"
        )
    }) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        "hookshot_programs.bin"
            | "hookshot_shaders.bin"
            | "hookshot_pipelines.bin"
            | "mkw_vk_pipelines.bin"
    ) || name
        .strip_prefix("mkw_vk_pipelines_play_")
        .and_then(|rest| rest.strip_suffix(".bin"))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Diagnostic output is regenerable, even when log rotation changes its suffix.
/// Do not exclude whole `logs` directories: they can also contain real app data.
pub fn looks_like_diagnostic_log(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix(".gz").unwrap_or(&name);
    if name.ends_with(".log") {
        return true;
    }
    if let Some((_, rotation)) = name.rsplit_once(".log.") {
        if !rotation.is_empty()
            && rotation
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == '_' || c == '.')
        {
            return true;
        }
    }
    let in_logs_dir = path.parent().is_some_and(|parent| {
        parent.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|part| part.eq_ignore_ascii_case("logs"))
        })
    });
    in_logs_dir
        && (name == "catalina.out"
            || name == "access_log"
            || name == "error_log"
            || ["localhost_access_log.", "access_log.", "error_log."]
                .iter()
                .any(|prefix| {
                    name.strip_prefix(prefix)
                        .and_then(|rest| rest.strip_suffix(".txt"))
                        .is_some_and(|date| {
                            date.len() == 10
                                && date.bytes().enumerate().all(|(i, c)| {
                                    if i == 4 || i == 7 {
                                        c == b'-'
                                    } else {
                                        c.is_ascii_digit()
                                    }
                                })
                        })
                }))
}

pub fn looks_like_secret(path: &Path) -> bool {
    normal_components(path).iter().any(|part| {
        let name = part.to_ascii_lowercase();
        if matches!(name.as_str(), ".ssh" | ".gnupg" | ".env")
            || name.starts_with(".env.")
            || [".pem", ".key", ".p12", ".pfx"]
                .iter()
                .any(|ext| name.ends_with(ext))
        {
            return true;
        }
        let named_credential = [
            "secret",
            "secrets",
            "credentials",
            "cookies",
            "login.keychain",
            "refresh_token",
            "access_token",
            "client_secret",
            "id_rsa",
            "id_ed25519",
            "id_ecdsa",
            "id_dsa",
        ]
        .iter()
        .any(|stem| {
            name == *stem
                || name.strip_prefix(stem).is_some_and(|suffix| {
                    suffix.starts_with('.')
                        || suffix == "-wal"
                        || suffix == "-shm"
                        || suffix == "-journal"
                        || (*stem == "login.keychain" && suffix == "-db")
                })
        });
        let stem = name
            .rsplit_once('.')
            .filter(|(_, extension)| {
                matches!(
                    *extension,
                    "json"
                        | "toml"
                        | "yaml"
                        | "yml"
                        | "ini"
                        | "txt"
                        | "db"
                        | "sqlite"
                        | "sqlite3"
                        | "env"
                )
            })
            .map_or(name.as_str(), |(stem, _)| stem);
        named_credential
            || [
                "secret",
                "secrets",
                "credentials",
                "refresh_token",
                "access_token",
                "client_secret",
            ]
            .iter()
            .any(|suffix| {
                stem.strip_suffix(suffix).is_some_and(|prefix| {
                    prefix.ends_with('-') || prefix.ends_with('_') || prefix.ends_with('.')
                })
            })
    })
}

pub fn looks_like_user_state(path: &Path) -> bool {
    let components = normal_components(path);
    // Runtime app-data and Steam's XDG library may contain entire installations.
    // Their placement beneath .local/share says nothing about asset mutability.
    for (i, parts) in components.windows(2).enumerate() {
        if parts == ["noland-runtime", "app-data"]
            && components
                .get(i + 3)
                .is_some_and(|part| part.eq_ignore_ascii_case("Games"))
            && components.get(i + 4).is_some()
        {
            return install_user_state_components(&components[i + 5..]);
        }
        if parts[0].eq_ignore_ascii_case("steamapps")
            && parts[1].eq_ignore_ascii_case("common")
            && components.get(i + 2).is_some()
        {
            return install_user_state_components(&components[i + 3..]);
        }
    }
    user_state_components(&components)
        || components
            .windows(2)
            .any(|parts| parts == [".local", "share"])
}

/// State signals relative to a known install root; XDG ancestors and the game
/// directory's name must not turn every asset into persistent user state.
pub fn looks_like_user_state_in_install(path: &Path, install_root: &Path) -> bool {
    path.strip_prefix(install_root)
        .ok()
        .is_some_and(|relative| install_user_state_components(&normal_components(relative)))
}

fn install_user_state_components(components: &[&str]) -> bool {
    user_state_components(components)
        || components
            .iter()
            .any(|part| part.eq_ignore_ascii_case("user"))
}

fn user_state_components(components: &[&str]) -> bool {
    components.iter().any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "save"
                | "saves"
                | "saved games"
                | "savestates"
                | "states"
                | "memcards"
                | "worlds"
                | "mods"
                | "config"
                | ".config"
                | "configs"
                | "settings"
                | "preferences"
                | "userdata"
                | "profiles"
        )
    }) || components.last().is_some_and(|name| {
        let name = name.to_ascii_lowercase();
        [".sav", ".save", ".ini", ".cfg"]
            .iter()
            .any(|ext| name.ends_with(ext))
            || matches!(
                name.as_str(),
                "save.dat" | "settings.json" | "config.json" | "settings.toml" | "config.toml"
            )
    })
}

pub fn looks_like_os_or_lib(path: &Path) -> bool {
    is_base_system_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_assets_are_not_xdg_state_but_saves_and_settings_are() {
        let root = Path::new("/home/user/.local/share/noland-runtime/app-data/123/Games/Hookshot");
        for relative in [
            "romfs/a/b/c/d/e/f/g/assets.bin",
            "romfs/Model/Rock_LonLon_SecretRoomArch_A_Alb.bntx.zs",
            "bin/hookshot",
            "saves-thumbnail.png",
            "hookshot_programs.bin",
        ] {
            let path = root.join(relative);
            assert!(!looks_like_user_state(&path), "{}", path.display());
            assert!(
                !looks_like_user_state_in_install(&path, root),
                "{}",
                path.display()
            );
        }
        for relative in [
            "user/save/slot_00/progress.sav",
            "user/profile.dat",
            "saves/slot.bin",
            "mods/texture.bin",
            "config/settings.json",
            "hookshot_options.ini",
        ] {
            let path = root.join(relative);
            assert!(looks_like_user_state(&path), "{}", path.display());
            assert!(
                looks_like_user_state_in_install(&path, root),
                "{}",
                path.display()
            );
        }
        assert!(looks_like_user_state(Path::new(
            "/home/user/.local/share/other-app/data.db"
        )));
        assert!(!looks_like_user_state(Path::new(
            "/home/user/Games/Other/assets.bin"
        )));
        assert!(!looks_like_user_state(Path::new(
            "/home/user/.local/share/Steam/steamapps/common/Game/assets.bin"
        )));
    }

    #[test]
    fn secret_detection_uses_credential_names_not_asset_substrings() {
        for path in [
            "/home/user/Game/romfs/Rock_LonLon_SecretRoomArch_A_Alb.bntx.zs",
            "/home/user/Game/secretary.png",
            "/home/user/Game/cookies_texture.bin",
            "/home/user/Game/credentialsmith.dat",
        ] {
            assert!(!looks_like_secret(Path::new(path)), "{path}");
        }
        for path in [
            "/home/user/.ssh",
            "/home/user/.ssh/id_rsa",
            "/home/user/.gnupg/private-keys-v1.d/key",
            "/home/user/Game/secrets.json",
            "/home/user/Game/credentials.toml",
            "/home/user/Game/service-credentials.json",
            "/home/user/Game/refresh_token",
            "/home/user/Game/client_secret.json",
            "/home/user/Game/cookies.sqlite",
            "/home/user/Game/.env.production",
            "/home/user/Game/private.key",
            "/home/user/Game/private.pem",
            "/home/user/Library/Keychains/login.keychain-db",
        ] {
            assert!(looks_like_secret(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn hookshot_cache_candidates_are_exact_top_level_files_and_opt_in() {
        let root = Path::new("/home/user/.local/share/noland-runtime/app-data/123/Games/Hookshot");
        for name in [
            "hookshot_programs.bin",
            "hookshot_shaders.bin",
            "hookshot_pipelines.bin",
            "mkw_vk_pipelines.bin",
            "mkw_vk_pipelines_play_123.bin",
        ] {
            let path = root.join(name);
            assert!(is_regenerable_game_cache(&path), "{name}");
            assert!(
                !looks_like_cache(&path),
                "default backup exclusion changed for {name}"
            );
            for subdir in ["user/save", "config", "mods", "romfs"] {
                assert!(!is_regenerable_game_cache(&root.join(subdir).join(name)));
                assert!(!is_regenerable_game_cache(
                    &root.join(subdir).join("Hookshot").join(name)
                ));
            }
            assert!(!is_regenerable_game_cache(
                &root.with_file_name("OtherGame").join(name)
            ));
        }
        for name in [
            "data.bin",
            "shader_cache.bin",
            "mkw_vk_pipelines_play_.bin",
            "mkw_vk_pipelines_play_user.bin",
            "mkw_vk_pipelines_play_1.bin.bak",
        ] {
            assert!(!is_regenerable_game_cache(&root.join(name)), "{name}");
        }
        assert!(!is_regenerable_game_cache(
            &root.join("../OtherGame/hookshot_programs.bin")
        ));
    }

    #[test]
    fn cache_directory_itself_can_be_pruned_without_matching_similar_names() {
        for path in [
            "/home/user/.cache",
            "/home/user/Game/cache",
            "/home/user/Game/shadercache",
        ] {
            assert!(looks_like_cache(Path::new(path)));
        }
        assert!(!looks_like_cache(Path::new(
            "/home/user/Game/cache-assets/data.bin"
        )));
    }

    #[test]
    fn index_keys_must_be_absolute_lossless_and_without_parent_traversal() {
        assert!(is_valid_index_path(Path::new("/home/user/Game/romfs/file")));
        for path in [
            "relative/file",
            "/home/user/Game/../Other/file",
            "/home/user/Game/./file",
            "/home/user/Game/\0file",
        ] {
            assert!(!is_valid_index_path(Path::new(path)));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(!is_valid_index_path(Path::new(
                std::ffi::OsStr::from_bytes(b"/home/user/\xff")
            )));
        }
    }

    #[test]
    fn diagnostic_logs_include_rotated_and_dated_server_logs_but_not_save_data() {
        for path in [
            "/opt/apache-tomcat-9.0.112/logs/localhost_access_log.2026-11-09.txt",
            "/opt/tomcat/logs/catalina.out",
            "/opt/server/logs/access_log.2026-11-09.txt.gz",
            "/home/gamer/.local/share/game/player.log.1",
            "/home/gamer/.local/share/game/player.log.2026-11-09.gz",
            "/home/gamer/game/PLAYER.LOG",
        ] {
            assert!(looks_like_cache(Path::new(path)), "{path}");
        }
        for path in [
            "/home/gamer/game/logs/progress.sav",
            "/home/gamer/game/logs/settings.txt",
            "/home/gamer/game/logs/world.log.dat",
            "/home/gamer/game/user/save/slot_00/progress.sav",
            "/home/gamer/game/hookshot_options.ini",
            "/home/gamer/game/localhost_access_log.2026-11-09.txt",
        ] {
            assert!(!looks_like_cache(Path::new(path)), "{path}");
        }
    }

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
