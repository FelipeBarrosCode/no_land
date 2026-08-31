#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use noland_state_core::{
    EbpfFilesystemFact, EbpfProcessFact, FsEventKind, ObservationSource, ProcessEventKind,
};

#[cfg(target_os = "linux")]
use crate::abi::parse_record;
use crate::abi::{RawEvent, RawEventKind, FLAG_PARENT_AND_NAME, FLAG_SAMPLED};
use crate::{CgroupResolver, ObserverHub};

pub const DEFAULT_OBJECT_NAME: &str = "noland_observer.bpf.o";
pub const DEFAULT_RINGBUF_MAP: &str = "events";
pub const DEFAULT_CONFIG_MAP: &str = "config";
pub const DEFAULT_IGNORED_CGROUPS_MAP: &str = "ignored_cgroups";
pub const DEFAULT_CGROUP_MODE_MAP: &str = "cgroup_mode";
pub const DEFAULT_LOSS_MAP: &str = "stats";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CgroupObservationMode {
    None = 0,
    Discovery = 1,
    Steady = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BpfFeature {
    Process,
    Filesystem,
}

impl BpfFeature {
    /// Kernel facilities used by this feature's tracing programs.
    pub fn capability_description(self) -> &'static str {
        match self {
            Self::Process => "CAP_BPF and CAP_PERFMON with sched_process raw tracepoints",
            Self::Filesystem => {
                "CAP_BPF and CAP_PERFMON with BTF-enabled fexit/security_* and fexit/vfs_* tracing"
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct BpfObserverConfig {
    /// Override the object generated into this crate's `OUT_DIR`.
    pub object_path: Option<PathBuf>,
    pub ringbuf_map: String,
    pub config_map: String,
    pub ignored_cgroups_map: String,
    pub cgroup_mode_map: String,
    pub loss_map: String,
    pub enabled_features: HashSet<BpfFeature>,
    pub ignored_cgroup_ids: HashSet<u64>,
    pub cgroup_modes: HashMap<u64, CgroupObservationMode>,
    pub default_mode: CgroupObservationMode,
    pub poll_interval: Duration,
    /// `0` or `1` retains all reads; `N` asks BPF to retain roughly 1/N.
    pub read_sample_rate: u32,
    pub target_cgroup_id: u64,
    pub discovery_read_window: Duration,
    pub steady_read_window: Duration,
    pub write_window: Duration,
}

impl Default for BpfObserverConfig {
    fn default() -> Self {
        Self {
            object_path: None,
            ringbuf_map: DEFAULT_RINGBUF_MAP.into(),
            config_map: DEFAULT_CONFIG_MAP.into(),
            ignored_cgroups_map: DEFAULT_IGNORED_CGROUPS_MAP.into(),
            cgroup_mode_map: DEFAULT_CGROUP_MODE_MAP.into(),
            loss_map: DEFAULT_LOSS_MAP.into(),
            enabled_features: [BpfFeature::Process, BpfFeature::Filesystem]
                .into_iter()
                .collect(),
            ignored_cgroup_ids: HashSet::new(),
            cgroup_modes: HashMap::new(),
            default_mode: CgroupObservationMode::Discovery,
            poll_interval: Duration::from_millis(100),
            read_sample_rate: 1,
            target_cgroup_id: 0,
            discovery_read_window: Duration::from_secs(5),
            steady_read_window: Duration::from_secs(60),
            write_window: Duration::from_millis(250),
        }
    }
}

impl BpfObserverConfig {
    pub fn object_path(&self) -> io::Result<PathBuf> {
        if let Some(path) = &self.object_path {
            return Ok(path.clone());
        }
        if let Some(path) = option_env!("NOLAND_OBSERVER_BPF_OBJECT") {
            return Ok(PathBuf::from(path));
        }
        let out_dir = option_env!("OUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_default();
        let candidates = [
            out_dir.join(DEFAULT_OBJECT_NAME),
            out_dir.join("noland-observer.bpf.o"),
            out_dir
                .join("noland_observer.bpf")
                .join(DEFAULT_OBJECT_NAME),
        ];
        candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "BPF object not found under {}; expected {} or set NOLAND_OBSERVER_BPF_OBJECT at build time",
                        out_dir.display(),
                        DEFAULT_OBJECT_NAME
                    ),
                )
            })
    }
}

#[derive(Debug)]
enum NormalizedEvent {
    Process(EbpfProcessFact),
    Filesystem(EbpfFilesystemFact),
}

fn normalize(raw: RawEvent, at: DateTime<Utc>) -> Option<NormalizedEvent> {
    let pid = i32::try_from(raw.tgid).unwrap_or(i32::MAX);
    match raw.kind {
        RawEventKind::ProcessExec | RawEventKind::ProcessFork | RawEventKind::ProcessExit => {
            let kind = match raw.kind {
                RawEventKind::ProcessExec => ProcessEventKind::Exec,
                RawEventKind::ProcessFork => ProcessEventKind::Fork,
                RawEventKind::ProcessExit => ProcessEventKind::Exit,
                _ => unreachable!(),
            };
            let executable = bytes_path(raw.path);
            let comm = nonempty_string(raw.comm);
            Some(NormalizedEvent::Process(EbpfProcessFact {
                kind,
                tgid: pid,
                tid: i32::try_from(raw.pid).unwrap_or(i32::MAX),
                ppid: i32::try_from(raw.ppid).unwrap_or(i32::MAX),
                uid: raw.uid,
                gid: raw.gid,
                cgroup_id: raw.cgroup_id,
                cgroup: CgroupResolver::read_proc_cgroup(pid),
                executable,
                argv_hash: None,
                comm,
                source: ObservationSource::Ebpf,
                sequence: raw.sequence,
                at,
            }))
        }
        kind => {
            let path = event_path(&raw.path, &raw.name, raw.flags);
            let dest_path = event_path(&raw.dest_path, &raw.dest_name, raw.flags);
            Some(NormalizedEvent::Filesystem(EbpfFilesystemFact {
                kind: match kind {
                    RawEventKind::FsOpen => FsEventKind::Open,
                    RawEventKind::FsRead => FsEventKind::Read,
                    RawEventKind::FsWrite => FsEventKind::Write,
                    RawEventKind::FsCreate => FsEventKind::Create,
                    RawEventKind::FsTruncate => FsEventKind::Truncate,
                    RawEventKind::FsRename => FsEventKind::Rename,
                    RawEventKind::FsUnlink => FsEventKind::Unlink,
                    RawEventKind::FsMkdir => FsEventKind::Mkdir,
                    RawEventKind::FsRmdir => FsEventKind::Rmdir,
                    RawEventKind::FsSymlink => FsEventKind::Symlink,
                    RawEventKind::FsChmod => FsEventKind::Chmod,
                    RawEventKind::FsChown => FsEventKind::Chown,
                    RawEventKind::FsMmap => FsEventKind::Mmap,
                    _ => unreachable!(),
                },
                tgid: pid,
                tid: i32::try_from(raw.pid).unwrap_or(i32::MAX),
                ppid: i32::try_from(raw.ppid).unwrap_or(i32::MAX),
                cgroup_id: raw.cgroup_id,
                path,
                second_path: (!dest_path.as_os_str().is_empty()).then_some(dest_path),
                inode: (raw.ino != 0).then_some(raw.ino),
                device: (raw.dev != 0).then_some(raw.dev),
                io_result: Some(raw.result),
                open_flags: matches!(kind, RawEventKind::FsOpen).then_some(raw.operation_flags),
                mmap_prot: matches!(kind, RawEventKind::FsMmap).then_some(raw.mode),
                source: ObservationSource::Ebpf,
                sequence: raw.sequence,
                accumulated_count: raw.accumulated_count.max(1),
                at,
                sampled: raw.flags & FLAG_SAMPLED != 0,
            }))
        }
    }
}

fn event_path(parent: &[u8], name: &[u8], flags: u16) -> PathBuf {
    let mut path = bytes_path(parent.to_vec()).unwrap_or_default();
    if flags & FLAG_PARENT_AND_NAME != 0 && !name.is_empty() {
        path.push(bytes_path(name.to_vec()).unwrap_or_default());
    }
    path
}

fn dispatch(hub: &ObserverHub, event: NormalizedEvent) {
    match event {
        NormalizedEvent::Process(event) => hub.inject_ebpf_process(event),
        NormalizedEvent::Filesystem(event) => hub.inject_ebpf_fs(event),
    }
}

fn nonempty_string(mut bytes: Vec<u8>) -> Option<String> {
    trim_nul(&mut bytes);
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn bytes_path(mut bytes: Vec<u8>) -> Option<PathBuf> {
    trim_nul(&mut bytes);
    if bytes.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

fn trim_nul(bytes: &mut Vec<u8>) {
    if let Some(end) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(end);
    }
}

fn program_feature(section: &str) -> Option<BpfFeature> {
    if section.starts_with("raw_tracepoint/sched_process_") {
        Some(BpfFeature::Process)
    } else if section.starts_with("fentry/security_")
        || section.starts_with("fexit/security_")
        || section.starts_with("fexit/vfs_")
    {
        Some(BpfFeature::Filesystem)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    use libbpf_rs::{
        Link, MapCore, MapFlags, Object, ObjectBuilder, OpenObject, RingBufferBuilder,
    };

    static EMBEDDED_OBJECT: &[u8] = include_bytes!(env!("NOLAND_OBSERVER_BPF_OBJECT"));

    enum Command {
        ReplaceIgnored(HashSet<u64>, mpsc::SyncSender<io::Result<()>>),
        SetCgroupMode(u64, CgroupObservationMode, mpsc::SyncSender<io::Result<()>>),
        Stop,
    }

    pub struct BpfObserver {
        commands: mpsc::Sender<Command>,
        running: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl BpfObserver {
        pub fn start(hub: Arc<ObserverHub>, config: BpfObserverConfig) -> io::Result<Self> {
            let (object, object_label) = if let Some(path) = &config.object_path {
                (std::fs::read(path)?, path.display().to_string())
            } else {
                (
                    EMBEDDED_OBJECT.to_vec(),
                    env!("NOLAND_OBSERVER_BPF_OBJECT").to_owned(),
                )
            };
            let (commands, command_rx) = mpsc::channel();
            let (startup_tx, startup_rx) = mpsc::sync_channel(1);
            let running = Arc::new(AtomicBool::new(false));
            let worker_running = running.clone();
            let thread = thread::Builder::new()
                .name("noland-bpf-observer".into())
                .spawn(move || {
                    let result = run(
                        hub,
                        config,
                        &object,
                        &object_label,
                        command_rx,
                        worker_running.clone(),
                        startup_tx,
                    );
                    if let Err(error) = result {
                        tracing::error!(error = %error, "BPF observer stopped");
                    }
                    worker_running.store(false, Ordering::Release);
                })?;

            match startup_rx.recv() {
                Ok(Ok(())) => Ok(Self {
                    commands,
                    running,
                    thread: Some(thread),
                }),
                Ok(Err(error)) => {
                    let _ = thread.join();
                    Err(error)
                }
                Err(_) => {
                    let _ = thread.join();
                    Err(io::Error::new(
                        io::ErrorKind::Other,
                        "BPF observer exited during startup",
                    ))
                }
            }
        }

        pub fn replace_ignored_cgroups(&self, ids: HashSet<u64>) -> io::Result<()> {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            self.commands
                .send(Command::ReplaceIgnored(ids, reply_tx))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "BPF observer is stopped")
                })?;
            reply_rx.recv().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "BPF observer stopped during map update",
                )
            })?
        }

        pub fn set_cgroup_mode(
            &self,
            cgroup_id: u64,
            mode: CgroupObservationMode,
        ) -> io::Result<()> {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            self.commands
                .send(Command::SetCgroupMode(cgroup_id, mode, reply_tx))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "BPF observer is stopped")
                })?;
            reply_rx.recv().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "BPF observer stopped during mode update",
                )
            })?
        }

        pub fn is_running(&self) -> bool {
            self.running.load(Ordering::Acquire)
        }

        pub fn stop(&mut self) -> io::Result<()> {
            let _ = self.commands.send(Command::Stop);
            if let Some(thread) = self.thread.take() {
                thread.join().map_err(|_| {
                    io::Error::new(io::ErrorKind::Other, "BPF observer thread panicked")
                })?;
            }
            Ok(())
        }
    }

    impl Drop for BpfObserver {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    fn run(
        hub: Arc<ObserverHub>,
        config: BpfObserverConfig,
        object_data: &[u8],
        object_label: &str,
        commands: mpsc::Receiver<Command>,
        running: Arc<AtomicBool>,
        startup: mpsc::SyncSender<io::Result<()>>,
    ) -> io::Result<()> {
        let result = (|| {
            let (supported, gaps) = probe_programs(object_data, &config.enabled_features)?;
            let mut open = open_object(object_data)?;
            select_programs(&mut open, &supported);
            let object = open.load().map_err(libbpf_error)?;
            let links = attach_selected_programs(&object, &supported)?;
            for gap in &gaps {
                tracing::warn!(tracing_program_gap = %gap, "optional BPF tracing program unavailable");
            }
            write_config(&object, &config)?;
            replace_ignored(
                &object,
                &config.ignored_cgroups_map,
                &HashSet::new(),
                &config.ignored_cgroup_ids,
            )?;
            for (&cgroup_id, &mode) in &config.cgroup_modes {
                set_cgroup_mode(&object, &config.cgroup_mode_map, cgroup_id, mode)?;
            }

            let events_map = object
                .maps()
                .find(|map| map.name() == config.ringbuf_map.as_str())
                .ok_or_else(|| missing_map(&config.ringbuf_map))?;
            let callback_hub = hub.clone();
            let clock = BootClock::new();
            let mut builder = RingBufferBuilder::new();
            builder
                .add(&events_map, move |bytes| {
                    match parse_record(bytes) {
                        Ok(raw) => {
                            let at = clock.at(raw.timestamp_ns);
                            if let Some(event) = normalize(raw, at) {
                                dispatch(&callback_hub, event);
                            }
                        }
                        Err(error) => {
                            callback_hub.report_loss(1);
                            tracing::warn!(error = %error, "discarding malformed BPF event");
                        }
                    }
                    0
                })
                .map_err(libbpf_error)?;
            let ring = builder.build().map_err(libbpf_error)?;

            let mut ignored = config.ignored_cgroup_ids.clone();
            let mut last_lost = 0;
            running.store(true, Ordering::Release);
            let _ = startup.send(Ok(()));
            tracing::info!(
                object = object_label,
                links = links.len(),
                "BPF observer started"
            );

            loop {
                while let Ok(command) = commands.try_recv() {
                    match command {
                        Command::ReplaceIgnored(ids, reply) => {
                            let update = replace_ignored(
                                &object,
                                &config.ignored_cgroups_map,
                                &ignored,
                                &ids,
                            );
                            if update.is_ok() {
                                ignored = ids;
                            }
                            let failed = update
                                .as_ref()
                                .err()
                                .map(|error| io::Error::new(error.kind(), error.to_string()));
                            let _ = reply.send(failed.map_or(Ok(()), Err));
                            update?;
                        }
                        Command::SetCgroupMode(cgroup_id, mode, reply) => {
                            let update =
                                set_cgroup_mode(&object, &config.cgroup_mode_map, cgroup_id, mode);
                            let failed = update
                                .as_ref()
                                .err()
                                .map(|error| io::Error::new(error.kind(), error.to_string()));
                            let _ = reply.send(failed.map_or(Ok(()), Err));
                            update?;
                        }
                        Command::Stop => return Ok(()),
                    }
                }
                ring.poll(config.poll_interval).map_err(libbpf_error)?;
                if let Some(total) = read_loss(&object, &config.loss_map)? {
                    let delta = if total >= last_lost {
                        total - last_lost
                    } else {
                        total
                    };
                    if delta > 0 {
                        hub.report_loss(delta);
                    }
                    last_lost = total;
                }
            }
        })();

        if let Err(error) = &result {
            let _ = startup.send(Err(io::Error::new(error.kind(), error.to_string())));
        }
        result
    }

    fn open_object(data: &[u8]) -> io::Result<OpenObject> {
        ObjectBuilder::default()
            .open_memory(data)
            .map_err(libbpf_error)
    }

    fn probe_programs(
        object_data: &[u8],
        enabled: &HashSet<BpfFeature>,
    ) -> io::Result<(HashSet<String>, Vec<String>)> {
        let discovery = open_object(object_data)?;
        let candidates: Vec<(String, String, BpfFeature)> = discovery
            .progs()
            .filter_map(|program| {
                let section = program.section().to_string_lossy().into_owned();
                let feature = program_feature(&section)?;
                enabled.contains(&feature).then(|| {
                    (
                        program.name().to_string_lossy().into_owned(),
                        section,
                        feature,
                    )
                })
            })
            .collect();
        drop(discovery);

        let mut supported = HashSet::new();
        let mut gaps = Vec::new();
        for (name, section, feature) in candidates {
            match probe_program(object_data, &name) {
                Ok(()) => {
                    supported.insert(name);
                }
                Err(error) => gaps.push(format!(
                    "{name} [{section}] ({feature:?}; {}): {error}",
                    feature.capability_description()
                )),
            }
        }
        if supported.contains("noland_security_file_truncate") {
            supported.remove("noland_security_path_truncate");
        }
        if supported.is_empty() && !enabled.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "none of the requested BPF tracing programs are supported: {}",
                    gaps.join(", ")
                ),
            ));
        }
        Ok((supported, gaps))
    }

    fn probe_program(object_data: &[u8], selected: &str) -> io::Result<()> {
        let mut open = open_object(object_data)?;
        for mut program in open.progs_mut() {
            let autoload = program.name() == selected;
            program.set_autoload(autoload);
        }
        let object = open.load().map_err(libbpf_error)?;
        let program = object
            .progs()
            .find(|program| program.name() == selected)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("BPF program {selected} disappeared after load"),
                )
            })?;
        let _probe_link = program.attach().map_err(libbpf_error)?;
        Ok(())
    }

    fn select_programs(open: &mut libbpf_rs::OpenObject, selected: &HashSet<String>) {
        for mut program in open.progs_mut() {
            program.set_autoload(selected.contains(program.name().to_string_lossy().as_ref()));
        }
    }

    fn attach_selected_programs(
        object: &Object,
        selected: &HashSet<String>,
    ) -> io::Result<Vec<Link>> {
        object
            .progs()
            .filter(|program| selected.contains(program.name().to_string_lossy().as_ref()))
            .map(|program| program.attach().map_err(libbpf_error))
            .collect()
    }

    fn write_config(object: &Object, config: &BpfObserverConfig) -> io::Result<()> {
        let Some(map) = object
            .maps()
            .find(|map| map.name() == config.config_map.as_str())
        else {
            tracing::debug!(map = %config.config_map, "BPF config map is not present");
            return Ok(());
        };
        let key = vec![0; map.key_size() as usize];
        let mut value = vec![0; map.value_size() as usize];
        const CONFIG_V1_SIZE: usize = 64;
        if value.len() < CONFIG_V1_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BPF config map value is smaller than noland_config_v1",
            ));
        }
        let mut enabled_mask = 0_u64;
        if config.enabled_features.contains(&BpfFeature::Process) {
            enabled_mask |= 1 << 0;
        }
        if config.enabled_features.contains(&BpfFeature::Filesystem) {
            enabled_mask |= (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
        }
        value[0..2].copy_from_slice(&crate::abi::ABI_VERSION.to_le_bytes());
        value[2..4].copy_from_slice(&(CONFIG_V1_SIZE as u16).to_le_bytes());
        value[8..16].copy_from_slice(&enabled_mask.to_le_bytes());
        value[16..24].copy_from_slice(&config.target_cgroup_id.to_le_bytes());
        let discovery_ns = duration_ns(config.discovery_read_window);
        let steady_ns = duration_ns(config.steady_read_window);
        let write_ns = duration_ns(config.write_window);
        value[24..32].copy_from_slice(&discovery_ns.to_le_bytes());
        value[32..40].copy_from_slice(&steady_ns.to_le_bytes());
        value[40..48].copy_from_slice(&write_ns.to_le_bytes());
        value[48..52].copy_from_slice(&config.read_sample_rate.to_le_bytes());
        value[52..56].copy_from_slice(&(unsafe { libc::getpid() } as u32).to_le_bytes());
        value[56..60].copy_from_slice(&(config.default_mode as u32).to_le_bytes());
        map.update(&key, &value, MapFlags::ANY)
            .map_err(libbpf_error)?;
        Ok(())
    }

    fn duration_ns(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    fn set_cgroup_mode(
        object: &Object,
        map_name: &str,
        cgroup_id: u64,
        mode: CgroupObservationMode,
    ) -> io::Result<()> {
        let map = object
            .maps()
            .find(|map| map.name() == map_name)
            .ok_or_else(|| missing_map(map_name))?;
        if map.key_size() != 8 || map.value_size() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "cgroup-mode BPF map must use u64 keys and u32 values",
            ));
        }
        if mode == CgroupObservationMode::None {
            let mut value = vec![0; map.value_size() as usize];
            value[..4].copy_from_slice(&(mode as u32).to_le_bytes());
            map.update(&cgroup_id.to_le_bytes(), &value, MapFlags::ANY)
                .map_err(libbpf_error)
        } else {
            let mut value = vec![0; map.value_size() as usize];
            value[..4].copy_from_slice(&(mode as u32).to_le_bytes());
            map.update(&cgroup_id.to_le_bytes(), &value, MapFlags::ANY)
                .map_err(libbpf_error)
        }
    }

    fn replace_ignored(
        object: &Object,
        map_name: &str,
        old: &HashSet<u64>,
        new: &HashSet<u64>,
    ) -> io::Result<()> {
        let Some(map) = object.maps().find(|map| map.name() == map_name) else {
            if new.is_empty() {
                return Ok(());
            }
            return Err(missing_map(map_name));
        };
        if map.key_size() != 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ignored-cgroups BPF map must use u64 keys",
            ));
        }
        for id in old.difference(new) {
            map.delete(&id.to_le_bytes()).map_err(libbpf_error)?;
        }
        let mut value = vec![0; map.value_size() as usize];
        if let Some(first) = value.first_mut() {
            *first = 1;
        }
        for id in new {
            map.update(&id.to_le_bytes(), &value, MapFlags::ANY)
                .map_err(libbpf_error)?;
        }
        Ok(())
    }

    fn read_loss(object: &Object, map_name: &str) -> io::Result<Option<u64>> {
        let Some(map) = object.maps().find(|map| map.name() == map_name) else {
            return Ok(None);
        };
        let key = vec![0; map.key_size() as usize];
        if key.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BPF stats map key is smaller than u32",
            ));
        }
        // NOLAND_STAT_RINGBUF_DROPPED is the stable key zero.
        let values = if map.map_type().is_percpu() {
            map.lookup_percpu(&key, MapFlags::ANY)
                .map_err(libbpf_error)?
                .unwrap_or_default()
        } else {
            map.lookup(&key, MapFlags::ANY)
                .map_err(libbpf_error)?
                .into_iter()
                .collect()
        };
        Ok(Some(values.iter().map(|value| read_counter(value)).sum()))
    }

    fn read_counter(bytes: &[u8]) -> u64 {
        if bytes.len() >= 8 {
            u64::from_le_bytes(bytes[..8].try_into().expect("checked counter length"))
        } else if bytes.len() >= 4 {
            u32::from_le_bytes(bytes[..4].try_into().expect("checked counter length")) as u64
        } else {
            0
        }
    }

    fn missing_map(name: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("BPF object has no map named {name}"),
        )
    }

    fn libbpf_error(error: libbpf_rs::Error) -> io::Error {
        io::Error::new(io::ErrorKind::Other, error.to_string())
    }

    #[derive(Clone, Copy)]
    struct BootClock {
        wall: DateTime<Utc>,
        boot_ns: u64,
    }

    impl BootClock {
        fn new() -> Self {
            Self {
                wall: Utc::now(),
                boot_ns: boottime_ns(),
            }
        }

        fn at(self, event_ns: u64) -> DateTime<Utc> {
            let delta = event_ns as i128 - self.boot_ns as i128;
            let nanos = delta.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            self.wall + chrono::Duration::nanoseconds(nanos)
        }
    }

    fn boottime_ns() -> u64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a valid writable timespec pointer.
        let result = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) };
        if result == 0 {
            (time.tv_sec as u64)
                .saturating_mul(1_000_000_000)
                .saturating_add(time.tv_nsec as u64)
        } else {
            0
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    #[derive(Debug, Default)]
    pub struct BpfObserver;

    impl BpfObserver {
        pub fn start(_hub: Arc<ObserverHub>, _config: BpfObserverConfig) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the libbpf observer is only available on Linux",
            ))
        }

        pub fn replace_ignored_cgroups(&self, _ids: HashSet<u64>) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the libbpf observer is only available on Linux",
            ))
        }

        pub fn set_cgroup_mode(
            &self,
            _cgroup_id: u64,
            _mode: CgroupObservationMode,
        ) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the libbpf observer is only available on Linux",
            ))
        }

        pub fn is_running(&self) -> bool {
            false
        }

        pub fn stop(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

pub use platform::BpfObserver;

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_fs(flags: u16, result: i64) -> RawEvent {
        RawEvent {
            kind: RawEventKind::FsRename,
            flags,
            timestamp_ns: 0,
            cgroup_id: 0,
            dev: 0,
            ino: 0,
            result,
            offset: 0,
            length: 0,
            pid: 9,
            tgid: 9,
            ppid: 1,
            uid: 1000,
            gid: 1000,
            mnt_ns: 0,
            mode: 0,
            operation_flags: 0,
            sequence: 1,
            accumulated_count: 1,
            comm: b"mv".to_vec(),
            path: b"/old".to_vec(),
            name: b"save".to_vec(),
            dest_path: b"/new".to_vec(),
            dest_name: b"save".to_vec(),
        }
    }

    #[test]
    fn reconstructs_parent_and_name_paths() {
        let event = normalize(raw_fs(FLAG_PARENT_AND_NAME, 0), Utc::now()).unwrap();
        let NormalizedEvent::Filesystem(event) = event else {
            panic!("expected filesystem event");
        };
        assert_eq!(event.path, PathBuf::from("/old/save"));
        assert_eq!(event.second_path, Some(PathBuf::from("/new/save")));
    }

    #[test]
    fn retains_kernel_result_for_attribution_validation() {
        let event = normalize(raw_fs(0, -13), Utc::now()).unwrap();
        let NormalizedEvent::Filesystem(fact) = event else {
            panic!("expected filesystem event");
        };
        assert_eq!(fact.io_result, Some(-13));
    }

    #[test]
    fn classifies_tracing_sections_without_lsm_assumptions() {
        assert_eq!(
            program_feature("raw_tracepoint/sched_process_exec"),
            Some(BpfFeature::Process)
        );
        assert_eq!(
            program_feature("fexit/security_file_open"),
            Some(BpfFeature::Filesystem)
        );
        assert_eq!(
            program_feature("fexit/security_path_rename"),
            Some(BpfFeature::Filesystem)
        );
        assert_eq!(
            program_feature("fexit/vfs_write"),
            Some(BpfFeature::Filesystem)
        );
        assert_eq!(program_feature("lsm/file_open"), None);
        assert_eq!(
            program_feature("tracepoint/syscalls/sys_enter_openat"),
            None
        );
    }

    #[test]
    fn filesystem_capability_description_is_for_tracing_not_lsm() {
        let description = BpfFeature::Filesystem.capability_description();
        assert!(description.contains("CAP_BPF"));
        assert!(description.contains("CAP_PERFMON"));
        assert!(description.contains("fexit/security_*"));
        assert!(!description.contains("LSM"));
    }

    #[test]
    fn dispatches_normalized_events_into_hub() {
        let hub = ObserverHub::new(noland_state_core::metrics::Metrics::shared());
        let event = normalize(raw_fs(0, 0), Utc::now()).unwrap();
        dispatch(&hub, event);
        assert_eq!(hub.drain().len(), 1);
    }

    #[test]
    fn default_object_path_is_actionable_when_artifact_is_missing() {
        let config = BpfObserverConfig::default();
        if let Err(error) = config.object_path() {
            assert!(error.to_string().contains(DEFAULT_OBJECT_NAME));
        }
    }
}
