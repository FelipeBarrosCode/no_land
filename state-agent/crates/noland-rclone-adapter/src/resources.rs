//! Shared, credential-free CPU/memory/IO admission estimates.

use crate::TransferWorkload;

const MIB: u64 = 1024 * 1024;
const UNKNOWN_MEMORY_BYTES: u64 = 512 * MIB;
// Emergency protection against an implausible host report or unbounded thread
// creation, not a normal app throughput cap. Ordinary hosts scale with CPUs.
const EMERGENCY_CPU_WORKERS: usize = 128;

/// Raw resources on the machine performing an upload or download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerResources {
    /// Effective CPUs, accounting for OS affinity/quota when available.
    pub logical_cpus: usize,
    /// Raw currently available bytes, before any admission/headroom partition.
    pub available_memory: Option<u64>,
    /// Linux IO PSI `some avg10 >= 10%`, or a caller-supplied pressure signal.
    pub io_pressure: bool,
}

impl WorkerResources {
    /// Read only OS resource information, never application state or credentials.
    pub fn detect() -> Self {
        Self {
            logical_cpus: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            available_memory: available_memory_bytes(),
            io_pressure: io_pressure(),
        }
    }

    /// A quarter of raw availability for each workspace/buffer admission class.
    ///
    /// The normal reader/codec/transfer classes together leave a quarter for the
    /// coordinator and other allocations. Callers must account for coexisting
    /// workspaces in their per-worker costs, rather than creating extra pools
    /// from the same allowance. Unknown memory uses a finite 512 MiB raw
    /// estimate (128 MiB per class). Do not halve memory before calling helpers.
    ///
    /// This is an admission estimate, not a guarantee of total process RSS.
    pub fn memory_budget_bytes(&self) -> u64 {
        self.available_memory.unwrap_or(UNKNOWN_MEMORY_BYTES) / 4
    }

    /// Pure CPU-worker admission ceiling for a workspace class.
    ///
    /// Throughput uses all effective CPUs, Balanced half (rounded up), and
    /// GameplaySafe a quarter (rounded up). The result is bounded by the
    /// quarter-memory class budget divided by the caller's truthful per-worker
    /// cost, and a high emergency guard of 128 workers. IO pressure halves the
    /// resulting admission proportionally, including memory-limited hosts.
    ///
    /// At least one worker is returned even below its estimated memory cost;
    /// stage-specific progress floors and overlapping stages belong to callers.
    /// A zero cost is treated as one byte. No OS probing occurs in this helper.
    pub fn cpu_workers(&self, workload: TransferWorkload, memory_per_worker: u64) -> usize {
        let cpus = self.logical_cpus.max(1);
        let cpu_workers = match workload {
            TransferWorkload::Throughput => cpus,
            TransferWorkload::Balanced => cpus.div_ceil(2),
            TransferWorkload::GameplaySafe => cpus.div_ceil(4),
        }
        .min(EMERGENCY_CPU_WORKERS);
        let memory_workers = usize::try_from(self.memory_budget_bytes() / memory_per_worker.max(1))
            .unwrap_or(usize::MAX);
        let workers = cpu_workers.min(memory_workers).max(1);
        if self.io_pressure {
            workers.div_ceil(2)
        } else {
            workers
        }
    }
}

pub(crate) fn available_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux::available_memory_with(&|path| std::fs::read_to_string(path))
    }
    #[cfg(target_os = "macos")]
    {
        macos::available_memory_bytes()
    }
    #[cfg(target_os = "windows")]
    {
        windows::available_memory_bytes()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        // Never mistake installed physical memory for currently available RAM.
        // Other platforms use the finite fallback until a safe probe is added.
        None
    }
}

fn io_pressure() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/pressure/io")
            .ok()
            .is_some_and(|text| pressured_io(&text))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(any(target_os = "linux", test))]
fn pressured_io(text: &str) -> bool {
    text.lines()
        .find(|line| line.starts_with("some "))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix("avg10="))
        })
        .and_then(|value| value.parse::<f64>().ok())
        .is_some_and(|value| value >= 10.0)
}

