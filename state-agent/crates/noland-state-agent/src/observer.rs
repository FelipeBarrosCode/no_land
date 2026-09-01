use std::sync::Arc;

use noland_observer::{BpfObserver, BpfObserverConfig, ObserverHub};
use parking_lot::Mutex;
use serde::Serialize;

const MIN_CAPABILITY_KERNEL: (u32, u32) = (5, 8);
const CAP_PERFMON: u32 = 38;
const CAP_BPF: u32 = 39;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObserverCapabilityState {
    Starting,
    Active,
    Unsupported,
    MissingCapability,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObserverCapabilityStatus {
    pub state: ObserverCapabilityState,
    pub backend: &'static str,
    pub kernel_release: Option<String>,
    pub required_capabilities: [&'static str; 2],
    pub reason: Option<String>,
    pub queue_events_dropped: u64,
    pub loss_events: u64,
    pub reconciliation_required: bool,
}

impl ObserverCapabilityStatus {
    fn starting() -> Self {
        Self {
            state: ObserverCapabilityState::Starting,
            backend: "libbpf-ring-buffer",
            kernel_release: kernel_release(),
            required_capabilities: ["CAP_BPF", "CAP_PERFMON"],
            reason: None,
            queue_events_dropped: 0,
            loss_events: 0,
            reconciliation_required: false,
        }
    }
}

pub struct ObserverSupervisor {
    status: Mutex<ObserverCapabilityStatus>,
    observer: Mutex<Option<BpfObserver>>,
}

impl ObserverSupervisor {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(ObserverCapabilityStatus::starting()),
            observer: Mutex::new(None),
        }
    }

    pub fn start(&self, hub: Arc<ObserverHub>) {
        if !cfg!(target_os = "linux") {
            self.set_unavailable(
                ObserverCapabilityState::Unsupported,
                "eBPF observation is supported only on Linux".into(),
            );
            return;
        }

        let release = kernel_release();
        let Some(version) = release.as_deref().and_then(parse_kernel_version) else {
            self.set_unavailable(
                ObserverCapabilityState::Unsupported,
                format!("cannot determine Linux kernel version from {release:?}"),
            );
            return;
        };
        if version < MIN_CAPABILITY_KERNEL {
            self.set_unavailable(
                ObserverCapabilityState::Unsupported,
                format!(
                    "kernel {}.{} is unsupported with the least-privilege capability model; Linux 5.8+ is required",
                    version.0, version.1
                ),
            );
            return;
        }

        match effective_capabilities() {
            Ok(caps) if has_capability(caps, CAP_BPF) && has_capability(caps, CAP_PERFMON) => {}
            Ok(_) => {
                self.set_unavailable(
                    ObserverCapabilityState::MissingCapability,
                    "effective CAP_BPF and CAP_PERFMON are required".into(),
                );
                return;
            }
            Err(err) => {
                self.set_unavailable(
                    ObserverCapabilityState::Unavailable,
                    format!("cannot inspect effective Linux capabilities: {err}"),
                );
                return;
            }
        }

        let mut config = BpfObserverConfig::default();
        if let Some(cgroup_id) = current_cgroup_id() {
            config.ignored_cgroup_ids.insert(cgroup_id);
        }
        if let Some(path) = runtime_object_path() {
            config.object_path = Some(path);
        }
        match BpfObserver::start(hub, config) {
            Ok(observer) => {
                *self.observer.lock() = Some(observer);
                self.set_state(ObserverCapabilityState::Active, None);
                tracing::info!(
                    backend = "libbpf-ring-buffer",
                    kernel_release = release.as_deref().unwrap_or("unknown"),
                    capabilities = "CAP_BPF CAP_PERFMON",
                    "eBPF observer active"
                );
            }
            Err(err) => {
                let state = match err.kind() {
                    std::io::ErrorKind::Unsupported => ObserverCapabilityState::Unsupported,
                    std::io::ErrorKind::PermissionDenied => {
                        ObserverCapabilityState::MissingCapability
                    }
                    std::io::ErrorKind::NotFound => ObserverCapabilityState::Unavailable,
                    _ => ObserverCapabilityState::Failed,
                };
                self.set_unavailable(state, format!("cannot start libbpf observer: {err}"));
            }
        }
    }

    pub fn status(&self, queue_events_dropped: u64) -> ObserverCapabilityStatus {
        let stopped = self
            .observer
            .lock()
            .as_ref()
            .is_some_and(|observer| !observer.is_running());
        if stopped {
            self.set_state(
                ObserverCapabilityState::Failed,
                Some("libbpf observer worker stopped".into()),
            );
        }
        let mut status = self.status.lock().clone();
        status.queue_events_dropped = queue_events_dropped;
        status
    }

    pub fn loss_generation(&self) -> u64 {
        self.status.lock().loss_events
    }

    pub fn complete_reconciliation(&self, expected_loss_generation: u64) -> bool {
        let mut status = self.status.lock();
        if status.loss_events != expected_loss_generation {
            return false;
        }
        status.reconciliation_required = false;
        true
    }

    pub fn signal_loss(&self, source: &'static str, detail: impl Into<String>) {
        let detail = detail.into();
        let mut status = self.status.lock();
        status.loss_events = status.loss_events.saturating_add(1);
        status.reconciliation_required = true;
        tracing::warn!(
            source,
            detail,
            loss_events = status.loss_events,
            "observer event loss detected; reconciliation required"
        );
    }

    fn set_unavailable(&self, state: ObserverCapabilityState, reason: String) {
        self.set_state(state.clone(), Some(reason.clone()));
        tracing::warn!(
            ?state,
            reason,
            "eBPF observer unavailable; continuing with discovery and reconciliation"
        );
    }

    fn set_state(&self, state: ObserverCapabilityState, reason: Option<String>) {
        let mut status = self.status.lock();
        status.state = state;
        status.reason = reason;
    }
}

