use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Stable application identity. Never derived solely from an absolute install path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct AppId(pub String);

impl AppId {
    pub fn steam(app_id: u32) -> Self {
        Self(format!("steam:{app_id}"))
    }

    pub fn launcher(kind: &str, stable_id: &str) -> Self {
        Self(format!("{kind}:{stable_id}"))
    }

    pub fn desktop(normalized_id: &str) -> Self {
        Self(format!("desktop:{}", normalize_desktop_id(normalized_id)))
    }

    pub fn noland(definition_id: &str) -> Self {
        Self(format!("noland:{definition_id}"))
    }

    pub fn exe(normalized_name: &str, fingerprint: &str) -> Self {
        Self(format!(
            "exe:{}:{fingerprint}",
            normalize_desktop_id(normalized_name)
        ))
    }

    pub fn learned(uuid: uuid::Uuid) -> Self {
        Self(format!("learned:{uuid}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Filesystem-safe form for Shared Storage directory names.
    pub fn storage_safe(&self) -> String {
        self.0
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
                _ => '_',
            })
            .collect()
    }
}

impl std::fmt::Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AppId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for AppId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

pub fn normalize_desktop_id(raw: &str) -> String {
    raw.trim()
        .trim_end_matches(".desktop")
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '-',
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherKind {
    Steam,
    Proton,
    Wine,
    Bottles,
    Heroic,
    Lutris,
    Flatpak,
    Snap,
    Native,
    AppImage,
    Custom,
}

impl LauncherKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steam => "steam",
            Self::Proton => "proton",
            Self::Wine => "wine",
            Self::Bottles => "bottles",
            Self::Heroic => "heroic",
            Self::Lutris => "lutris",
            Self::Flatpak => "flatpak",
            Self::Snap => "snap",
            Self::Native => "native",
            Self::AppImage => "appimage",
            Self::Custom => "custom",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "steam" => Self::Steam,
            "proton" => Self::Proton,
            "wine" => Self::Wine,
            "bottles" => Self::Bottles,
            "heroic" => Self::Heroic,
            "lutris" => Self::Lutris,
            "flatpak" => Self::Flatpak,
            "snap" => Self::Snap,
            "native" => Self::Native,
            "appimage" => Self::AppImage,
            "custom" => Self::Custom,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMethod {
    Steam,
    DesktopEntry,
    Launcher,
    Executable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppIdentity {
    pub app_id: AppId,
    pub display_name: String,
    pub canonical_executable: Option<PathBuf>,
    pub desktop_entry_id: Option<String>,
    pub steam_app_id: Option<u32>,
    pub launcher: Option<LauncherKind>,
    pub aliases: Vec<String>,
    pub identity_confidence: f32,
    pub icon_path: Option<PathBuf>,
}

impl AppIdentity {
    pub fn new(app_id: AppId, display_name: impl Into<String>) -> Self {
        Self {
            app_id,
            display_name: display_name.into(),
            canonical_executable: None,
            desktop_entry_id: None,
            steam_app_id: None,
            launcher: None,
            aliases: Vec::new(),
            identity_confidence: 0.75,
            icon_path: None,
        }
    }

    pub fn merge_alias(&mut self, alias: impl Into<String>) {
        let alias = alias.into();
        if !alias.is_empty()
            && alias != self.display_name
            && !self.aliases.iter().any(|existing| existing == &alias)
        {
            self.aliases.push(alias);
        }
    }

    /// Derive how this app should be launched without storing redundant DB state.
    pub fn launch_method(&self) -> Option<LaunchMethod> {
        if self.steam_app_id.is_some()
            || self.launcher == Some(LauncherKind::Steam)
            || self.app_id.as_str().starts_with("steam:")
        {
            return Some(LaunchMethod::Steam);
        }
        if self.desktop_entry_id.is_some() {
            return Some(LaunchMethod::DesktopEntry);
        }
        match self.launcher {
            Some(LauncherKind::Native | LauncherKind::AppImage) => self
                .canonical_executable
                .as_ref()
                .map(|_| LaunchMethod::Executable),
            Some(_) => Some(LaunchMethod::Launcher),
            None => self
                .canonical_executable
                .as_ref()
                .map(|_| LaunchMethod::Executable),
        }
    }
}

/// Priority used to pick a durable app id. Higher is better.
pub fn identity_priority(app_id: &AppId) -> u8 {
    let value = app_id.as_str();
    if value.starts_with("steam:") {
        6
    } else if value.starts_with("noland:") {
        5
    } else if value.starts_with("desktop:") {
        4
    } else if value.starts_with("bottles:")
        || value.starts_with("wine:")
        || value.starts_with("heroic:")
        || value.starts_with("lutris:")
        || value.starts_with("flatpak:")
    {
        3
    } else if value.starts_with("exe:") {
        2
    } else if value.starts_with("learned:") {
        1
    } else {
        0
    }
}

/// Prefer a more stable identity when two discoveries describe the same app.
pub fn prefer_identity(current: &AppId, candidate: &AppId) -> AppId {
    if identity_priority(candidate) > identity_priority(current) {
        candidate.clone()
    } else {
        current.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_ids_are_stable_and_safe() {
        let id = AppId::steam(1234);
        assert_eq!(id.as_str(), "steam:1234");
        assert_eq!(id.storage_safe(), "steam_1234");
        assert!(identity_priority(&id) > identity_priority(&AppId::learned(uuid::Uuid::nil())));
    }

    #[test]
    fn derives_launch_method_from_existing_identity_fields() {
        let steam = AppIdentity::new(AppId::steam(1234), "Steam Game");
        assert_eq!(steam.launch_method(), Some(LaunchMethod::Steam));

        let mut desktop = AppIdentity::new(AppId::desktop("game"), "Desktop Game");
        desktop.desktop_entry_id = Some("game.desktop".into());
        desktop.canonical_executable = Some("/usr/bin/game".into());
        assert_eq!(desktop.launch_method(), Some(LaunchMethod::DesktopEntry));

        let mut executable = AppIdentity::new(AppId::from("exe:game:hash"), "Executable Game");
        executable.canonical_executable = Some("/opt/game/game".into());
        assert_eq!(executable.launch_method(), Some(LaunchMethod::Executable));
    }
}
