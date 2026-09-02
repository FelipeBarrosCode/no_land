/* SPDX-License-Identifier: MIT */
#ifndef NOLAND_OBSERVER_ABI_H
#define NOLAND_OBSERVER_ABI_H

#include "vmlinux.h"

#define NOLAND_EVENT_ABI_VERSION 1
#define NOLAND_COMM_LEN 16
#define NOLAND_PATH_LEN 4096
#define NOLAND_NAME_LEN 64

/* enabled_mask bits. A zero mask means "all events" for safe initial rollout. */
enum noland_event_class {
    NOLAND_CLASS_PROCESS = 1ULL << 0,
    NOLAND_CLASS_OPEN    = 1ULL << 1,
    NOLAND_CLASS_READ    = 1ULL << 2,
    NOLAND_CLASS_WRITE   = 1ULL << 3,
    NOLAND_CLASS_MMAP    = 1ULL << 4,
    NOLAND_CLASS_MUTATE  = 1ULL << 5,
};

enum noland_event_type {
    NOLAND_EVENT_PROCESS_FORK = 1,
    NOLAND_EVENT_PROCESS_EXEC,
    NOLAND_EVENT_PROCESS_EXIT,
    NOLAND_EVENT_FILE_OPEN = 16,
    NOLAND_EVENT_FILE_READ,
    NOLAND_EVENT_FILE_WRITE,
    NOLAND_EVENT_FILE_MMAP,
    NOLAND_EVENT_FILE_CREATE,
    NOLAND_EVENT_FILE_TRUNCATE,
    NOLAND_EVENT_FILE_RENAME,
    NOLAND_EVENT_FILE_UNLINK,
    NOLAND_EVENT_DIR_MKDIR,
    NOLAND_EVENT_DIR_RMDIR,
    NOLAND_EVENT_SYMLINK,
    NOLAND_EVENT_CHMOD,
    NOLAND_EVENT_CHOWN,
};

enum noland_event_flags {
    NOLAND_F_ATTEMPT          = 1U << 0,
    NOLAND_F_PARTIAL_PATH     = 1U << 1,
    NOLAND_F_PARENT_AND_NAME  = 1U << 2,
    NOLAND_F_SAMPLED          = 1U << 3,
};

struct noland_dev_inode {
    __u64 dev;
    __u64 ino;
};

enum noland_observer_mode {
    NOLAND_OBSERVE_NONE = 0,
    NOLAND_OBSERVE_DISCOVERY = 1,
    NOLAND_OBSERVE_STEADY = 2,
};

struct noland_config_v1 {
    __u16 abi_version;
    __u16 size;
    __u32 flags;
    __u64 enabled_mask;
    __u64 target_cgroup_id;       /* zero observes every cgroup */
    __u64 discovery_read_ns;      /* default 5 seconds */
    __u64 steady_read_ns;         /* default 60 seconds */
    __u64 write_ns;               /* default 250 milliseconds */
    __u32 read_sample_rate;       /* 0/1 keeps all, N keeps roughly 1/N */
    __u32 ignored_tgid;           /* convenient single-agent exclusion */
    __u32 default_mode;           /* discovery when zero for compatibility */
    __u32 reserved;
};

/* Fixed-size v1 record. size permits compatible tail extension in later ABIs. */
struct noland_event_v1 {
    __u16 abi_version;
    __u16 size;
    __u16 type;
    __u16 flags;
    __u64 timestamp_ns;
    __u64 cgroup_id;
    __u64 dev;
    __u64 ino;
    __s64 result;
    __u64 offset;
    __u64 length;
    __u32 pid;
    __u32 tgid;
    __u32 ppid;
    __u32 uid;
    __u32 gid;
    __u32 mnt_ns;
    __u32 mode;
    __u32 operation_flags;
    char comm[NOLAND_COMM_LEN];
    /* Dentry hooks carry a parent path and the final component separately. */
    char path[NOLAND_PATH_LEN];
    char name[NOLAND_NAME_LEN];
    char dest_path[NOLAND_PATH_LEN];
    char dest_name[NOLAND_NAME_LEN];
    __u64 sequence;
    __u32 accumulated_count;
    __u32 reserved;
};

enum noland_stat_index {
    /* Stable loader contract: key zero is aggregate ring-buffer loss. */
    NOLAND_STAT_RINGBUF_DROPPED = 0,
    NOLAND_STAT_EMITTED,
    NOLAND_STAT_FILTERED,
    NOLAND_STAT_IGNORED,
    NOLAND_STAT_SAMPLED_OUT,
    NOLAND_STAT_COALESCED,
    NOLAND_STAT_PATH_ERRORS,
    NOLAND_STAT_READ_COALESCED,
    NOLAND_STAT_WRITE_COALESCED,
    NOLAND_STAT_UNSUPPORTED_HOOK,
    NOLAND_STAT_MAX,
};

#endif
