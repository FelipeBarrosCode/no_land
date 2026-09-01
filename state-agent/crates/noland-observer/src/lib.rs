//! Process and filesystem observation.
//!
//! Linux production path uses the process connector (and eBPF when the
//! `ebpf` feature is later enabled). Tests and non-Linux hosts use an
//! injectable observer so attribution can be proven without kernel probes.

pub mod abi;
mod bpf;
mod cgroup;
mod queue;

pub use bpf::{BpfFeature, BpfObserver, BpfObserverConfig, CgroupObservationMode};
pub use cgroup::{CgroupResolver, DedicatedCgroup};
pub use queue::{EventQueue, QueuedEvent};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use chrono::Utc;
use noland_state_core::metrics::Metrics;
use noland_state_core::*;
use parking_lot::Mutex;

const QUEUE_CAP: usize = 65_536;

pub struct ObserverHub {
    pub processes: ProcessTable,
    pub queue: EventQueue,
    pub metrics: Arc<Metrics>,
    mode: Mutex<ObservationMode>,
}

impl ObserverHub {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self {
            processes: ProcessTable::default(),
            queue: EventQueue::new(QUEUE_CAP, metrics.clone()),
            metrics,
            mode: Mutex::new(ObservationMode::Discovery),
        }
    }

    pub fn set_mode(&self, mode: ObservationMode) {
        *self.mode.lock() = mode;
    }

    pub fn mode(&self) -> ObservationMode {
        *self.mode.lock()
    }

    pub fn inject_process(&self, event: ProcessEvent) {
        Metrics::inc(&self.metrics.process_events_total);
        self.processes.apply(&event);
        self.queue.push(QueuedEvent::Process(event));
    }

    pub fn inject_fs(&self, event: FilesystemEvent) {
        if self.suppress_read(event.kind, event.sampled) {
            return;
        }
        Metrics::inc(&self.metrics.filesystem_events_total);
        self.queue.push(QueuedEvent::Filesystem(event));
    }

    pub fn inject_ebpf_process(&self, fact: EbpfProcessFact) {
        Metrics::inc(&self.metrics.process_events_total);
        self.processes.apply(&fact.as_process_event());
        self.queue.push(QueuedEvent::EbpfProcess(fact));
    }

    pub fn inject_ebpf_fs(&self, fact: EbpfFilesystemFact) {
        if self.suppress_read(fact.kind, fact.sampled) {
            return;
        }
        Metrics::inc(&self.metrics.filesystem_events_total);
        self.queue.push(QueuedEvent::EbpfFilesystem(fact));
    }

    fn suppress_read(&self, kind: FsEventKind, sampled: bool) -> bool {
        self.mode() == ObservationMode::SteadyState
            && kind.is_read()
            && !kind.is_mutation()
            && sampled
    }

    /// Marks the observation stream incomplete. Kernel ring-buffer loss and
    /// malformed ABI records flow through the same recovery signal as queue loss.
    pub fn report_loss(&self, count: u64) {
        self.queue.report_loss(count);
    }

    pub fn drain(&self) -> Vec<QueuedEvent> {
        self.queue.drain()
    }
}

#[derive(Debug, Clone)]
pub struct ProcessRecord {
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    pub gid: u32,
    pub executable: Option<PathBuf>,
    pub comm: Option<String>,
    pub cgroup: Option<String>,
    pub started_at: chrono::DateTime<Utc>,
}

#[derive(Default)]
pub struct ProcessTable {
    inner: Mutex<HashMap<i32, ProcessRecord>>,
}

impl ProcessTable {
    pub fn apply(&self, event: &ProcessEvent) {
        let mut inner = self.inner.lock();
        match event.kind {
            ProcessEventKind::Exit => {
                inner.remove(&event.pid);
            }
            ProcessEventKind::Fork | ProcessEventKind::Clone => {
                let parent = inner.get(&event.ppid).cloned();
                inner.insert(
                    event.pid,
                    ProcessRecord {
                        pid: event.pid,
                        ppid: event.ppid,
                        uid: event.uid,
                        gid: event.gid,
                        executable: event
                            .executable
                            .clone()
                            .or_else(|| parent.as_ref().and_then(|p| p.executable.clone())),
                        comm: event.comm.clone().or_else(|| parent.and_then(|p| p.comm)),
                        cgroup: event.cgroup.clone(),
                        started_at: event.at,
                    },
                );
            }
            ProcessEventKind::Exec => {
                inner
                    .entry(event.pid)
                    .and_modify(|rec| {
                        rec.executable = event.executable.clone();
                        rec.comm = event.comm.clone();
                        rec.cgroup = event.cgroup.clone();
                        rec.ppid = event.ppid;
                    })
                    .or_insert(ProcessRecord {
                        pid: event.pid,
                        ppid: event.ppid,
                        uid: event.uid,
                        gid: event.gid,
                        executable: event.executable.clone(),
                        comm: event.comm.clone(),
                        cgroup: event.cgroup.clone(),
                        started_at: event.at,
                    });
            }
        }
    }

