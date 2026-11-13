//! Turn private runtime paths into durable host paths while the originating
//! process is still alive. A pathname (or an executable basename) alone is not
//! evidence of which application's private home was mounted.

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};

pub const RUNTIME_HOME: &str = "/home/runtime";

pub fn is_runtime_namespace_path(path: &Path) -> bool {
    path.starts_with(RUNTIME_HOME)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt};

    struct Fixture {
        root: PathBuf,
        proc: PathBuf,
        homes: [PathBuf; 2],
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::current_dir()
                .unwrap()
                .join("target/host-path-tests")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let proc = root.join("proc");
            fs::create_dir_all(proc.join("self")).unwrap();
            fs::write(
                proc.join("self/mountinfo"),
                "1 0 8:1 / / rw - ext4 /dev/root rw\n",
            )
            .unwrap();
            let homes = ["one", "two"].map(|app| {
                root.join("host with spaces/.local/share/noland-runtime/app-data")
                    .join(app)
            });
            for home in &homes {
                fs::create_dir_all(home.join("Games/Identical/saves")).unwrap();
                fs::write(home.join("Games/Identical/game"), b"same executable").unwrap();
                fs::write(home.join("Games/Identical/saves/current"), b"same contents").unwrap();
            }
            let fixture = Self { root, proc, homes };
            fixture.process(41, 0, 1, 1041);
            fixture.process(42, 1, 1, 1042);
            fixture
        }

        fn process(&self, pid: i32, home: usize, start: u64, namespace: u64) {
            let process = self.proc.join(pid.to_string());
            let _ = fs::remove_dir_all(&process);
            fs::create_dir_all(process.join("ns")).unwrap();
            fs::create_dir_all(process.join("root/home")).unwrap();
            symlink(&self.homes[home], process.join("root/home/runtime")).unwrap();
            symlink(format!("mnt:[{namespace}]"), process.join("ns/mnt")).unwrap();
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = "S".into();
            fields[1] = "1".into();
            fields[19] = start.to_string();
            fs::write(
                process.join("stat"),
                format!("{pid} (game ) name) {}", fields.join(" ")),
            )
            .unwrap();
            self.mount(pid, home);
        }

        fn mount(&self, pid: i32, home: usize) {
            let path = self.homes[home].to_string_lossy().replace(' ', "\\040");
            fs::write(self.proc.join(pid.to_string()).join("mountinfo"), format!(
                "10 0 0:10 / / rw - tmpfs tmpfs rw\n11 10 8:1 {path} /home/runtime rw - ext4 /dev/root rw\n"
            )).unwrap();
        }

        fn resolver(&self) -> HostPathResolver {
            HostPathResolver::with_proc_root(self.proc.clone())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn same_namespace_path_in_two_apps_uses_mount_and_inode_identity() {
        let fixture = Fixture::new();
        let mut resolver = fixture.resolver();
        let path = Path::new("/home/runtime/Games/Identical/saves/current");
        for (pid, index) in [(41, 0), (42, 1)] {
            let host = fixture.homes[index].join("Games/Identical/saves/current");
            let meta = fs::metadata(&host).unwrap();
            assert_eq!(
                resolver.resolve(
                    pid,
                    path,
                    PathContext {
                        mount_namespace: Some(1000 + pid as u64),
                        inode: Some(meta.ino()),
                        device: Some(meta.dev()),
                        ..PathContext::default()
                    }
                ),
                Ok(host)
            );
        }
        let wrong_inode = fs::metadata(fixture.homes[1].join("Games/Identical/saves/current"))
            .unwrap()
            .ino();
        assert!(resolver
            .resolve(
                41,
                path,
                PathContext {
                    inode: Some(wrong_inode),
                    ..PathContext::default()
                }
            )
            .is_err());
    }

    #[test]
    fn rename_delete_and_directory_mutations_survive_missing_leaves() {
        let fixture = Fixture::new();
        let mut resolver = fixture.resolver();
        let mut event = crate::fs_event(
            noland_state_core::FsEventKind::Rename,
            41,
            "/home/runtime/Games/Identical/old/missing",
        );
        event.dest_path = Some(PathBuf::from("/home/runtime/Games/Identical/saves/current"));
        crate::normalize_filesystem_event(&mut resolver, &mut event, PathContext::default())
            .unwrap();
        assert_eq!(
            event.path,
            fixture.homes[0].join("Games/Identical/old/missing")
        );
        assert_eq!(
            event.dest_path,
            Some(fixture.homes[0].join("Games/Identical/saves/current"))
        );
        for kind in [
            noland_state_core::FsEventKind::Unlink,
            noland_state_core::FsEventKind::Rmdir,
            noland_state_core::FsEventKind::Mkdir,
        ] {
            let mut event = crate::fs_event(kind, 41, "/home/runtime/Games/Identical/gone/nested");
            crate::normalize_filesystem_event(&mut resolver, &mut event, PathContext::default())
                .unwrap();
            assert_eq!(
                event.path,
                fixture.homes[0].join("Games/Identical/gone/nested")
            );
        }
        assert_eq!(
            resolver.resolve(41, Path::new(RUNTIME_HOME), PathContext::default()),
            Ok(fixture.homes[0].clone())
        );
        // Missing ordinary I/O is uncertain, not evidence of a durable leaf.
        assert!(resolver
            .resolve(
                41,
                Path::new("/home/runtime/missing"),
                PathContext::default()
            )
            .is_err());
    }

    #[test]
    fn checks_parent_identity_without_confusing_it_with_the_leaf() {
        let fixture = Fixture::new();
        let parent = fs::metadata(fixture.homes[0].join("Games/Identical/saves")).unwrap();
        let mut resolver = fixture.resolver();
        let path = Path::new("/home/runtime/Games/Identical/saves/deleted");
        let context = PathContext {
            parent_identity: true,
            inode: Some(parent.ino()),
            device: Some(parent.dev()),
            ..PathContext::default()
        };
        assert_eq!(
            resolver.resolve(41, path, context),
            Ok(fixture.homes[0].join("Games/Identical/saves/deleted"))
        );
        assert!(resolver
            .resolve(
                41,
                path,
                PathContext {
                    inode: Some(parent.ino() + 1),
                    ..context
                }
            )
            .is_err());
    }

    #[test]
    fn rejects_traversal_symlink_escapes_and_proc_magic_paths() {
        let fixture = Fixture::new();
        symlink(&fixture.homes[1], fixture.homes[0].join("escape")).unwrap();
        let mut resolver = fixture.resolver();
        for path in [
            "/home/runtime/../secret",
            "/home/runtime/escape/missing",
            "/home/runtime/escape/Games/Identical/game",
            "/proc/41/root/home/runtime/file",
        ] {
            assert!(
                resolver
                    .resolve(
                        41,
                        Path::new(path),
                        PathContext {
                            allow_missing: true,
                            ..PathContext::default()
                        }
                    )
                    .is_err(),
                "{path}"
            );
        }
        assert!(resolver
            .resolve(
                41,
                &fixture.homes[0].join("escape/missing"),
                PathContext {
                    allow_missing: true,
                    ..PathContext::default()
                }
            )
            .is_err());
    }

    #[test]
    fn ambiguous_or_incorrect_mounts_never_fall_back_to_a_matching_name() {
        let fixture = Fixture::new();
        let mut resolver = fixture.resolver();
        let path = Path::new("/home/runtime/Games/Identical/game");
        fixture.mount(41, 1); // namespace still exposes app one, mountinfo claims app two
        assert!(resolver.resolve(41, path, PathContext::default()).is_err());
        fixture.mount(41, 0);
        let mountinfo = fixture.proc.join("41/mountinfo");
        let text = fs::read_to_string(&mountinfo).unwrap();
        fs::write(
            &mountinfo,
            format!(
                "{text}{}\n",
                text.lines().nth(1).unwrap().replacen("11 ", "12 ", 1)
            ),
        )
        .unwrap();
        assert_eq!(
            resolver.resolve(41, path, PathContext::default()),
            Err(PathResolutionError::Ambiguous)
        );
        fs::remove_file(mountinfo).unwrap();
        assert!(resolver.resolve(41, path, PathContext::default()).is_err());
    }

    #[test]
    fn pid_reuse_namespace_changes_and_remounts_invalidate_cached_mapping() {
        let fixture = Fixture::new();
        let mut resolver = fixture.resolver();
        let path = Path::new("/home/runtime/Games/Identical/game");
        let first_token = resolver.process_token(41);
        assert_eq!(
            resolver.resolve(41, path, PathContext::default()),
            Ok(fixture.homes[0].join("Games/Identical/game"))
        );
        fixture.process(41, 1, 2, 2041);
        assert_ne!(first_token, resolver.process_token(41));
        assert_eq!(
            resolver.resolve(
                41,
                path,
                PathContext {
                    mount_namespace: Some(1041),
                    ..PathContext::default()
                }
            ),
            Err(PathResolutionError::ProcessChanged)
        );
        assert_eq!(
            resolver.resolve(41, path, PathContext::default()),
            Ok(fixture.homes[1].join("Games/Identical/game"))
        );
        // A bind remount changes neither PID/starttime nor namespace identity.
        let runtime_home = fixture.proc.join("41/root/home/runtime");
        fs::remove_file(&runtime_home).unwrap();
        symlink(&fixture.homes[0], &runtime_home).unwrap();
        fixture.mount(41, 0);
        assert_eq!(
            resolver.resolve(41, path, PathContext::default()),
            Ok(fixture.homes[0].join("Games/Identical/game"))
        );
        fs::remove_dir_all(fixture.proc.join("41")).unwrap();
        assert!(resolver.resolve(41, path, PathContext::default()).is_err());
        assert!(fixture
            .resolver()
            .resolve(41, path, PathContext::default())
            .is_err());
    }

    #[test]
    fn caches_are_bounded_and_normal_host_paths_do_not_need_procfs() {
        let fixture = Fixture::new();
        let mut resolver = fixture.resolver();
        for pid in 100..140 {
            fixture.process(pid, 0, 1, pid as u64);
            resolver
                .resolve(
                    pid,
                    Path::new("/home/runtime/Games/Identical/game"),
                    PathContext::default(),
                )
                .unwrap();
        }
        assert!(resolver.cache.len() <= 32);
        let host = fixture.root.join("ordinary/missing/host-file");
        assert_eq!(
            resolver.resolve(-1, &host, PathContext::default()),
            Ok(host)
        );
    }

    #[test]
    fn open_file_recovery_checks_the_descriptor_and_rejects_deleted_replacements() {
        let fixture = Fixture::new();
        let mut resolver = fixture.resolver();
        let host = fixture.homes[0].join("Games/Identical/saves/current");
        let descriptor = fs::metadata(&host).unwrap();
        let path = Path::new("/home/runtime/Games/Identical/saves/current");
        assert_eq!(
            platform::recover_open_file(&mut resolver, 41, path, &descriptor, Some(1041)),
            Ok(host)
        );
        assert!(
            platform::recover_open_file(&mut resolver, 42, path, &descriptor, Some(1042)).is_err()
        );
        assert!(platform::recover_open_file(
            &mut resolver,
            41,
            Path::new("/home/runtime/Games/Identical/saves/current (deleted)"),
            &descriptor,
            Some(1041)
        )
        .is_err());
    }

    #[test]
    fn a_nested_mount_cannot_escape_into_another_runtime_app() {
        let fixture = Fixture::new();
        let mountinfo = fixture.proc.join("41/mountinfo");
        let mut text = fs::read_to_string(&mountinfo).unwrap();
        text.push_str(&format!(
            "12 11 8:1 {} /home/runtime/other rw - ext4 /dev/root rw\n",
            fixture.homes[1].to_string_lossy().replace(' ', "\\040")
        ));
        fs::write(&mountinfo, text).unwrap();
        assert!(fixture
            .resolver()
            .resolve(
                41,
                Path::new("/home/runtime/other/Games/Identical/game"),
                PathContext::default()
            )
            .is_err());
    }

    #[test]
    fn a_failed_rename_endpoint_keeps_the_event_atomic() {
        let fixture = Fixture::new();
        symlink(&fixture.homes[1], fixture.homes[0].join("escape")).unwrap();
        let mut event = crate::fs_event(
            noland_state_core::FsEventKind::Rename,
            41,
            "/home/runtime/Games/Identical/gone",
        );
        event.dest_path = Some("/home/runtime/escape/missing".into());
        let before = event.clone();
        assert!(crate::normalize_filesystem_event(
            &mut fixture.resolver(),
            &mut event,
            PathContext::default()
        )
        .is_err());
        assert_eq!(event.path, before.path);
        assert_eq!(event.dest_path, before.dest_path);
    }
}

