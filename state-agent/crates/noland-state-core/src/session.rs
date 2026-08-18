use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::AppId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionSource {
    NolandLaunch,
    DesktopEntry,
    Steam,
    Proton,
    Wine,
    Bottles,
    ExecutableDiscovery,
    InstallerTransaction,
    ManualBinding,
    Learned,
}

impl SessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NolandLaunch => "NOLAND_LAUNCH",
            Self::DesktopEntry => "DESKTOP_ENTRY",
            Self::Steam => "STEAM",
            Self::Proton => "PROTON",
            Self::Wine => "WINE",
            Self::Bottles => "BOTTLES",
            Self::ExecutableDiscovery => "EXECUTABLE_DISCOVERY",
            Self::InstallerTransaction => "INSTALLER_TRANSACTION",
            Self::ManualBinding => "MANUAL_BINDING",
            Self::Learned => "LEARNED",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "NOLAND_LAUNCH" => Self::NolandLaunch,
            "DESKTOP_ENTRY" => Self::DesktopEntry,
            "STEAM" => Self::Steam,
            "PROTON" => Self::Proton,
            "WINE" => Self::Wine,
            "BOTTLES" => Self::Bottles,
            "EXECUTABLE_DISCOVERY" => Self::ExecutableDiscovery,
            "INSTALLER_TRANSACTION" => Self::InstallerTransaction,
            "MANUAL_BINDING" => Self::ManualBinding,
            _ => Self::Learned,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSession {
    pub session_id: Uuid,
    pub app_id: AppId,
    pub root_pid: i32,
    pub cgroup_path: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub source: SessionSource,
    pub identity_confidence: f32,
}

impl AppSession {
    pub fn new(app_id: AppId, root_pid: i32, source: SessionSource) -> Self {
        let session_id = Uuid::new_v4();
        Self {
            cgroup_path: dedicated_cgroup_path(&app_id, session_id),
            session_id,
            app_id,
            root_pid,
            started_at: Utc::now(),
            ended_at: None,
            source,
            identity_confidence: 0.75,
        }
    }

    pub fn is_open(&self) -> bool {
        self.ended_at.is_none()
    }
}

pub fn dedicated_cgroup_path(app_id: &AppId, session_id: Uuid) -> String {
    format!("/noland/apps/{}/{session_id}", app_id.storage_safe())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallTransactionType {
    NolandInitiated,
    LauncherInstall,
    LauncherUpdate,
    ObservedInstaller,
    ConcentratedCreate,
    PrefixCreate,
    ModInstall,
}

impl InstallTransactionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NolandInitiated => "noland_initiated",
            Self::LauncherInstall => "launcher_install",
            Self::LauncherUpdate => "launcher_update",
            Self::ObservedInstaller => "observed_installer",
            Self::ConcentratedCreate => "concentrated_create",
            Self::PrefixCreate => "prefix_create",
            Self::ModInstall => "mod_install",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "noland_initiated" => Self::NolandInitiated,
            "launcher_install" => Self::LauncherInstall,
            "launcher_update" => Self::LauncherUpdate,
            "observed_installer" => Self::ObservedInstaller,
            "prefix_create" => Self::PrefixCreate,
            "mod_install" => Self::ModInstall,
            _ => Self::ConcentratedCreate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallerTransaction {
    pub transaction_id: Uuid,
    pub app_id: AppId,
    pub session_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub candidate_roots: Vec<std::path::PathBuf>,
    pub transaction_type: InstallTransactionType,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyState {
    pub app_id: AppId,
    pub first_dirty_at: DateTime<Utc>,
    pub last_dirty_at: DateTime<Utc>,
    pub dirty_paths: Vec<i64>,
    pub requires_reconciliation: bool,
}
