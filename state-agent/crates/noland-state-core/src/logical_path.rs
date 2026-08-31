use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, StateError};
use crate::identity::AppId;

/// Logical roots used in committed manifests. Never commit only raw absolute paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LogicalRoot {
    Home,
    XdgConfigHome,
    XdgDataHome,
    XdgCacheHome,
    Documents,
    Downloads,
    Games,
    SteamRoot,
    SteamLibrary { id: String },
    SteamUserdata,
    ProtonPrefix { steam_app_id: u32 },
    WinePrefix { id: String },
    BottlesPrefix { id: String },
    AppInstallRoot { app_id: AppId },
    NolandCustomRoot { id: String },
}

impl LogicalRoot {
    pub fn as_token(&self) -> String {
        match self {
            Self::Home => "$HOME".into(),
            Self::XdgConfigHome => "$XDG_CONFIG_HOME".into(),
            Self::XdgDataHome => "$XDG_DATA_HOME".into(),
            Self::XdgCacheHome => "$XDG_CACHE_HOME".into(),
            Self::Documents => "$DOCUMENTS".into(),
            Self::Downloads => "$DOWNLOADS".into(),
            Self::Games => "$GAMES".into(),
            Self::SteamRoot => "$STEAM_ROOT".into(),
            Self::SteamLibrary { id } => format!("$STEAM_LIBRARY:{id}"),
            Self::SteamUserdata => "$STEAM_USERDATA".into(),
            Self::ProtonPrefix { steam_app_id } => format!("$PROTON_PREFIX:{steam_app_id}"),
            Self::WinePrefix { id } => format!("$WINE_PREFIX:{id}"),
            Self::BottlesPrefix { id } => format!("$BOTTLES_PREFIX:{id}"),
            Self::AppInstallRoot { app_id } => format!("$APP_INSTALL_ROOT:{}", app_id.as_str()),
            Self::NolandCustomRoot { id } => format!("$NOLAND_CUSTOM_ROOT:{id}"),
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        let token = token.trim();
        if let Some(rest) = token.strip_prefix("$STEAM_LIBRARY:") {
            return Some(Self::SteamLibrary { id: rest.into() });
        }
        if let Some(rest) = token.strip_prefix("$PROTON_PREFIX:") {
            return rest
                .parse()
                .ok()
                .map(|steam_app_id| Self::ProtonPrefix { steam_app_id });
        }
        if let Some(rest) = token.strip_prefix("$WINE_PREFIX:") {
            return Some(Self::WinePrefix { id: rest.into() });
        }
        if let Some(rest) = token.strip_prefix("$BOTTLES_PREFIX:") {
            return Some(Self::BottlesPrefix { id: rest.into() });
        }
        if let Some(rest) = token.strip_prefix("$APP_INSTALL_ROOT:") {
            return Some(Self::AppInstallRoot {
                app_id: AppId(rest.into()),
            });
        }
        if let Some(rest) = token.strip_prefix("$NOLAND_CUSTOM_ROOT:") {
            return Some(Self::NolandCustomRoot { id: rest.into() });
        }
        Some(match token {
            "$HOME" => Self::Home,
            "$XDG_CONFIG_HOME" => Self::XdgConfigHome,
            "$XDG_DATA_HOME" => Self::XdgDataHome,
            "$XDG_CACHE_HOME" => Self::XdgCacheHome,
            "$DOCUMENTS" => Self::Documents,
            "$DOWNLOADS" => Self::Downloads,
            "$GAMES" => Self::Games,
            "$STEAM_ROOT" => Self::SteamRoot,
            "$STEAM_USERDATA" => Self::SteamUserdata,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalPath {
    pub logical_root: LogicalRoot,
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path_hint: Option<String>,
}

impl LogicalPath {
    pub fn new(logical_root: LogicalRoot, relative_path: impl Into<String>) -> Self {
        Self {
            logical_root,
            relative_path: normalize_relative(relative_path.into()),
            source_path_hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.source_path_hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogicalRootMap {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_cache_home: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub downloads: Option<PathBuf>,
    pub games: Option<PathBuf>,
    pub steam_root: Option<PathBuf>,
    pub steam_userdata: Option<PathBuf>,
    pub steam_libraries: BTreeMap<String, PathBuf>,
    pub proton_prefixes: BTreeMap<u32, PathBuf>,
    pub wine_prefixes: BTreeMap<String, PathBuf>,
    pub bottles_prefixes: BTreeMap<String, PathBuf>,
    pub app_install_roots: BTreeMap<String, PathBuf>,
    pub custom_roots: BTreeMap<String, PathBuf>,
}

impl LogicalRootMap {
    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            xdg_config_home: Some(home.join(".config")),
            xdg_data_home: Some(home.join(".local/share")),
            xdg_cache_home: Some(home.join(".cache")),
            documents: Some(home.join("Documents")),
            downloads: Some(home.join("Downloads")),
            games: Some(home.join("Games")),
            steam_root: Some(home.join(".steam/steam")),
            steam_userdata: Some(home.join(".steam/steam/userdata")),
            home: Some(home),
            ..Self::default()
        }
    }

    pub fn resolve(&self, root: &LogicalRoot) -> Option<PathBuf> {
        match root {
            LogicalRoot::Home => self.home.clone(),
            LogicalRoot::XdgConfigHome => self.xdg_config_home.clone(),
            LogicalRoot::XdgDataHome => self.xdg_data_home.clone(),
            LogicalRoot::XdgCacheHome => self.xdg_cache_home.clone(),
            LogicalRoot::Documents => self.documents.clone(),
            LogicalRoot::Downloads => self.downloads.clone(),
            LogicalRoot::Games => self.games.clone(),
            LogicalRoot::SteamRoot => self.steam_root.clone(),
            LogicalRoot::SteamLibrary { id } => self.steam_libraries.get(id).cloned(),
            LogicalRoot::SteamUserdata => self.steam_userdata.clone(),
            LogicalRoot::ProtonPrefix { steam_app_id } => {
                self.proton_prefixes.get(steam_app_id).cloned()
            }
            LogicalRoot::WinePrefix { id } => self.wine_prefixes.get(id).cloned(),
            LogicalRoot::BottlesPrefix { id } => self.bottles_prefixes.get(id).cloned(),
            LogicalRoot::AppInstallRoot { app_id } => {
                self.app_install_roots.get(app_id.as_str()).cloned()
            }
            LogicalRoot::NolandCustomRoot { id } => self.custom_roots.get(id).cloned(),
        }
    }

    pub fn classify(&self, path: &Path) -> Option<LogicalPath> {
        let candidates: Vec<(usize, LogicalRoot, PathBuf)> = [
            self.xdg_config_home
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::XdgConfigHome, p.clone())),
            self.xdg_data_home
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::XdgDataHome, p.clone())),
            self.xdg_cache_home
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::XdgCacheHome, p.clone())),
            self.steam_userdata
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::SteamUserdata, p.clone())),
            self.steam_root
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::SteamRoot, p.clone())),
            self.documents
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::Documents, p.clone())),
            self.downloads
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::Downloads, p.clone())),
            self.games
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::Games, p.clone())),
            self.home
                .as_ref()
                .map(|p| (p.as_os_str().len(), LogicalRoot::Home, p.clone())),
        ]
        .into_iter()
        .flatten()
        .chain(self.steam_libraries.iter().map(|(id, p)| {
            (
                p.as_os_str().len(),
                LogicalRoot::SteamLibrary { id: id.clone() },
                p.clone(),
            )
        }))
        .chain(self.proton_prefixes.iter().map(|(id, p)| {
            (
                p.as_os_str().len(),
                LogicalRoot::ProtonPrefix { steam_app_id: *id },
                p.clone(),
            )
        }))
        .chain(self.wine_prefixes.iter().map(|(id, p)| {
            (
                p.as_os_str().len(),
                LogicalRoot::WinePrefix { id: id.clone() },
                p.clone(),
            )
        }))
        .chain(self.bottles_prefixes.iter().map(|(id, p)| {
            (
                p.as_os_str().len(),
                LogicalRoot::BottlesPrefix { id: id.clone() },
                p.clone(),
            )
        }))
        .chain(self.app_install_roots.iter().map(|(id, p)| {
            (
                p.as_os_str().len(),
                LogicalRoot::AppInstallRoot {
                    app_id: AppId(id.clone()),
                },
                p.clone(),
            )
        }))
        .chain(self.custom_roots.iter().map(|(id, p)| {
            (
                p.as_os_str().len(),
                LogicalRoot::NolandCustomRoot { id: id.clone() },
                p.clone(),
            )
        }))
        .collect();

        let mut best: Option<(usize, LogicalRoot, PathBuf)> = None;
        for (len, root, base) in candidates {
            if path_is_within(path, &base)
                && best
                    .as_ref()
                    .map(|(best_len, _, _)| len > *best_len)
                    .unwrap_or(true)
            {
                best = Some((len, root, base));
            }
        }
        best.map(|(_, root, base)| {
            let rel = path.strip_prefix(&base).unwrap_or(path);
            LogicalPath::new(root, rel.to_string_lossy().as_ref())
                .with_hint(path.display().to_string())
        })
    }
}

