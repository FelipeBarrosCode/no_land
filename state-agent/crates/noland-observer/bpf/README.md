# Noland eBPF observer attachment contract

`noland_observer.bpf.c` is a libbpf CO-RE object. It uses tracing trampolines and raw tracepoints only; it contains no BPF LSM programs and has no boot `lsm=` dependency.

## ABI and transport

- One `BPF_MAP_TYPE_RINGBUF` carries process and filesystem records in sequence order.
- `noland_event_v1` is explicitly versioned and size-delimited.
- A per-CPU scratch map permits `PATH_MAX`-sized paths without placing large buffers on the BPF stack.
- `sequence` orders records and `accumulated_count` reports coalesced access evidence.
- Rust parses fields individually rather than casting unaligned ring-buffer bytes.

## Maps

- `events`: 128 MiB ring buffer. The larger buffer absorbs discovery bursts while the v1 ABI still uses fixed-size records.
- `config`: event classes, default observation mode, sampling, and discovery/steady/write coalescing windows.
- `ignored_tgids`, `ignored_cgroups`, `ignored_files`: feedback-loop exclusions.
- `cgroup_mode`: per-cgroup `NONE`, `DISCOVERY`, or `STEADY` mode.
- `coalesce`: per-cgroup/device/inode/operation LRU state.
- `pending_create`: short-lived `security_inode_create` to `security_file_open` correlation.
- `pending_open`: short-lived `sys_enter_open*` to `security_file_open` correlation. The captured user path seeds userspace canonical resolution for all later `(device, inode)` anchors.
- `pending_open_scratch`: per-CPU scratch space for capturing open paths.
- `sequence_counter`: global event sequence.
- `scratch_events`: per-CPU event/path construction space.
- `stats`: per-CPU loss, filtering, coalescing, and path-resolution counters.

## Hooks

Process:

- `raw_tracepoint/sched_process_fork`
- `raw_tracepoint/sched_process_exec`
- `raw_tracepoint/sched_process_exit`

Successful I/O:

- `fexit/vfs_read`
- `fexit/vfs_write`
- `fexit/vfs_iter_read`
- `fexit/vfs_iter_write`

Open correlation (no path reconstruction inside BPF):

- `tracepoint/syscalls/sys_enter_open`
- `tracepoint/syscalls/sys_enter_openat`
- `tracepoint/syscalls/sys_enter_openat2`
- `tracepoint/syscalls/sys_exit_open`
- `tracepoint/syscalls/sys_exit_openat`
- `tracepoint/syscalls/sys_exit_openat2`

Entry tracepoints capture the user-supplied path before the resolved `struct file`
exists. `security_file_open` consumes the pending path and emits it with the
open fact so userspace can resolve it relative to the process cwd/dirfd and cache
that canonical path for the file's device/inode. Exit tracepoints remove any
correlation left behind by denied opens or kernels lacking the open trampoline.

Resolved filesystem facts:

- `fexit/security_file_open`
- `fexit/security_mmap_file`
- `fexit/security_file_permission` (fallback only when primary I/O hooks are incomplete)
- `fexit/security_inode_create`
- `fexit/security_path_mknod`
- `fexit/security_file_truncate` or `fexit/security_path_truncate`
- `fexit/security_path_rename`
- `fexit/security_path_unlink`
- `fexit/security_path_mkdir`
- `fexit/security_path_rmdir`
- `fexit/security_path_symlink`
- `fexit/security_path_chmod`
- `fexit/security_path_chown`

The loader probes every program independently and disables unsupported sections before loading the final object. When both truncate variants exist, `security_file_truncate` is preferred to avoid duplicate facts. `security_file_permission` remains disabled when both primary read and write trampoline families are available, preventing duplicate access evidence.

Security-wrapper programs are `fexit` tracing programs so denied operations (`result < 0`) can be discarded. Every BPF program returns zero; none can authorize or deny kernel operations.

## Path resolution

`bpf_d_path()` is restricted to a small kernel BTF allowlist and is rejected from ordinary `fentry`/`fexit` probes on several supported kernels, so the BPF object never calls it.

Instead every filesystem fact carries a stable `(device, inode)` anchor encoded in
userspace `stat(2)` form. The Rust consumer resolves anchors to paths through
`/proc/<tgid>/fd`, `/proc/<tgid>/cwd`, and `/proc/<tgid>/root`, caches the
result, and reuses it for subsequent reads, writes, and mmaps. Open facts seeded
by the `sys_enter_open*` tracepoints are resolved relative to `AT_FDCWD` or the
supplied directory fd, keeping resolution working even when the newly opened
descriptor closes before the ring consumer runs.

The observer emits kernel facts only. AppSession correlation, baseline/package lookup, semantic classification, persistence policy, and backup decisions remain in Rust.