#[cfg(any(target_os = "macos", test))]
mod macos {
    #[cfg(target_os = "macos")]
    pub(super) fn available_memory_bytes() -> Option<u64> {
        // One snapshot from the system utility, not a shell or an interval
        // sampler. Fix the executable and locale; never inspect application
        // state, process arguments or credentials.
        let output = std::process::Command::new("/usr/bin/vm_stat")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_vm_stat(std::str::from_utf8(&output.stdout).ok()?)
    }

    fn parse_vm_stat(text: &str) -> Option<u64> {
        let mut lines = text.lines();
        let page_size = lines
            .next()?
            .trim()
            .strip_prefix("Mach Virtual Memory Statistics: (page size of ")?
            .strip_suffix(" bytes)")?
            .parse::<u64>()
            .ok()?;
        if !page_size.is_power_of_two() || !(1024..=1024 * 1024).contains(&page_size) {
            return None;
        }

        let mut free = None;
        let mut inactive = None;
        let mut speculative = None;
        for line in lines {
            let Some((name, count)) = line.split_once(':') else {
                continue;
            };
            let slot = match name.trim() {
                "Pages free" => &mut free,
                "Pages inactive" => &mut inactive,
                "Pages speculative" => &mut speculative,
                // Purgeable is a subset of other queues, not another pool.
                // Active, wired and compressor pages are not free headroom.
                _ => continue,
            };
            if slot.is_some() {
                return None;
            }
            let count = count.trim();
            *slot = Some(
                count
                    .strip_suffix('.')
                    .unwrap_or(count)
                    .parse::<u64>()
                    .ok()?,
            );
        }
        // vm_stat reports speculative pages separately from its "Pages free"
        // count. Inactive pages are reclaimable, not necessarily already free;
        // callers still apply the shared quarter-memory/headroom partition.
        free?
            .checked_add(inactive?)?
            .checked_add(speculative?)?
            .checked_mul(page_size)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_intel_page_size_and_excludes_overlapping_or_used_queues() {
            assert_eq!(
                parse_vm_stat(
                    "Mach Virtual Memory Statistics: (page size of 4096 bytes)\n\
                     Pages free: 1024.\n\
                     Pages active: 999999.\n\
                     Pages inactive: 4096.\n\
                     Pages speculative: 256.\n\
                     Pages wired down: 999999.\n\
                     Pages purgeable: 999999.\n\
                     Pages stored in compressor: 999999.\n\
                     Pages occupied by compressor: 999999.\n"
                ),
                Some(21 * super::super::MIB)
            );
        }

        #[test]
        fn parses_apple_silicon_page_size_and_whitespace() {
            assert_eq!(
                parse_vm_stat(
                    "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                     Pages speculative:                  256.\n\
                     Pages inactive:                    4096.\n\
                     Pages free:                        1024.\n\
                     Pages purgeable:                   2000.\n"
                ),
                Some(84 * super::super::MIB)
            );
        }

        #[test]
        fn zero_available_pages_is_not_unknown_or_installed_memory() {
            assert_eq!(
                parse_vm_stat(
                    "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                     Pages free: 0.\nPages inactive: 0.\nPages speculative: 0.\n\
                     Pages active: 4194304.\nPages wired down: 4194304.\n"
                ),
                Some(0)
            );
        }

        #[test]
        fn malformed_incomplete_and_duplicate_snapshots_remain_unknown() {
            let header = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\n";
            for body in [
                "",
                "Pages free: 1.\nPages inactive: 2.\n",
                "Pages free: 1.\nPages inactive: invalid.\nPages speculative: 3.\n",
                "Pages free: -1.\nPages inactive: 2.\nPages speculative: 3.\n",
                "Pages free: 1.\nPages inactive: 2.\nPages speculative: 3.\nPages free: 4.\n",
            ] {
                assert_eq!(parse_vm_stat(&format!("{header}{body}")), None);
            }
            for header in [
                "",
                "unrecognized header\n",
                "Mach Virtual Memory Statistics: (page size of 0 bytes)\n",
                "Mach Virtual Memory Statistics: (page size of 4097 bytes)\n",
                "Mach Virtual Memory Statistics: (page size of 18446744073709551615 bytes)\n",
            ] {
                assert_eq!(
                    parse_vm_stat(&format!(
                        "{header}Pages free: 1.\nPages inactive: 2.\nPages speculative: 3.\n"
                    )),
                    None
                );
            }
        }

        #[test]
        fn page_sum_and_byte_conversion_overflow_remain_unknown() {
            for counts in [
                "Pages free: 18446744073709551615.\nPages inactive: 1.\nPages speculative: 1.\n",
                "Pages free: 18446744073709551615.\nPages inactive: 0.\nPages speculative: 0.\n",
            ] {
                assert_eq!(
                    parse_vm_stat(&format!(
                        "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n{counts}"
                    )),
                    None
                );
            }
        }
    }
}

