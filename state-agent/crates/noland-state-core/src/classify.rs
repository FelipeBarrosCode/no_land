use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PersistenceClass {
    BaseImage,
    ReconstructableApp,
    PersistentState,
    Ephemeral,
    SharedState,
    Unknown,
}

impl PersistenceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaseImage => "BASE_IMAGE",
            Self::ReconstructableApp => "RECONSTRUCTABLE_APP",
            Self::PersistentState => "PERSISTENT_STATE",
            Self::Ephemeral => "EPHEMERAL",
            Self::SharedState => "SHARED_STATE",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "BASE_IMAGE" => Self::BaseImage,
            "RECONSTRUCTABLE_APP" => Self::ReconstructableApp,
            "PERSISTENT_STATE" => Self::PersistentState,
            "EPHEMERAL" => Self::Ephemeral,
            "SHARED_STATE" => Self::SharedState,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SemanticRole {
    AppContent,
    UserState,
    SharedRuntime,
    Cache,
    Temp,
    Os,
    Secret,
    Unknown,
}

impl SemanticRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppContent => "APP_CONTENT",
            Self::UserState => "USER_STATE",
            Self::SharedRuntime => "SHARED_RUNTIME",
            Self::Cache => "CACHE",
            Self::Temp => "TEMP",
            Self::Os => "OS",
            Self::Secret => "SECRET",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "APP_CONTENT" => Self::AppContent,
            "USER_STATE" => Self::UserState,
            "SHARED_RUNTIME" => Self::SharedRuntime,
            "CACHE" => Self::Cache,
            "TEMP" => Self::Temp,
            "OS" => Self::Os,
            "SECRET" => Self::Secret,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupMode {
    PersonalState,
    CompleteApplication,
    Custom,
}

impl BackupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PersonalState => "personal_state",
            Self::CompleteApplication => "complete_application",
            Self::Custom => "custom",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "complete_application" | "complete" => Self::CompleteApplication,
            "custom" => Self::Custom,
            _ => Self::PersonalState,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupPerformanceMode {
    Fast,
    #[default]
    Balanced,
    Full,
}

impl BackupPerformanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Full => "full",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "fast" => Self::Fast,
            "full" => Self::Full,
            _ => Self::Balanced,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupDecision {
    Include,
    IncludeSharedReference,
    IncludeAsOverlay,
    IncludeAsBaseOrOverlay,
    MetadataOnly,
    EncryptedInclude,
    Exclude,
    DeferAndReconcile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    PersonalState,
    CompleteApplication,
    Custom,
}

impl RestoreMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PersonalState => "personal_state",
            Self::CompleteApplication => "complete_application",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyKind {
    Snapshot,
    BestEffort,
}

impl ConsistencyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::BestEffort => "best_effort",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "best_effort" => Self::BestEffort,
            _ => Self::Snapshot,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    File,
    Directory,
    Symlink,
    Other,
}

impl FileType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "directory" => Self::Directory,
            "symlink" => Self::Symlink,
            "other" => Self::Other,
            _ => Self::File,
        }
    }
}
