use std::path::{Path, PathBuf};

use noland_state_core::{looks_like_user_state, AppId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDisposition {
    FinalApplicationFile,
    UserStateFile,
    InstallStagingFile,
    RuntimeDependency,
    TemporaryFile,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPolicyDecision {
    pub disposition: PathDisposition,
    pub app_id: Option<AppId>,
    pub transaction_root: Option<PathBuf>,
}

impl PathPolicyDecision {
    fn new(disposition: PathDisposition) -> Self {
        Self {
            disposition,
            app_id: None,
            transaction_root: None,
        }
    }
}

/// Classifies an observed path before attribution or indexing. Launcher-specific staging
/// conventions stay here so the rest of the event pipeline remains launcher agnostic.
pub fn classify_observed_path(path: &Path) -> PathPolicyDecision {
    if let Some((app_id, root)) = steam_staging_path(path) {
        return PathPolicyDecision {
            disposition: PathDisposition::InstallStagingFile,
            app_id: Some(app_id),
            transaction_root: Some(root),
        };
    }

    if is_temporary_path(path) {
        return PathPolicyDecision::new(PathDisposition::TemporaryFile);
    }

    let normalized = normalized_path(path);
    if normalized.contains("/steamapps/compatdata/")
        || normalized.contains("/compatibilitytools.d/")
        || steam_common_runtime_path(&normalized)
    {
        return PathPolicyDecision::new(PathDisposition::RuntimeDependency);
    }
    if normalized.contains("/steamapps/common/") {
        return PathPolicyDecision::new(PathDisposition::FinalApplicationFile);
    }
    if looks_like_user_state(path) {
        return PathPolicyDecision::new(PathDisposition::UserStateFile);
    }

    PathPolicyDecision::new(PathDisposition::Unknown)
}

fn steam_common_runtime_path(normalized: &str) -> bool {
    let Some(relative) = normalized.split("/steamapps/common/").nth(1) else {
        return false;
    };
    let install_dir = relative.split('/').next().unwrap_or_default();
    install_dir == "proton"
        || install_dir.starts_with("proton ")
        || install_dir.starts_with("steamlinuxruntime")
        || install_dir == "steamworks shared"
}

fn steam_staging_path(path: &Path) -> Option<(AppId, PathBuf)> {
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        if !component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("steamapps")
        {
            continue;
        }
        let staging = components.get(index + 1)?;
        if !staging
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("downloading")
        {
            continue;
        }
        let raw_app_id = components.get(index + 2)?.as_os_str().to_string_lossy();
        let app_id = raw_app_id.parse::<u32>().ok()?;
        let mut root = PathBuf::new();
        for part in components.iter().take(index + 3) {
            root.push(part.as_os_str());
        }
        return Some((AppId::steam(app_id), root));
    }
    None
}

fn is_temporary_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    [".tmp", ".part", ".partial", ".crdownload"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn normalized_path(path: &Path) -> String {
    format!(
        "/{}",
        path.to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    )
    .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_download_content_is_staging_with_an_app_identity() {
        let path = Path::new(
            "/home/user/.steam/steam/steamapps/downloading/553850/data/incomplete.stream",
        );
        let decision = classify_observed_path(path);
        assert_eq!(decision.disposition, PathDisposition::InstallStagingFile);
        assert_eq!(decision.app_id, Some(AppId::steam(553850)));
        assert_eq!(
            decision.transaction_root,
            Some(PathBuf::from(
                "/home/user/.steam/steam/steamapps/downloading/553850"
            ))
        );
    }

    #[test]
    fn final_steam_content_is_not_treated_as_staging() {
        let path =
            Path::new("/home/user/.steam/steam/steamapps/common/Helldivers 2/data/game.bundle");
        assert_eq!(
            classify_observed_path(path).disposition,
            PathDisposition::FinalApplicationFile
        );
    }

    #[test]
    fn atomic_temporary_files_are_deferred_until_their_final_rename() {
        let path = Path::new("/home/user/.config/game/settings.json.tmp");
        assert_eq!(
            classify_observed_path(path).disposition,
            PathDisposition::TemporaryFile
        );
    }

    #[test]
    fn proton_install_is_a_runtime_dependency_not_game_content() {
        for path in [
            "/home/user/.steam/steam/steamapps/common/Proton - Experimental/proton",
            "/home/user/.steam/steam/steamapps/common/Proton 9.0/proton",
            "/home/user/.steam/steam/steamapps/common/SteamLinuxRuntime_soldier/run",
            "/home/user/.steam/steam/steamapps/common/SteamLinuxRuntime_sniper/run",
            "/home/user/.steam/steam/steamapps/common/Steamworks Shared/_CommonRedist",
        ] {
            assert_eq!(
                classify_observed_path(Path::new(path)).disposition,
                PathDisposition::RuntimeDependency,
                "{path}"
            );
        }
    }

    #[test]
    fn similarly_named_game_is_still_final_application_content() {
        let path = Path::new("/home/user/.steam/steam/steamapps/common/Protonium/data/game.bundle");
        assert_eq!(
            classify_observed_path(path).disposition,
            PathDisposition::FinalApplicationFile
        );
    }
}