#[cfg(any(target_os = "windows", test))]
mod windows {
    /// Win32 MEMORYSTATUSEX: two DWORDs followed by seven DWORDLONGs. This
    /// fixed-size layout contains no architecture-dependent pointers or usize.
    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    const _: [(); 64] = [(); std::mem::size_of::<MemoryStatusEx>()];
    #[cfg(target_os = "windows")]
    const _: [(); 8] = [(); std::mem::align_of::<MemoryStatusEx>()];

    #[cfg(target_os = "windows")]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GlobalMemoryStatusEx"]
        fn global_memory_status_ex(buffer: *mut MemoryStatusEx) -> i32;
    }

    #[cfg(target_os = "windows")]
    pub(super) fn available_memory_bytes() -> Option<u64> {
        let mut status = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            ..MemoryStatusEx::default()
        };
        // SAFETY: status is a valid, aligned, writable MEMORYSTATUSEX of the
        // asserted Win32 ABI size, and dwLength is initialized as required.
        // The API only writes this record; it changes no OS resource settings.
        let succeeded = unsafe { global_memory_status_ex(&mut status) } != 0;
        if succeeded {
            available_physical(&status)
        } else {
            None
        }
    }

    fn available_physical(status: &MemoryStatusEx) -> Option<u64> {
        (status.length as usize == std::mem::size_of::<MemoryStatusEx>()
            && status.total_phys > 0
            && status.avail_phys <= status.total_phys)
            .then_some(status.avail_phys)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn memory_status_layout_matches_the_fixed_win32_abi() {
            assert_eq!(std::mem::size_of::<MemoryStatusEx>(), 64);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, length), 0);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, memory_load), 4);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, total_phys), 8);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, avail_phys), 16);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, total_page_file), 24);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, avail_page_file), 32);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, total_virtual), 40);
            assert_eq!(std::mem::offset_of!(MemoryStatusEx, avail_virtual), 48);
            assert_eq!(
                std::mem::offset_of!(MemoryStatusEx, avail_extended_virtual),
                56
            );
        }

        #[test]
        fn uses_available_physical_not_installed_ram_pagefile_or_virtual_memory() {
            let mut status = MemoryStatusEx {
                length: 64,
                total_phys: 64 * 1024 * super::super::MIB,
                avail_phys: 3 * 1024 * super::super::MIB,
                total_page_file: u64::MAX,
                avail_page_file: u64::MAX,
                total_virtual: u64::MAX,
                avail_virtual: u64::MAX,
                ..MemoryStatusEx::default()
            };
            assert_eq!(
                available_physical(&status),
                Some(3 * 1024 * super::super::MIB)
            );
            status.avail_phys = 0;
            assert_eq!(available_physical(&status), Some(0));
        }

        #[test]
        fn invalid_abi_length_and_inconsistent_reports_remain_unknown() {
            assert_eq!(available_physical(&MemoryStatusEx::default()), None);
            let mut status = MemoryStatusEx {
                length: 64,
                total_phys: 1,
                avail_phys: u64::MAX,
                ..MemoryStatusEx::default()
            };
            assert_eq!(available_physical(&status), None);
            status.avail_phys = 1;
            status.length = 63;
            assert_eq!(available_physical(&status), None);
        }
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux {
    use std::io;
    use std::path::{Component, Path, PathBuf};

    use super::UNKNOWN_MEMORY_BYTES;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Hierarchy {
        Unified,
        MemoryV1,
    }

    struct Mount {
        hierarchy: Hierarchy,
        root: PathBuf,
        point: PathBuf,
    }

    #[derive(Default)]
    struct Ceiling {
        available: Option<u64>,
        uncertain: bool,
    }

    /// Resolve this process's nested cgroups, then take the minimum remaining
    /// memory across every visible ancestor and the host's MemAvailable. Parent
    /// usage includes siblings, so subtract each parent's own current usage.
    pub(super) fn available_memory_with(
        read: &impl Fn(&Path) -> io::Result<String>,
    ) -> Option<u64> {
        let host = read(Path::new("/proc/meminfo"))
            .ok()
            .and_then(|text| host_available(&text));
        let cgroup = cgroup_ceiling(read);
        let available = minimum(host, cgroup.available);
        if cgroup.uncertain {
            // An unresolved container hierarchy is not permission to use host
            // totals. Retain any tighter known bound and cap uncertainty at the
            // same finite fallback used when all memory probing is unavailable.
            available.map(|bytes| bytes.min(UNKNOWN_MEMORY_BYTES))
        } else {
            available
        }
    }

    fn host_available(text: &str) -> Option<u64> {
        text.lines().find_map(|line| {
            let mut fields = line.strip_prefix("MemAvailable:")?.split_whitespace();
            let kib = fields.next()?.parse::<u64>().ok()?;
            if fields.next()? != "kB" {
                return None;
            }
            kib.checked_mul(1024)
        })
    }

    fn cgroup_ceiling(read: &impl Fn(&Path) -> io::Result<String>) -> Ceiling {
        let Some(memberships) = read(Path::new("/proc/self/cgroup"))
            .ok()
            .and_then(|text| memberships(&text))
        else {
            return Ceiling {
                uncertain: true,
                ..Ceiling::default()
            };
        };
        if memberships.is_empty() {
            return Ceiling::default();
        }
        let mounts = read(Path::new("/proc/self/mountinfo"))
            .ok()
            .map(|text| mounts(&text))
            .unwrap_or_default();
        let mut ceiling = Ceiling::default();
        for (hierarchy, membership) in memberships {
            let matching: Vec<_> = mounts
                .iter()
                .filter(|mount| mount.hierarchy == hierarchy)
                .collect();
            if matching.is_empty() {
                // Common layouts still supply useful tighter bounds if proc
                // mount information is unavailable, but remain uncertain.
                ceiling.uncertain = true;
                let fallback = Mount {
                    hierarchy,
                    root: PathBuf::from("/"),
                    point: PathBuf::from(match hierarchy {
                        Hierarchy::Unified => "/sys/fs/cgroup",
                        Hierarchy::MemoryV1 => "/sys/fs/cgroup/memory",
                    }),
                };
                merge(
                    &mut ceiling,
                    hierarchy_ceiling(&fallback, &membership, read),
                );
            } else {
                for mount in matching {
                    merge(&mut ceiling, hierarchy_ceiling(mount, &membership, read));
                }
            }
        }
        ceiling
    }

    fn hierarchy_ceiling(
        mount: &Mount,
        membership: &Path,
        read: &impl Fn(&Path) -> io::Result<String>,
    ) -> Ceiling {
        // mountinfo roots may be a delegated subtree, not "/". Cgroup
        // namespaces instead report membership relative to that subtree.
        let Ok(relative) = membership
            .strip_prefix(&mount.root)
            .or_else(|_| membership.strip_prefix("/"))
        else {
            return Ceiling {
                uncertain: true,
                ..Ceiling::default()
            };
        };
        let leaf = mount.point.join(relative);
        let (limit_file, current_file) = match mount.hierarchy {
            Hierarchy::Unified => ("memory.max", "memory.current"),
            Hierarchy::MemoryV1 => ("memory.limit_in_bytes", "memory.usage_in_bytes"),
        };
        let mut ceiling = Ceiling::default();
        let mut found = false;
        for group in leaf
            .ancestors()
            .take_while(|path| path.starts_with(&mount.point))
        {
            let limit = match read(&group.join(limit_file)) {
                Ok(text) => text,
                // The real v2 root and groups without the memory controller
                // need not expose memory.max; still examine their ancestors.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    ceiling.uncertain = true;
                    continue;
                }
            };
            found = true;
            let limit = limit.trim();
            if mount.hierarchy == Hierarchy::Unified && limit == "max" {
                continue;
            }
            let Ok(limit) = limit.parse::<u64>() else {
                ceiling.uncertain = true;
                continue;
            };
            // v1 uses a huge page-rounded sentinel rather than "max".
            if mount.hierarchy == Hierarchy::MemoryV1 && limit >= (1u64 << 60) {
                continue;
            }
            let remaining = read(&group.join(current_file))
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok())
                .map(|current| limit.saturating_sub(current))
                // A readable limit with unknown usage must not become a host
                // fallback, or even the full limit without subtracting usage.
                .unwrap_or(0);
            ceiling.available = minimum(ceiling.available, Some(remaining));
        }
        if !found && mount.hierarchy == Hierarchy::Unified && mount.root == Path::new("/") {
            // The real unified hierarchy root has no memory.max. A readable
            // root controller interface confirms this is not simply an
            // unavailable/misresolved cgroup mount; it has no ancestor limit.
            // Memory may also be disabled here (e.g. v1/v2 hybrid). Its limits
            // are then supplied by the separately resolved v1 hierarchy.
            found = read(&mount.point.join("cgroup.controllers")).is_ok();
        }
        ceiling.uncertain |= !found;
        ceiling
    }

    fn memberships(text: &str) -> Option<Vec<(Hierarchy, PathBuf)>> {
        let mut memberships = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.splitn(3, ':');
            let id = fields.next()?;
            let controllers = fields.next()?;
            let path = fields.next()?;
            let hierarchy = if id == "0" && controllers.is_empty() {
                Hierarchy::Unified
            } else if controllers
                .split(',')
                .any(|controller| controller == "memory")
            {
                Hierarchy::MemoryV1
            } else {
                continue;
            };
            memberships.push((hierarchy, absolute_path(path)?));
        }
        Some(memberships)
    }

    fn mounts(text: &str) -> Vec<Mount> {
        text.lines()
            .filter_map(|line| {
                let (before, after) = line.split_once(" - ")?;
                let mut after = after.split_whitespace();
                let filesystem = after.next()?;
                after.next()?; // mount source
                let options = after.next()?;
                let hierarchy = match filesystem {
                    "cgroup2" => Hierarchy::Unified,
                    "cgroup" if options.split(',').any(|option| option == "memory") => {
                        Hierarchy::MemoryV1
                    }
                    _ => return None,
                };
                let mut before = before.split_whitespace().skip(3);
                Some(Mount {
                    hierarchy,
                    root: mount_path(before.next()?)?,
                    point: mount_path(before.next()?)?,
                })
            })
            .collect()
    }

    fn mount_path(text: &str) -> Option<PathBuf> {
        absolute_path(
            &text
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\"),
        )
    }

    fn absolute_path(text: &str) -> Option<PathBuf> {
        let path = Path::new(text);
        (path.is_absolute() && !path.components().any(|part| part == Component::ParentDir))
            .then(|| path.to_path_buf())
    }

    fn merge(target: &mut Ceiling, other: Ceiling) {
        target.available = minimum(target.available, other.available);
        target.uncertain |= other.uncertain;
    }

    fn minimum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(bytes), None) | (None, Some(bytes)) => Some(bytes),
            (None, None) => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;

        use super::*;

        const MIB: u64 = super::super::MIB;
        const GIB: u64 = 1024 * MIB;
        const V2_MOUNT: &str = "29 23 0:26 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n";

        fn available(files: &[(&str, &str)]) -> Option<u64> {
            let files: HashMap<_, _> = files.iter().copied().collect();
            available_memory_with(&|path| {
                files
                    .get(path.to_str().unwrap())
                    .map(|contents| contents.to_string())
                    .ok_or_else(|| io::ErrorKind::NotFound.into())
            })
        }

        #[test]
        fn nested_v2_uses_the_tightest_ancestor_remaining_memory() {
            let files = [
                (
                    "/proc/meminfo",
                    "MemTotal: 67108864 kB\nMemAvailable: 33554432 kB\n",
                ),
                ("/proc/self/cgroup", "0::/tenant/job\n"),
                ("/proc/self/mountinfo", V2_MOUNT),
                ("/sys/fs/cgroup/tenant/job/memory.max", "8589934592"),
                ("/sys/fs/cgroup/tenant/job/memory.current", "1073741824"),
                ("/sys/fs/cgroup/tenant/memory.max", "4294967296"),
                ("/sys/fs/cgroup/tenant/memory.current", "3221225472"),
                ("/sys/fs/cgroup/memory.max", "max"),
            ];
            assert_eq!(available(&files), Some(GIB));
        }

        #[test]
        fn host_available_still_bounds_an_unlimited_cgroup() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 65536 kB\n"),
                    ("/proc/self/cgroup", "0::/\n"),
                    ("/proc/self/mountinfo", V2_MOUNT),
                    ("/sys/fs/cgroup/memory.max", "max"),
                ]),
                Some(64 * MIB)
            );
        }

        #[test]
        fn real_unified_root_without_memory_max_uses_host_availability() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    ("/proc/self/cgroup", "0::/\n"),
                    ("/proc/self/mountinfo", V2_MOUNT),
                    ("/sys/fs/cgroup/cgroup.controllers", "cpu io memory pids\n"),
                ]),
                Some(32 * GIB)
            );
        }

        #[test]
        fn delegated_mount_root_resolves_the_real_process_group() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    ("/proc/self/cgroup", "0::/docker/container/job\n"),
                    (
                        "/proc/self/mountinfo",
                        "29 23 0:26 /docker/container /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
                    ),
                    ("/sys/fs/cgroup/job/memory.max", "268435456"),
                    ("/sys/fs/cgroup/job/memory.current", "67108864"),
                    ("/sys/fs/cgroup/memory.max", "8589934592"),
                    ("/sys/fs/cgroup/memory.current", "1073741824"),
                ]),
                Some(192 * MIB)
            );
        }

        #[test]
        fn cgroup_namespace_membership_is_relative_to_the_mounted_root() {
            assert_eq!(
                available(&[
                    ("/proc/self/cgroup", "0::/job\n"),
                    (
                        "/proc/self/mountinfo",
                        "29 23 0:26 /docker/container /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
                    ),
                    ("/sys/fs/cgroup/job/memory.max", "268435456"),
                    ("/sys/fs/cgroup/job/memory.current", "67108864"),
                    ("/sys/fs/cgroup/memory.max", "max"),
                ]),
                Some(192 * MIB)
            );
        }

        #[test]
        fn nested_v1_walks_ancestors_and_ignores_the_unlimited_sentinel() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    (
                        "/proc/self/cgroup",
                        "5:cpu,cpuacct:/jobs/task\n7:memory:/jobs/task\n"
                    ),
                    (
                        "/proc/self/mountinfo",
                        "29 23 0:26 / /sys/fs/cgroup/memory rw - cgroup memory rw,memory\n",
                    ),
                    (
                        "/sys/fs/cgroup/memory/jobs/task/memory.limit_in_bytes",
                        "8589934592"
                    ),
                    (
                        "/sys/fs/cgroup/memory/jobs/task/memory.usage_in_bytes",
                        "1073741824"
                    ),
                    (
                        "/sys/fs/cgroup/memory/jobs/memory.limit_in_bytes",
                        "2147483648"
                    ),
                    (
                        "/sys/fs/cgroup/memory/jobs/memory.usage_in_bytes",
                        "1879048192"
                    ),
                    (
                        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                        "9223372036854771712"
                    ),
                ]),
                Some(256 * MIB)
            );
        }

        #[test]
        fn hybrid_hierarchy_does_not_mistake_disabled_v2_memory_for_an_unknown_limit() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    ("/proc/self/cgroup", "0::/jobs/task\n7:memory:/jobs/task\n"),
                    (
                        "/proc/self/mountinfo",
                        "29 23 0:26 / /sys/fs/cgroup/unified rw - cgroup2 cgroup rw\n\
                         30 23 0:27 / /sys/fs/cgroup/memory rw - cgroup memory rw,memory\n",
                    ),
                    ("/sys/fs/cgroup/unified/cgroup.controllers", "cpu io pids\n"),
                    (
                        "/sys/fs/cgroup/memory/jobs/task/memory.limit_in_bytes",
                        "8589934592",
                    ),
                    (
                        "/sys/fs/cgroup/memory/jobs/task/memory.usage_in_bytes",
                        "2147483648",
                    ),
                    (
                        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                        "9223372036854771712",
                    ),
                ]),
                Some(6 * GIB)
            );
        }

        #[test]
        fn memory_over_limit_and_unknown_current_usage_do_not_use_host_totals() {
            for current in [Some("536870912"), Some("invalid"), None] {
                let mut files = vec![
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    ("/proc/self/cgroup", "0::/\n"),
                    ("/proc/self/mountinfo", V2_MOUNT),
                    ("/sys/fs/cgroup/memory.max", "268435456"),
                ];
                if let Some(current) = current {
                    files.push(("/sys/fs/cgroup/memory.current", current));
                }
                assert_eq!(available(&files), Some(0));
            }
        }

        #[test]
        fn unresolved_container_information_is_finitely_conservative() {
            assert_eq!(available(&[]), None);
            assert_eq!(
                available(&[("/proc/meminfo", "MemAvailable: 33554432 kB\n")]),
                Some(512 * MIB)
            );
            // No root mount fallback may turn malformed membership into access
            // outside the cgroup tree, or an oversized host-memory allowance.
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    ("/proc/self/cgroup", "0::/../../outside\n"),
                    ("/proc/self/mountinfo", V2_MOUNT),
                ]),
                Some(512 * MIB)
            );
        }

        #[test]
        fn missing_mountinfo_keeps_a_tighter_root_limit() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 33554432 kB\n"),
                    ("/proc/self/cgroup", "0::/\n"),
                    ("/sys/fs/cgroup/memory.max", "268435456"),
                    ("/sys/fs/cgroup/memory.current", "67108864"),
                ]),
                Some(192 * MIB)
            );
        }

        #[test]
        fn unreadable_nested_limit_never_inherits_an_oversized_host_allowance() {
            assert_eq!(
                available_memory_with(&|path| {
                    match path.to_str().unwrap() {
                        "/proc/meminfo" => Ok("MemAvailable: 33554432 kB\n".into()),
                        "/proc/self/cgroup" => Ok("0::/tenant/job\n".into()),
                        "/proc/self/mountinfo" => Ok(V2_MOUNT.into()),
                        "/sys/fs/cgroup/tenant/job/memory.max" => {
                            Err(io::ErrorKind::PermissionDenied.into())
                        }
                        "/sys/fs/cgroup/memory.max" => Ok("max".into()),
                        _ => Err(io::ErrorKind::NotFound.into()),
                    }
                }),
                Some(512 * MIB)
            );
        }

        #[test]
        fn mountinfo_escapes_and_host_meminfo_overflow_are_safe() {
            assert_eq!(
                available(&[
                    ("/proc/meminfo", "MemAvailable: 18446744073709551615 kB\n"),
                    ("/proc/self/cgroup", "0::/\n"),
                    (
                        "/proc/self/mountinfo",
                        "29 23 0:26 / /sys/fs/cgroup\\040memory rw - cgroup2 cgroup rw\n",
                    ),
                    ("/sys/fs/cgroup memory/memory.max", "268435456"),
                    ("/sys/fs/cgroup memory/memory.current", "67108864"),
                ]),
                Some(192 * MIB)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    #[test]
    fn cpu_admission_scales_past_old_app_caps_in_each_workload() {
        for (cpus, throughput, balanced, gameplay) in [
            (0, 1, 1, 1),
            (1, 1, 1, 1),
            (3, 3, 2, 1),
            (4, 4, 2, 1),
            (5, 5, 3, 2),
            (16, 16, 8, 4),
            (24, 24, 12, 6),
            (32, 32, 16, 8),
            (64, 64, 32, 16),
            (128, 128, 64, 32),
            (512, 128, 128, 128),
        ] {
            let resources = WorkerResources {
                logical_cpus: cpus,
                available_memory: Some(64 * GIB),
                io_pressure: false,
            };
            for (workload, expected) in [
                (TransferWorkload::Throughput, throughput),
                (TransferWorkload::Balanced, balanced),
                (TransferWorkload::GameplaySafe, gameplay),
            ] {
                assert_eq!(resources.cpu_workers(workload, 96 * MIB), expected);
            }
        }
    }

    #[test]
    fn class_memory_partition_and_unknown_fallback_are_consistent() {
        for (available_memory, cost, expected) in [
            (None, 128 * MIB, 1),
            (None, 32 * MIB, 4),
            (Some(512 * MIB), 32 * MIB, 4),
            (Some(GIB), 128 * MIB, 2),
            (Some(64 * MIB), 96 * MIB, 1),
            (Some(0), 96 * MIB, 1),
        ] {
            let resources = WorkerResources {
                logical_cpus: 64,
                available_memory,
                io_pressure: false,
            };
            assert_eq!(
                resources.cpu_workers(TransferWorkload::Throughput, cost),
                expected
            );
        }
    }

    #[test]
    fn io_pressure_halves_admission_even_with_gameplay_or_memory_pressure() {
        for (memory, workload, expected) in [
            (Some(64 * GIB), TransferWorkload::Throughput, 32),
            (Some(64 * GIB), TransferWorkload::Balanced, 16),
            (Some(64 * GIB), TransferWorkload::GameplaySafe, 8),
            (Some(4 * GIB), TransferWorkload::Throughput, 4),
            (Some(4 * GIB), TransferWorkload::GameplaySafe, 4),
            (None, TransferWorkload::Throughput, 1),
        ] {
            let resources = WorkerResources {
                logical_cpus: 64,
                available_memory: memory,
                io_pressure: true,
            };
            assert_eq!(resources.cpu_workers(workload, 128 * MIB), expected);
        }
    }

    #[test]
    fn extreme_inputs_never_overflow_or_admit_unbounded_workers() {
        for logical_cpus in [0, 1, usize::MAX] {
            for available_memory in [None, Some(0), Some(1), Some(u64::MAX)] {
                for memory_per_worker in [0, 1, u64::MAX] {
                    for io_pressure in [false, true] {
                        let resources = WorkerResources {
                            logical_cpus,
                            available_memory,
                            io_pressure,
                        };
                        for workload in [
                            TransferWorkload::Throughput,
                            TransferWorkload::Balanced,
                            TransferWorkload::GameplaySafe,
                        ] {
                            let ceiling = resources.cpu_workers(workload, memory_per_worker);
                            assert!((1..=128).contains(&ceiling));
                            if io_pressure {
                                assert!(ceiling <= 64);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn io_probe_only_uses_the_recent_some_pressure_signal() {
        assert!(!pressured_io("some avg10=9.99 avg60=50.0 total=500\n"));
        assert!(pressured_io("some avg10=10.00 avg60=0.0 total=500\n"));
        assert!(!pressured_io("full avg10=90.0\n"));
        assert!(!pressured_io("some avg10=NaN\n"));
        assert!(!pressured_io("invalid"));
    }
}