pub fn normalize_relative(path: impl AsRef<str>) -> String {
    path.as_ref()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

pub fn path_is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Reject traversal, NUL, and absolute relative paths.
pub fn validate_relative_path(relative: &str) -> Result<()> {
    let normalized = relative.replace('\\', "/");
    if normalized.is_empty() {
        return Err(StateError::UnsafePath("empty relative path".into()));
    }
    if normalized.starts_with('/') || normalized.contains('\0') {
        return Err(StateError::UnsafePath(relative.into()));
    }
    for component in normalized.split('/') {
        if component == ".." {
            return Err(StateError::UnsafePath(format!(
                "path traversal in '{relative}'"
            )));
        }
    }
    Ok(())
}

pub fn join_validated(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative)?;
    let joined = root.join(relative.replace('\\', "/"));
    let canonical_root = root;
    if !joined.starts_with(canonical_root) {
        return Err(StateError::UnsafePath(format!(
            "resolved path {} escapes {}",
            joined.display(),
            root.display()
        )));
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(validate_relative_path("../etc/passwd").is_err());
        assert!(validate_relative_path("ok/../secret").is_err());
        assert!(validate_relative_path("saves/world.dat").is_ok());
    }

    #[test]
    fn classifies_xdg_over_home() {
        let map = LogicalRootMap::from_home("/home/gamer");
        let classified = map
            .classify(Path::new("/home/gamer/.local/share/example-game/save1.db"))
            .unwrap();
        assert_eq!(classified.logical_root.as_token(), "$XDG_DATA_HOME");
        assert_eq!(classified.relative_path, "example-game/save1.db");
    }
}