/// The runtime's per-app storage boundary, not an application identity. Runtime
/// IDs and state-agent AppIds are deliberately not interchangeable.
pub fn runtime_private_home(path: &Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        prefix.push(component.as_os_str());
        if index >= 4
            && components[index - 4].as_os_str() == ".local"
            && components[index - 3].as_os_str() == "share"
            && components[index - 2].as_os_str() == "noland-runtime"
            && components[index - 1].as_os_str() == "app-data"
            && matches!(component, Component::Normal(_))
        {
            return Some(prefix);
        }
    }
    None
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PathContext {
    /// Mount namespace from the kernel event, when available.
    pub mount_namespace: Option<u64>,
    /// bpf_ktime_get_ns(), used to reject events from a previous occupant of a PID.
    pub monotonic_ns: Option<u64>,
    pub observed_at: Option<DateTime<Utc>>,
    pub inode: Option<u64>,
    pub device: Option<u64>,
    /// security_path_* facts identify the parent, not the mutated leaf.
    pub parent_identity: bool,
    pub allow_missing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathResolutionError {
    InvalidPath,
    ProcessChanged,
    Unavailable,
    Ambiguous,
    OutsideRuntimeHome,
    IdentityMismatch,
}

fn valid_absolute(path: &Path) -> bool {
    let mut parts = path.components();
    let proc_magic_link = parts.next() == Some(Component::RootDir)
        && parts.next().is_some_and(|part| part.as_os_str() == "proc")
        && parts.next().is_some()
        && parts.next().is_some_and(|part| part.as_os_str() == "root");
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        && !proc_magic_link
}

/// Only parsed mount tables are cached. Every lookup checks process start time,
/// namespace, root identity and current mountinfo; every mapping verifies inode /
/// device identities again. Missing leaves are resolved from a verified ancestor.
pub struct HostPathResolver {
    proc_root: PathBuf,
    #[cfg(unix)]
    cache: std::collections::VecDeque<platform::CachedMounts>,
}

impl Default for HostPathResolver {
    fn default() -> Self {
        Self::with_proc_root(PathBuf::from("/proc"))
    }
}

impl HostPathResolver {
    /// Also permits fixture procfs trees without mounting a namespace in tests.
    pub fn with_proc_root(proc_root: PathBuf) -> Self {
        Self {
            proc_root,
            #[cfg(unix)]
            cache: Default::default(),
        }
    }

    pub fn invalidate_pid(&mut self, pid: i32) {
        #[cfg(unix)]
        self.cache
            .retain(|entry| entry.identity.is_none_or(|id| id.pid != pid));
        #[cfg(not(unix))]
        let _ = pid;
    }

    /// Opaque, short-lived generation token for queued observations. It is never
    /// persisted and includes the mount namespace and chroot, not just the PID.
    pub fn process_token(&self, pid: i32) -> Option<String> {
        #[cfg(unix)]
        {
            platform::process_identity(&self.proc_root, pid).map(|identity| format!("{identity:?}"))
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }

    pub fn resolve(
        &mut self,
        pid: i32,
        path: &Path,
        context: PathContext,
    ) -> Result<PathBuf, PathResolutionError> {
        // Ordinary host paths retain the existing observation semantics. Never
        // make a procfs magic link, traversal, or an unresolved runtime path durable.
        if !valid_absolute(path) {
            return Err(PathResolutionError::InvalidPath);
        }
        if !is_runtime_namespace_path(path) {
            #[cfg(unix)]
            if let Some(home) = runtime_private_home(path) {
                return platform::check_host_path(path, &home, context);
            }
            return Ok(path.to_path_buf());
        }
        #[cfg(unix)]
        {
            self.resolve_runtime(pid, path, context)
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, context, &self.proc_root);
            Err(PathResolutionError::Unavailable)
        }
    }
}

#[cfg(unix)]
pub(crate) mod platform {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::io::Read;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, Instant};

    const MAX_MOUNTINFO_BYTES: u64 = 1024 * 1024;
    const MAX_MOUNTS: usize = 4096;
    const MAX_CACHED_TABLES: usize = 32;
    const CACHE_TTL: Duration = Duration::from_secs(30);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(crate) struct ProcessIdentity {
        pub pid: i32,
        pub start_ticks: u64,
        pub mount_namespace: u64,
        root_device: u64,
        root_inode: u64,
    }

    pub(crate) fn process_identity(proc_root: &Path, pid: i32) -> Option<ProcessIdentity> {
        if pid <= 0 {
            return None;
        }
        let process = proc_root.join(pid.to_string());
        let stat = fs::read_to_string(process.join("stat")).ok()?;
        // comm can contain spaces and ')'. starttime is field 22.
        let start_ticks = stat
            .rsplit_once(") ")?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()?;
        let namespace = fs::read_link(process.join("ns/mnt")).ok()?;
        let namespace = namespace
            .to_str()?
            .strip_prefix("mnt:[")?
            .strip_suffix(']')?;
        let root = fs::metadata(process.join("root")).ok()?;
        Some(ProcessIdentity {
            pid,
            start_ticks,
            mount_namespace: namespace.parse().ok()?,
            root_device: root.dev(),
            root_inode: root.ino(),
        })
    }

    pub(crate) fn context_matches(identity: ProcessIdentity, context: PathContext) -> bool {
        if context
            .mount_namespace
            .is_some_and(|namespace| namespace != identity.mount_namespace)
        {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if hz <= 0 {
                return false;
            }
            let tick_ns = 1_000_000_000u64 / hz as u64;
            let start_ns = identity.start_ticks.saturating_mul(tick_ns);
            let boot_ns = clock_ns(libc::CLOCK_BOOTTIME);
            let observed_boot_ns = if let Some(at) = context.monotonic_ns {
                let now = clock_ns(libc::CLOCK_MONOTONIC);
                boot_ns.saturating_sub(now.saturating_sub(at))
            } else if let Some(at) = context.observed_at {
                let age = (Utc::now() - at)
                    .num_nanoseconds()
                    .unwrap_or(i64::MAX)
                    .max(0) as u64;
                boot_ns.saturating_sub(age)
            } else {
                boot_ns
            };
            // A process born after the event cannot supply its namespace mapping.
            if observed_boot_ns.saturating_add(tick_ns) < start_ns {
                return false;
            }
        }
        true
    }

    #[cfg(target_os = "linux")]
    fn clock_ns(clock: libc::clockid_t) -> u64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(clock, &mut time) } != 0 {
            return 0;
        }
        (time.tv_sec as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(time.tv_nsec as u64)
    }

    #[derive(Clone, Debug)]
    struct Mount {
        device: String,
        root: PathBuf,
        point: PathBuf,
    }

    pub(super) struct CachedMounts {
        key: PathBuf,
        pub identity: Option<ProcessIdentity>,
        text: String,
        mounts: Vec<Mount>,
        used: Instant,
    }

    fn mount_text(path: &Path) -> Result<String, PathResolutionError> {
        let mut text = String::new();
        fs::File::open(path)
            .and_then(|file| file.take(MAX_MOUNTINFO_BYTES + 1).read_to_string(&mut text))
            .map_err(|_| PathResolutionError::Unavailable)?;
        if text.len() as u64 > MAX_MOUNTINFO_BYTES {
            return Err(PathResolutionError::Unavailable);
        }
        Ok(text)
    }

    fn mounts(
        cache: &mut VecDeque<CachedMounts>,
        key: PathBuf,
        identity: Option<ProcessIdentity>,
    ) -> Result<(String, Vec<Mount>), PathResolutionError> {
        let text = mount_text(&key)?;
        cache.retain(|entry| entry.used.elapsed() < CACHE_TTL);
        if let Some(entry) = cache
            .iter_mut()
            .find(|entry| entry.key == key && entry.identity == identity && entry.text == text)
        {
            entry.used = Instant::now();
            return Ok((text, entry.mounts.clone()));
        }
        cache.retain(|entry| entry.key != key);
        let parsed = parse_mountinfo(&text)?;
        if cache.len() >= MAX_CACHED_TABLES {
            cache.pop_front();
        }
        cache.push_back(CachedMounts {
            key,
            identity,
            text: text.clone(),
            mounts: parsed.clone(),
            used: Instant::now(),
        });
        Ok((text, parsed))
    }

    fn parse_mountinfo(text: &str) -> Result<Vec<Mount>, PathResolutionError> {
        let mut mounts = Vec::new();
        for line in text.lines() {
            if mounts.len() == MAX_MOUNTS {
                return Err(PathResolutionError::Unavailable);
            }
            let (fields, _) = line
                .split_once(" - ")
                .ok_or(PathResolutionError::Unavailable)?;
            let fields = fields.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 6 {
                return Err(PathResolutionError::Unavailable);
            }
            let mount = Mount {
                device: fields[2].to_owned(),
                root: unescape_path(fields[3])?,
                point: unescape_path(fields[4])?,
            };
            if !mount.root.is_absolute()
                || !mount.point.is_absolute()
                || mount
                    .root
                    .components()
                    .chain(mount.point.components())
                    .any(|part| matches!(part, Component::ParentDir))
            {
                return Err(PathResolutionError::InvalidPath);
            }
            mounts.push(mount);
        }
        Ok(mounts)
    }

    fn unescape_path(text: &str) -> Result<PathBuf, PathResolutionError> {
        let mut bytes = Vec::new();
        let mut remaining = text.as_bytes();
        while !remaining.is_empty() {
            if remaining[0] == b'\\' {
                if remaining.len() < 4 || !remaining[1..4].iter().all(|b| (b'0'..=b'7').contains(b))
                {
                    return Err(PathResolutionError::InvalidPath);
                }
                let value = ((remaining[1] - b'0') as u16 * 64)
                    + ((remaining[2] - b'0') as u16 * 8)
                    + (remaining[3] - b'0') as u16;
                if value == 0 || value > 255 {
                    return Err(PathResolutionError::InvalidPath);
                }
                bytes.push(value as u8);
                remaining = &remaining[4..];
            } else {
                bytes.push(remaining[0]);
                remaining = &remaining[1..];
            }
        }
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }

    fn candidates(
        path: &Path,
        process: &[Mount],
        host: &[Mount],
    ) -> Result<Vec<PathBuf>, PathResolutionError> {
        let depth = process
            .iter()
            .filter(|mount| path.starts_with(&mount.point))
            .map(|mount| mount.point.components().count())
            .max()
            .ok_or(PathResolutionError::Unavailable)?;
        let covering = process
            .iter()
            .filter(|mount| {
                path.starts_with(&mount.point) && mount.point.components().count() == depth
            })
            .collect::<Vec<_>>();
        // Stacked mounts cannot be selected by a mount ID's numeric ordering.
        if covering.len() != 1 {
            return Err(PathResolutionError::Ambiguous);
        }
        let mount = covering[0];
        let filesystem_path = mount.root.join(path.strip_prefix(&mount.point).unwrap());
        let mut found = Vec::new();
        for visible in host.iter().filter(|visible| visible.device == mount.device) {
            if let Ok(relative) = filesystem_path.strip_prefix(&visible.root) {
                let candidate = visible.point.join(relative);
                if valid_absolute(&candidate) {
                    found.push(candidate);
                }
            }
        }
        found.sort();
        found.dedup();
        Ok(found)
    }

    fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino() && a.file_type() == b.file_type()
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn recover_open_file(
        resolver: &mut HostPathResolver,
        pid: i32,
        path: &Path,
        descriptor: &fs::Metadata,
        namespace: Option<u64>,
    ) -> Result<PathBuf, PathResolutionError> {
        if !descriptor.is_file() || path.as_os_str().as_encoded_bytes().ends_with(b" (deleted)") {
            return Err(PathResolutionError::Unavailable);
        }
        let path = resolver.resolve(
            pid,
            path,
            PathContext {
                mount_namespace: namespace,
                inode: Some(descriptor.ino()),
                device: Some(descriptor.dev()),
                ..PathContext::default()
            },
        )?;
        if !fs::metadata(&path).is_ok_and(|host| same_file(&host, descriptor)) {
            return Err(PathResolutionError::IdentityMismatch);
        }
        Ok(path)
    }

    /// Refuse symlinks even when the leaf no longer exists. canonicalize(leaf)
    /// followed by a lexical fallback would allow an escaping symlink ancestor.
    fn checked_candidate(path: &Path, home: &Path) -> Result<PathBuf, PathResolutionError> {
        let relative = path
            .strip_prefix(home)
            .map_err(|_| PathResolutionError::OutsideRuntimeHome)?;
        if fs::symlink_metadata(home)
            .map_err(|_| PathResolutionError::Unavailable)?
            .file_type()
            .is_symlink()
        {
            return Err(PathResolutionError::OutsideRuntimeHome);
        }
        let mut checked = fs::canonicalize(home).map_err(|_| PathResolutionError::Unavailable)?;
        for component in relative.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(PathResolutionError::InvalidPath);
            }
            checked.push(component);
            match fs::symlink_metadata(&checked) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(PathResolutionError::OutsideRuntimeHome)
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(PathResolutionError::Unavailable),
            }
        }
        Ok(checked)
    }

    pub(super) fn check_host_path(
        path: &Path,
        home: &Path,
        context: PathContext,
    ) -> Result<PathBuf, PathResolutionError> {
        let checked = checked_candidate(path, home)?;
        let expected = if context.parent_identity {
            checked.parent().unwrap_or(&checked)
        } else {
            &checked
        };
        match fs::symlink_metadata(expected) {
            Ok(meta)
                if context.inode.is_some_and(|inode| inode != meta.ino())
                    || context.device.is_some_and(|device| device != meta.dev()) =>
            {
                Err(PathResolutionError::IdentityMismatch)
            }
            Ok(_) => Ok(checked),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && (context.allow_missing || context.parent_identity) =>
            {
                Ok(checked)
            }
            Err(_) => Err(PathResolutionError::Unavailable),
        }
    }

    fn verify_path(
        namespace: &Path,
        host: &Path,
        home: &Path,
        context: PathContext,
    ) -> Result<(), PathResolutionError> {
        let mut namespace = namespace.to_path_buf();
        let mut host = host.to_path_buf();
        let expected = if context.parent_identity {
            host.parent()
        } else {
            Some(host.as_path())
        }
        .map(Path::to_path_buf);
        loop {
            match (fs::metadata(&namespace), fs::symlink_metadata(&host)) {
                (Ok(a), Ok(b)) => {
                    if !same_file(&a, &b) || a.file_type().is_symlink() {
                        return Err(PathResolutionError::IdentityMismatch);
                    }
                    if expected.as_deref() == Some(host.as_path())
                        && (context.inode.is_some_and(|inode| inode != b.ino())
                            || context.device.is_some_and(|device| device != b.dev()))
                    {
                        return Err(PathResolutionError::IdentityMismatch);
                    }
                    if !context.parent_identity
                        || expected.as_deref() == Some(host.as_path())
                        || host == home
                    {
                        return Ok(());
                    }
                }
                (Err(a), Err(b))
                    if a.kind() == std::io::ErrorKind::NotFound
                        && b.kind() == std::io::ErrorKind::NotFound
                        && (context.allow_missing || context.parent_identity) => {}
                _ => return Err(PathResolutionError::IdentityMismatch),
            }
            if host == home || !host.pop() || !namespace.pop() {
                return Err(PathResolutionError::Unavailable);
            }
        }
    }

    impl HostPathResolver {
        pub(super) fn resolve_runtime(
            &mut self,
            pid: i32,
            path: &Path,
            context: PathContext,
        ) -> Result<PathBuf, PathResolutionError> {
            let identity =
                process_identity(&self.proc_root, pid).ok_or(PathResolutionError::Unavailable)?;
            if !context_matches(identity, context) {
                self.invalidate_pid(pid);
                return Err(PathResolutionError::ProcessChanged);
            }
            let process = self.proc_root.join(pid.to_string());
            let process_mountinfo = process.join("mountinfo");
            let host_mountinfo = self.proc_root.join("self/mountinfo");
            let (process_text, process_mounts) =
                mounts(&mut self.cache, process_mountinfo.clone(), Some(identity))?;
            let (host_text, host_mounts) = mounts(&mut self.cache, host_mountinfo.clone(), None)?;
            let namespace_home = process.join("root/home/runtime");
            let home_identity =
                fs::metadata(&namespace_home).map_err(|_| PathResolutionError::Unavailable)?;
            let mut homes = Vec::new();
            for candidate in candidates(Path::new(RUNTIME_HOME), &process_mounts, &host_mounts)? {
                // Require the actual per-app home, not a scan for a matching leaf.
                if runtime_private_home(&candidate).as_deref() != Some(candidate.as_path()) {
                    continue;
                }
                let Ok(checked) = checked_candidate(&candidate, &candidate) else {
                    continue;
                };
                if fs::metadata(&checked).is_ok_and(|meta| same_file(&home_identity, &meta)) {
                    homes.push(checked);
                }
            }
            homes.sort();
            homes.dedup();
            let home = match homes.as_slice() {
                [home] => home,
                [] => return Err(PathResolutionError::Unavailable),
                _ => return Err(PathResolutionError::Ambiguous),
            };
            let namespace_path = process.join("root").join(path.strip_prefix("/").unwrap());
            let mut resolved = Vec::new();
            for candidate in candidates(path, &process_mounts, &host_mounts)? {
                // Canonicalize only the private-home prefix; never a missing leaf.
                let Some(candidate_home) = runtime_private_home(&candidate) else {
                    continue;
                };
                let Ok(canonical_home) = fs::canonicalize(&candidate_home) else {
                    continue;
                };
                if canonical_home != *home {
                    continue;
                }
                let Ok(checked) = checked_candidate(&candidate, &candidate_home) else {
                    continue;
                };
                if verify_path(&namespace_path, &checked, home, context).is_ok() {
                    resolved.push(checked);
                }
            }
            resolved.sort();
            resolved.dedup();
            let resolved = match resolved.as_slice() {
                [path] => path.clone(),
                [] => return Err(PathResolutionError::IdentityMismatch),
                _ => return Err(PathResolutionError::Ambiguous),
            };
            // Remounts need not change the namespace inode. A process can also
            // exit/reuse its PID during the lookup; do not publish either race.
            if process_identity(&self.proc_root, pid) != Some(identity)
                || mount_text(&process_mountinfo)? != process_text
                || mount_text(&host_mountinfo)? != host_text
            {
                self.invalidate_pid(pid);
                return Err(PathResolutionError::ProcessChanged);
            }
            Ok(resolved)
        }
    }
}
