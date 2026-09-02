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
    #[serde(default, rename = "second_path", alias = "dest_path")]
    pub dest_path: Option<PathBuf>,
    pub at: DateTime<Utc>,
    pub sampled: bool,
}

impl FilesystemEvent {
    pub fn second_path(&self) -> Option<&std::path::Path> {
        self.dest_path.as_deref()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    Ebpf,
    Proc,
    Fanotify,
    Synthetic,
    #[default]
    Unknown,
}

/// Process identity and correlation data emitted by the eBPF observer.
///
/// All non-essential fields default during deserialization so persisted facts
/// produced by older agents remain readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EbpfProcessFact {
    pub kind: ProcessEventKind,
    pub tgid: i32,
    pub tid: i32,
    pub ppid: i32,
    pub uid: u32,
    pub gid: u32,
    pub cgroup_id: u64,
    pub cgroup: Option<String>,
    pub executable: Option<PathBuf>,
    pub argv_hash: Option<String>,
    pub comm: Option<String>,
    pub source: ObservationSource,
    pub sequence: u64,
    pub at: DateTime<Utc>,
}

impl Default for EbpfProcessFact {
    fn default() -> Self {
        Self {
            kind: ProcessEventKind::Exec,
            tgid: 0,
            tid: 0,
            ppid: 0,
            uid: 0,
            gid: 0,
            cgroup_id: 0,
            cgroup: None,
            executable: None,
            argv_hash: None,
            comm: None,
            source: ObservationSource::Unknown,
            sequence: 0,
            at: DateTime::<Utc>::default(),
        }
    }
}

impl EbpfProcessFact {
    pub fn process_id(&self) -> i32 {
        if self.tgid != 0 {
            self.tgid
        } else {
            self.tid
        }
    }

    pub fn as_process_event(&self) -> ProcessEvent {
        ProcessEvent {
            kind: self.kind,
            pid: self.process_id(),
            ppid: self.ppid,
            uid: self.uid,
            gid: self.gid,
            cgroup: self.cgroup.clone(),
            executable: self.executable.clone(),
            argv_hash: self.argv_hash.clone(),
            comm: self.comm.clone(),
            at: self.at,
        }
    }
}

/// Filesystem operation details emitted by the eBPF observer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EbpfFilesystemFact {
    pub kind: FsEventKind,
    pub tgid: i32,
    pub tid: i32,
    pub ppid: i32,
    pub cgroup_id: u64,
    pub path: PathBuf,
    #[serde(alias = "dest_path")]
    pub second_path: Option<PathBuf>,
    pub inode: Option<u64>,
    pub device: Option<u64>,
    pub io_result: Option<i64>,
    pub open_flags: Option<u32>,
    pub mmap_requested_prot: Option<u32>,
    pub mmap_prot: Option<u32>,
    pub mmap_flags: Option<u32>,
    pub source: ObservationSource,
    pub sequence: u64,
    pub accumulated_count: u32,
    pub at: DateTime<Utc>,
    pub sampled: bool,
}

impl Default for EbpfFilesystemFact {
    fn default() -> Self {
        Self {
            kind: FsEventKind::Open,
            tgid: 0,
            tid: 0,
            ppid: 0,
            cgroup_id: 0,
            path: PathBuf::new(),
            second_path: None,
            inode: None,
            device: None,
            io_result: None,
            open_flags: None,
            mmap_requested_prot: None,
            mmap_prot: None,
            mmap_flags: None,
            source: ObservationSource::Unknown,
            sequence: 0,
            accumulated_count: 1,
            at: DateTime::<Utc>::default(),
            sampled: false,
        }
    }
}

impl EbpfFilesystemFact {
    pub fn process_id(&self) -> i32 {
        if self.tgid != 0 {
            self.tgid
        } else {
            self.tid
        }
    }

    pub fn as_filesystem_event(&self) -> FilesystemEvent {
        FilesystemEvent {
            kind: self.kind,
            pid: self.process_id(),
            path: self.path.clone(),
            dest_path: self.second_path.clone(),
            at: self.at,
            sampled: self.sampled,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_path_uses_new_name_and_accepts_legacy_name() {
        let legacy = serde_json::json!({
            "kind": "rename",
            "tgid": 10,
            "dest_path": "/home/user/new-name",
            "path": "/home/user/old-name"
        });
        let fact: EbpfFilesystemFact = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            fact.second_path.as_deref(),
            Some(std::path::Path::new("/home/user/new-name"))
        );

        let serialized = serde_json::to_value(&fact).unwrap();
        assert_eq!(serialized["second_path"], "/home/user/new-name");
        assert!(serialized.get("dest_path").is_none());
    }

    #[test]
    fn richer_facts_default_when_reading_short_payloads() {
        let fact: EbpfFilesystemFact = serde_json::from_value(serde_json::json!({
            "kind": "write",
            "path": "/home/user/save.dat"
        }))
        .unwrap();
        assert_eq!(fact.tgid, 0);
        assert_eq!(fact.cgroup_id, 0);
        assert_eq!(fact.source, ObservationSource::Unknown);
        assert_eq!(fact.inode, None);
    }
}
