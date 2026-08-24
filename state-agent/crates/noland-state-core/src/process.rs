use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEventKind {
    Exec,
    Fork,
    Clone,
    Exit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEvent {
    pub kind: ProcessEventKind,
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    pub gid: u32,
    pub cgroup: Option<String>,
    pub executable: Option<PathBuf>,
    pub argv_hash: Option<String>,
    pub comm: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsEventKind {
    Open,
    Read,
    Write,
    Create,
    Truncate,
    Rename,
    Unlink,
    Mkdir,
    Rmdir,
    Chmod,
    Chown,
    Symlink,
    Mmap,
    Execve,
}

impl FsEventKind {
    pub fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::Write
                | Self::Create
                | Self::Truncate
                | Self::Rename
                | Self::Unlink
                | Self::Mkdir
                | Self::Rmdir
                | Self::Chmod
                | Self::Chown
                | Self::Symlink
        )
    }

    pub fn is_read(self) -> bool {
        matches!(self, Self::Open | Self::Read | Self::Mmap)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemEvent {
    pub kind: FsEventKind,
    pub pid: i32,
    pub path: PathBuf,
    pub dest_path: Option<PathBuf>,
    pub at: DateTime<Utc>,
    pub sampled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationMode {
    Discovery,
    SteadyState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GraphRelation {
    AppLaunchesProcess,
    ProcessReadsPath,
    ProcessWritesPath,
    AppOwnsPath,
    AppUsesPath,
    AppDependsOnRuntime,
    AppUsesPrefix,
    AppSharesPath,
    BundleContainsPath,
    PathMaterializesFromObject,
    AppReconstructableBy,
}

impl GraphRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppLaunchesProcess => "APP_LAUNCHES_PROCESS",
            Self::ProcessReadsPath => "PROCESS_READS_PATH",
            Self::ProcessWritesPath => "PROCESS_WRITES_PATH",
            Self::AppOwnsPath => "APP_OWNS_PATH",
            Self::AppUsesPath => "APP_USES_PATH",
            Self::AppDependsOnRuntime => "APP_DEPENDS_ON_RUNTIME",
            Self::AppUsesPrefix => "APP_USES_PREFIX",
            Self::AppSharesPath => "APP_SHARES_PATH",
            Self::BundleContainsPath => "BUNDLE_CONTAINS_PATH",
            Self::PathMaterializesFromObject => "PATH_MATERIALIZES_FROM_OBJECT",
            Self::AppReconstructableBy => "APP_RECONSTRUCTABLE_BY",
        }
    }
}
