# Noland eBPF observer attachment contract

`noland_observer.bpf.c` is a libbpf CO-RE object. It uses tracing trampolines and raw tracepoints only; it contains no BPF LSM programs and has no boot `lsm=` dependency.

## ABI and transport

- One `BPF_MAP_TYPE_RINGBUF` carries process and filesystem records in sequence order.
- `noland_event_v1` is explicitly versioned and size-delimited.
- A per-CPU scratch map permits `PATH_MAX`-sized paths without placing large buffers on the BPF stack.
- `sequence` orders records and `accumulated_count` reports coalesced access evidence.
- Rust parses fields individually rather than casting unaligned ring-buffer bytes.

## Maps

- `events`: 16 MiB ring buffer.
- `config`: event classes, default observation mode, sampling, and discovery/steady/write coalescing windows.
- `ignored_tgids`, `ignored_cgroups`, `ignored_files`: feedback-loop exclusions.
- `cgroup_mode`: per-cgroup `NONE`, `DISCOVERY`, or `STEADY` mode.
- `coalesce`: per-cgroup/device/inode/operation LRU state.
- `pending_create`: short-lived `security_inode_create` to `security_file_open` correlation.
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

Resolved filesystem facts:

- `fexit/security_file_open`
- `fexit/security_mmap_file`
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

The loader probes every program independently and disables unsupported sections before loading the final object. When both truncate variants exist, `security_file_truncate` is preferred to avoid duplicate facts.

Security-wrapper programs are `fexit` tracing programs so denied operations (`result < 0`) can be discarded. Every BPF program returns zero; none can authorize or deny kernel operations.

The observer emits kernel facts only. AppSession correlation, baseline/package lookup, semantic classification, persistence policy, and backup decisions remain in Rust.