fn runtime_object_path() -> Option<std::path::PathBuf> {
    std::env::var_os("NOLAND_BPF_OBJECT")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let installed = std::path::PathBuf::from("/usr/local/lib/noland/noland_observer.bpf.o");
            installed.is_file().then_some(installed)
        })
}

#[cfg(target_os = "linux")]
fn current_cgroup_id() -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    let entry = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let relative = entry
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim_start_matches('/');
    std::fs::metadata(std::path::Path::new("/sys/fs/cgroup").join(relative))
        .ok()
        .map(|metadata| metadata.ino())
}

#[cfg(not(target_os = "linux"))]
fn current_cgroup_id() -> Option<u64> {
    None
}

fn kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_owned())
}

fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn effective_capabilities() -> std::io::Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:\t"))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "CapEff is absent"))?;
    u64::from_str_radix(value.trim(), 16)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

fn has_capability(mask: u64, capability: u32) -> bool {
    mask & (1_u64 << capability) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_distribution_kernel_release() {
        assert_eq!(parse_kernel_version("6.8.0-52-generic"), Some((6, 8)));
        assert_eq!(parse_kernel_version("5.15.149-linuxkit"), Some((5, 15)));
        assert_eq!(parse_kernel_version("invalid"), None);
    }

    #[test]
    fn checks_high_numbered_linux_capabilities() {
        let mask = (1_u64 << CAP_BPF) | (1_u64 << CAP_PERFMON);
        assert!(has_capability(mask, CAP_BPF));
        assert!(has_capability(mask, CAP_PERFMON));
        assert!(!has_capability(mask, 21));
    }

    #[test]
    fn capability_status_signals_reconciliation_after_loss() {
        let observer = ObserverSupervisor::new();
        observer.signal_loss("test", "synthetic loss");
        let status = observer.status(7);
        assert_eq!(status.loss_events, 1);
        assert_eq!(status.queue_events_dropped, 7);
        assert!(status.reconciliation_required);
    }

    #[test]
    fn reconciliation_only_clears_the_observed_loss_generation() {
        let observer = ObserverSupervisor::new();
        observer.signal_loss("test", "first loss");
        let generation = observer.loss_generation();
        assert!(observer.complete_reconciliation(generation));
        assert!(!observer.status(0).reconciliation_required);

        observer.signal_loss("test", "second loss");
        assert!(!observer.complete_reconciliation(generation));
        assert!(observer.status(0).reconciliation_required);
    }
}