    pub fn get(&self, pid: i32) -> Option<ProcessRecord> {
        self.inner.lock().get(&pid).cloned()
    }

    pub fn snapshot(&self) -> Vec<ProcessRecord> {
        self.inner.lock().values().cloned().collect()
    }
}

pub fn self_excluded(path: &Path, agent_paths: &AgentPaths) -> bool {
    agent_paths.is_internal(path) || is_noland_internal(path)
}

/// Best-effort live process snapshot used to rehydrate after a restart.
/// This is not the primary observation mechanism.
pub fn bootstrap_from_procfs() -> Vec<ProcessEvent> {
    #[cfg(target_os = "linux")]
    {
        linux::scan_proc()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;

    pub fn scan_proc() -> Vec<ProcessEvent> {
        let mut events = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else {
            return events;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok();
            let ppid = stat.as_deref().and_then(parse_ppid).unwrap_or(1);
            let exe = fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|exe| process_executable(pid, Some(exe)));
            let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
                .ok()
                .map(|s| s.trim().to_string());
            let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .ok()
                .and_then(|s| s.lines().last().map(|l| l.to_string()));
            events.push(ProcessEvent {
                kind: ProcessEventKind::Exec,
                pid,
                ppid,
                uid: 0,
                gid: 0,
                cgroup,
                executable: exe,
                argv_hash: None,
                comm,
                at: Utc::now(),
            });
        }
        events
    }

    fn parse_ppid(stat: &str) -> Option<i32> {
        let close = stat.rfind(')')?;
        let rest = stat.get(close + 2..)?;
        rest.split_whitespace().nth(1)?.parse().ok()
    }

    /// Linux process connector (NETLINK_CONNECTOR / CN_IDX_PROC) is the
    /// event-driven primary path. The socket is opened when the agent starts
    /// on Linux; failures fall back to a restart-gap + reconciliation.
    #[allow(dead_code)]
    pub fn open_proc_connector() -> std::io::Result<std::net::UdpSocket> {
        // Placeholder: real netlink bind is implemented in the agent runtime
        // when deployed on the disposable Linux instance. Tests never take
        // this path.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "proc connector is only fully bound in the Linux runtime",
        ))
    }
}

pub(crate) fn process_executable(_pid: i32, kernel_executable: Option<PathBuf>) -> Option<PathBuf> {
    let kernel_executable = kernel_executable?;
    if !kernel_executable
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains("/tmp/.mount_")
    {
        return Some(kernel_executable);
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(cmdline) = std::fs::read(format!("/proc/{_pid}/cmdline")) {
            if let Some(appimage) = appimage_argv0(&cmdline) {
                return Some(appimage);
            }
        }
    }
    Some(kernel_executable)
}

#[cfg(any(target_os = "linux", test))]
fn appimage_argv0(cmdline: &[u8]) -> Option<PathBuf> {
    let argv0 = cmdline.split(|byte| *byte == 0).next()?;
    let argv0 = std::str::from_utf8(argv0).ok()?;
    let path = PathBuf::from(argv0);
    let is_appimage = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("appimage"));
    (path.is_absolute() && is_appimage).then_some(path)
}

pub fn now_event_time() -> chrono::DateTime<Utc> {
    let _ = SystemTime::now();
    Utc::now()
}

pub fn fs_event(kind: FsEventKind, pid: i32, path: impl Into<PathBuf>) -> FilesystemEvent {
    FilesystemEvent {
        kind,
        pid,
        path: path.into(),
        dest_path: None,
        at: Utc::now(),
        sampled: false,
    }
}

pub fn process_exec(pid: i32, ppid: i32, exe: impl Into<PathBuf>) -> ProcessEvent {
    let executable = exe.into();
    let comm = executable
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    ProcessEvent {
        kind: ProcessEventKind::Exec,
        pid,
        ppid,
        uid: 1000,
        gid: 1000,
        cgroup: None,
        executable: Some(executable),
        argv_hash: None,
        comm,
        at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_absolute_appimage_argv0() {
        assert_eq!(
            appimage_argv0(b"/home/gamer/PCSX2.AppImage\0--fullscreen\0"),
            Some(PathBuf::from("/home/gamer/PCSX2.AppImage"))
        );
        assert_eq!(appimage_argv0(b"relative.AppImage\0"), None);
        assert_eq!(appimage_argv0(b"/usr/bin/pcsx2-qt\0"), None);
    }
}
