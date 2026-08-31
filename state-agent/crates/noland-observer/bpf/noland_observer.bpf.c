// SPDX-License-Identifier: MIT
#include "vmlinux.h"
#include "bpf_helpers.h"
#include "noland_observer.h"

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 16 * 1024 * 1024);
} events SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct noland_config_v1);
} config SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u32);
    __type(value, __u8);
} ignored_tgids SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u64);
    __type(value, __u8);
} ignored_cgroups SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 8192);
    __type(key, struct noland_dev_inode);
    __type(value, __u8);
} ignored_files SEC(".maps");

struct coalesce_key {
    __u64 cgroup_id;
    __u64 dev;
    __u64 ino;
    __u16 type;
    __u16 pad;
    __u32 reserved;
};

struct coalesce_state {
    __u64 last_emit_ns;
    __u32 count;
    __u32 reserved;
};

struct pending_create_value {
    __u64 dentry;
    __u64 timestamp_ns;
};

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 32768);
    __type(key, struct coalesce_key);
    __type(value, struct coalesce_state);
} coalesce SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u64);
    __type(value, __u32);
} cgroup_mode SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 4096);
    __type(key, __u64);
    __type(value, struct pending_create_value);
} pending_create SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} sequence_counter SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct noland_event_v1);
} scratch_events SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, NOLAND_STAT_MAX);
    __type(key, __u32);
    __type(value, __u64);
} stats SEC(".maps");

_Static_assert(sizeof(struct noland_event_v1) == 8448,
               "event ABI must match the Rust v1 parser");
_Static_assert(sizeof(struct noland_config_v1) == 64,
               "config ABI must match the Rust v1 writer");

static __always_inline void count_stat(__u32 index)
{
    __u64 *value = bpf_map_lookup_elem(&stats, &index);
    if (value)
        *value += 1;
}

static __always_inline const struct noland_config_v1 *get_config(void)
{
    __u32 zero = 0;
    const struct noland_config_v1 *cfg = bpf_map_lookup_elem(&config, &zero);

    /* A zeroed value is the default. Ignore layouts this object cannot parse. */
    if (cfg && ((cfg->abi_version && cfg->abi_version != NOLAND_EVENT_ABI_VERSION) ||
                (cfg->size && cfg->size < sizeof(*cfg))))
        return NULL;
    return cfg;
}

static __always_inline int class_enabled(const struct noland_config_v1 *cfg, __u64 class)
{
    return !cfg || !cfg->enabled_mask || (cfg->enabled_mask & class);
}

static __always_inline __u32 observation_mode(const struct noland_config_v1 *cfg,
                                              __u64 cgroup_id)
{
    __u32 *mode = bpf_map_lookup_elem(&cgroup_mode, &cgroup_id);
    if (mode)
        return *mode;
    if (cfg && cfg->default_mode)
        return cfg->default_mode;
    return NOLAND_OBSERVE_DISCOVERY;
}

static __always_inline int task_is_ignored(__u32 tgid, __u64 cgroup_id,
                                            const struct noland_config_v1 *cfg)
{
    if ((cfg && cfg->ignored_tgid && cfg->ignored_tgid == tgid) ||
        bpf_map_lookup_elem(&ignored_tgids, &tgid) ||
        bpf_map_lookup_elem(&ignored_cgroups, &cgroup_id)) {
        count_stat(NOLAND_STAT_IGNORED);
        return 1;
    }
    if (cfg && cfg->target_cgroup_id && cfg->target_cgroup_id != cgroup_id) {
        count_stat(NOLAND_STAT_FILTERED);
        return 1;
    }
    return 0;
}

static __always_inline int current_is_ignored(__u64 class,
                                               const struct noland_config_v1 **cfg_out)
{
    const struct noland_config_v1 *cfg = get_config();
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u32 tgid = pid_tgid >> 32;
    __u64 cgroup_id = bpf_get_current_cgroup_id();

    *cfg_out = cfg;
    if (!class_enabled(cfg, class) || observation_mode(cfg, cgroup_id) == NOLAND_OBSERVE_NONE) {
        count_stat(NOLAND_STAT_FILTERED);
        return 1;
    }
    return task_is_ignored(tgid, cgroup_id, cfg);
}

static __always_inline int file_is_ignored(struct file *file)
{
    struct inode *inode;
    struct super_block *sb;
    struct noland_dev_inode key = {};

    if (!file)
        return 0;
    inode = BPF_CORE_READ(file, f_inode);
    if (!inode)
        return 0;
    sb = BPF_CORE_READ(inode, i_sb);
    key.ino = BPF_CORE_READ(inode, i_ino);
    if (sb)
        key.dev = BPF_CORE_READ(sb, s_dev);
    if (bpf_map_lookup_elem(&ignored_files, &key)) {
        count_stat(NOLAND_STAT_IGNORED);
        return 1;
    }
    return 0;
}

static __always_inline void fill_identity(struct noland_event_v1 *event)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u64 uid_gid = bpf_get_current_uid_gid();
    struct task_struct *task = bpf_get_current_task_btf();
    struct task_struct *parent;
    struct nsproxy *nsproxy;
    struct mnt_namespace *mnt_ns;

    event->abi_version = NOLAND_EVENT_ABI_VERSION;
    event->size = sizeof(*event);
    event->timestamp_ns = bpf_ktime_get_ns();
    event->pid = (__u32)pid_tgid;
    event->tgid = pid_tgid >> 32;
    event->uid = (__u32)uid_gid;
    event->gid = uid_gid >> 32;
    event->cgroup_id = bpf_get_current_cgroup_id();
    {
        __u32 zero = 0;
        __u64 *sequence = bpf_map_lookup_elem(&sequence_counter, &zero);
        if (sequence)
            event->sequence = __sync_fetch_and_add(sequence, 1) + 1;
    }
    event->accumulated_count = 1;
    bpf_get_current_comm(event->comm, sizeof(event->comm));

    if (!task)
        return;
    parent = BPF_CORE_READ(task, real_parent);
    if (parent)
        event->ppid = BPF_CORE_READ(parent, tgid);
    nsproxy = BPF_CORE_READ(task, nsproxy);
    if (!nsproxy)
        return;
    mnt_ns = BPF_CORE_READ(nsproxy, mnt_ns);
    if (mnt_ns)
        event->mnt_ns = BPF_CORE_READ(mnt_ns, ns.inum);
}

static __always_inline struct noland_event_v1 *new_event(__u16 type, __u16 flags)
{
    __u32 zero = 0;
    struct noland_event_v1 *event = bpf_map_lookup_elem(&scratch_events, &zero);
    if (!event)
        return NULL;

    event->type = type;
    event->flags = flags;
    event->timestamp_ns = 0;
    event->cgroup_id = 0;
    event->dev = 0;
    event->ino = 0;
    event->result = 0;
    event->offset = 0;
    event->length = 0;
    event->pid = 0;
    event->tgid = 0;
    event->ppid = 0;
    event->uid = 0;
    event->gid = 0;
    event->mnt_ns = 0;
    event->mode = 0;
    event->operation_flags = 0;
    event->sequence = 0;
    event->accumulated_count = 1;
    event->reserved = 0;
    event->path[0] = 0;
    event->name[0] = 0;
    event->dest_path[0] = 0;
    event->dest_name[0] = 0;
    fill_identity(event);
    return event;
}

static __always_inline void submit_event(struct noland_event_v1 *event)
{
    if (bpf_ringbuf_output(&events, event, sizeof(*event), 0) < 0) {
        count_stat(NOLAND_STAT_RINGBUF_DROPPED);
        return;
    }
    count_stat(NOLAND_STAT_EMITTED);
}

static __always_inline int consume_pending_create(struct file *file)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    struct pending_create_value *pending;
    struct dentry *dentry;
    int matched = 0;

    pending = bpf_map_lookup_elem(&pending_create, &pid_tgid);
    if (!pending)
        return 0;
    dentry = file ? BPF_CORE_READ(file, f_path.dentry) : NULL;
    if (dentry && pending->dentry == (__u64)dentry &&
        bpf_ktime_get_ns() - pending->timestamp_ns < 1000000000ULL)
        matched = 1;
    bpf_map_delete_elem(&pending_create, &pid_tgid);
    return matched;
}

static __always_inline void fill_inode(struct noland_event_v1 *event, struct inode *inode)
{
    struct super_block *sb;
    if (!inode)
        return;
    event->ino = BPF_CORE_READ(inode, i_ino);
    event->mode = BPF_CORE_READ(inode, i_mode);
    sb = BPF_CORE_READ(inode, i_sb);
    if (sb)
        event->dev = BPF_CORE_READ(sb, s_dev);
}

static __always_inline void fill_file(struct noland_event_v1 *event, struct file *file)
{
    struct inode *inode;
    long path_len;
    if (!file)
        return;
    inode = BPF_CORE_READ(file, f_inode);
    fill_inode(event, inode);
    event->operation_flags = BPF_CORE_READ(file, f_flags);
    path_len = bpf_d_path(__builtin_preserve_access_index(&file->f_path),
                          event->path, sizeof(event->path));
    if (path_len < 0) {
        event->flags |= NOLAND_F_PARTIAL_PATH;
        count_stat(NOLAND_STAT_PATH_ERRORS);
    }
}

static __always_inline void fill_parent_and_name(struct noland_event_v1 *event,
                                                  const struct path *parent,
                                                  struct dentry *dentry,
                                                  int destination)
{
    const unsigned char *name;
    struct inode *inode;
    long path_len;

    if (destination) {
        path_len = parent ? bpf_d_path((struct path *)parent, event->dest_path,
                                      sizeof(event->dest_path)) : -1;
        name = dentry ? BPF_CORE_READ(dentry, d_name.name) : NULL;
        if (name)
            bpf_probe_read_kernel_str(event->dest_name, sizeof(event->dest_name), name);
    } else {
        path_len = parent ? bpf_d_path((struct path *)parent, event->path,
                                      sizeof(event->path)) : -1;
        name = dentry ? BPF_CORE_READ(dentry, d_name.name) : NULL;
        if (name)
            bpf_probe_read_kernel_str(event->name, sizeof(event->name), name);
        inode = dentry ? BPF_CORE_READ(dentry, d_inode) : NULL;
        fill_inode(event, inode);
    }
    event->flags |= NOLAND_F_PARENT_AND_NAME;
    if (path_len < 0) {
        event->flags |= NOLAND_F_PARTIAL_PATH;
        count_stat(NOLAND_STAT_PATH_ERRORS);
    }
}

static __always_inline int should_sample_read(const struct noland_config_v1 *cfg)
{
    __u32 rate = cfg ? cfg->read_sample_rate : 0;
    if (rate <= 1)
        return 0;
    if ((bpf_ktime_get_ns() % rate) != 0) {
        count_stat(NOLAND_STAT_SAMPLED_OUT);
        return 1;
    }
    return 0;
}

static __always_inline int coalesced(const struct noland_config_v1 *cfg, __u16 type,
                                     __u64 cgroup_id, __u64 dev, __u64 ino,
                                     __u32 *accumulated)
{
    struct coalesce_key key = {
        .cgroup_id = cgroup_id,
        .dev = dev,
        .ino = ino,
        .type = type,
    };
    struct coalesce_state next = {};
    struct coalesce_state *previous;
    __u64 now = bpf_ktime_get_ns();
    __u64 window = 0;
    __u32 mode;

    *accumulated = 1;
    if (!cfg || !ino)
        return 0;
    mode = observation_mode(cfg, cgroup_id);
    if (type == NOLAND_EVENT_FILE_READ) {
        window = mode == NOLAND_OBSERVE_STEADY ? cfg->steady_read_ns : cfg->discovery_read_ns;
    } else if (type == NOLAND_EVENT_FILE_WRITE) {
        window = cfg->write_ns;
    }
    if (!window)
        return 0;

    previous = bpf_map_lookup_elem(&coalesce, &key);
    if (previous && now - previous->last_emit_ns < window) {
        next.last_emit_ns = previous->last_emit_ns;
        next.count = previous->count + 1;
        bpf_map_update_elem(&coalesce, &key, &next, BPF_ANY);
        count_stat(NOLAND_STAT_COALESCED);
        count_stat(type == NOLAND_EVENT_FILE_READ ? NOLAND_STAT_READ_COALESCED
                                                  : NOLAND_STAT_WRITE_COALESCED);
        return 1;
    }
    next.last_emit_ns = now;
    next.count = previous ? previous->count + 1 : 1;
    *accumulated = next.count;
    next.count = 0;
    bpf_map_update_elem(&coalesce, &key, &next, BPF_ANY);
    return 0;
}

static __always_inline int emit_io(struct file *file, __u16 type, __s64 result,
                                   __u64 requested, loff_t *position)
{
    const struct noland_config_v1 *cfg;
    struct noland_event_v1 *event;
    struct inode *inode;
    struct super_block *sb;
    __u64 dev = 0, ino = 0;
    __u64 cgroup_id = bpf_get_current_cgroup_id();
    __u32 accumulated = 1;
    __u64 class = type == NOLAND_EVENT_FILE_READ ? NOLAND_CLASS_READ : NOLAND_CLASS_WRITE;

    if (result <= 0 || current_is_ignored(class, &cfg) || file_is_ignored(file))
        return 0;
    if (type == NOLAND_EVENT_FILE_READ && should_sample_read(cfg))
        return 0;
    inode = file ? BPF_CORE_READ(file, f_inode) : NULL;
    if (inode) {
        ino = BPF_CORE_READ(inode, i_ino);
        sb = BPF_CORE_READ(inode, i_sb);
        if (sb)
            dev = BPF_CORE_READ(sb, s_dev);
    }
    if (coalesced(cfg, type, cgroup_id, dev, ino, &accumulated))
        return 0;

    event = new_event(type, 0);
    if (!event)
        return 0;
    event->result = result;
    event->length = requested;
    event->accumulated_count = accumulated;
    if (position)
        bpf_probe_read_kernel(&event->offset, sizeof(event->offset), position);
    fill_file(event, file);
    submit_event(event);
    return 0;
}

static __always_inline int trace_mutation(__u16 type, const struct path *parent,
                                          struct dentry *dentry, __u32 mode,
                                          __u32 operation_flags, int result)
{
    const struct noland_config_v1 *cfg;
    struct noland_event_v1 *event;

    if (result < 0 || current_is_ignored(NOLAND_CLASS_MUTATE, &cfg))
        return 0;
    event = new_event(type, 0);
    if (!event)
        return 0;
    event->result = result;
    event->mode = mode;
    event->operation_flags = operation_flags;
    fill_parent_and_name(event, parent, dentry, 0);
    submit_event(event);
    /* Tracing programs report the observed security result; they never return it. */
    return 0;
}

SEC("raw_tracepoint/sched_process_fork")
int noland_process_fork(struct bpf_raw_tracepoint_args *ctx)
{
    const struct noland_config_v1 *cfg;
    struct task_struct *parent = (void *)ctx->args[0];
    struct task_struct *child = (void *)ctx->args[1];
    struct noland_event_v1 *event;
    __u32 child_tgid;

    if (!child || current_is_ignored(NOLAND_CLASS_PROCESS, &cfg))
        return 0;
    child_tgid = BPF_CORE_READ(child, tgid);
    if (task_is_ignored(child_tgid, bpf_get_current_cgroup_id(), cfg))
        return 0;
    event = new_event(NOLAND_EVENT_PROCESS_FORK, 0);
    if (!event)
        return 0;
    event->pid = BPF_CORE_READ(child, pid);
    event->tgid = child_tgid;
    event->ppid = parent ? BPF_CORE_READ(parent, tgid) : 0;
    bpf_probe_read_kernel_str(event->comm, sizeof(event->comm),
                              __builtin_preserve_access_index(&child->comm[0]));
    submit_event(event);
    return 0;
}

SEC("raw_tracepoint/sched_process_exec")
int noland_process_exec(struct bpf_raw_tracepoint_args *ctx)
{
    const struct noland_config_v1 *cfg;
    struct linux_binprm *bprm = (void *)ctx->args[2];
    struct noland_event_v1 *event;
    const char *filename;

    if (current_is_ignored(NOLAND_CLASS_PROCESS, &cfg))
        return 0;
    event = new_event(NOLAND_EVENT_PROCESS_EXEC, 0);
    if (!event)
        return 0;
    if (bprm) {
        filename = BPF_CORE_READ(bprm, filename);
        if (filename)
            bpf_probe_read_kernel_str(event->path, sizeof(event->path), filename);
    }
    submit_event(event);
    return 0;
}

SEC("raw_tracepoint/sched_process_exit")
int noland_process_exit(struct bpf_raw_tracepoint_args *ctx)
{
    const struct noland_config_v1 *cfg;
    struct noland_event_v1 *event;
    if (current_is_ignored(NOLAND_CLASS_PROCESS, &cfg))
        return 0;
    event = new_event(NOLAND_EVENT_PROCESS_EXIT, 0);
    if (event)
        submit_event(event);
    return 0;
}

SEC("fexit/vfs_read")
int noland_vfs_read(__u64 *ctx)
{
    return emit_io((struct file *)ctx[0], NOLAND_EVENT_FILE_READ,
                   (__s64)ctx[4], ctx[2], (loff_t *)ctx[3]);
}

SEC("fexit/vfs_write")
int noland_vfs_write(__u64 *ctx)
{
    return emit_io((struct file *)ctx[0], NOLAND_EVENT_FILE_WRITE,
                   (__s64)ctx[4], ctx[2], (loff_t *)ctx[3]);
}

SEC("fexit/vfs_iter_read")
int noland_vfs_iter_read(__u64 *ctx)
{
    return emit_io((struct file *)ctx[0], NOLAND_EVENT_FILE_READ,
                   (__s64)ctx[4], 0, (loff_t *)ctx[2]);
}

SEC("fexit/vfs_iter_write")
int noland_vfs_iter_write(__u64 *ctx)
{
    return emit_io((struct file *)ctx[0], NOLAND_EVENT_FILE_WRITE,
                   (__s64)ctx[4], 0, (loff_t *)ctx[2]);
}

/*
 * These are tracing trampolines on the ordinary security_* wrappers, not BPF
 * LSM programs. fexit context is original arguments followed by the return
 * value. Every program returns zero, so observation cannot authorize or deny.
 */
SEC("fexit/security_file_open")
int noland_security_file_open(__u64 *ctx)
{
    struct file *file = (void *)ctx[0];
    int result = (int)ctx[1];
    const struct noland_config_v1 *cfg;
    struct noland_event_v1 *event;

    if (result < 0 || current_is_ignored(NOLAND_CLASS_OPEN, &cfg) || file_is_ignored(file))
        return 0;
    event = new_event(consume_pending_create(file) ? NOLAND_EVENT_FILE_CREATE
                                                   : NOLAND_EVENT_FILE_OPEN,
                      0);
    if (!event)
        return 0;
    event->result = result;
    fill_file(event, file);
    submit_event(event);
    return 0;
}

/* security_mmap_file(file, prot, flags) -> int */
SEC("fexit/security_mmap_file")
int noland_security_mmap_file(__u64 *ctx)
{
    struct file *file = (void *)ctx[0];
    int result = (int)ctx[3];
    const struct noland_config_v1 *cfg;
    struct noland_event_v1 *event;

    if (!file || result < 0 || current_is_ignored(NOLAND_CLASS_MMAP, &cfg) || file_is_ignored(file))
        return 0;
    event = new_event(NOLAND_EVENT_FILE_MMAP, 0);
    if (!event)
        return 0;
    event->result = result;
    event->mode = (__u32)ctx[1];
    event->operation_flags = (__u32)ctx[2];
    fill_file(event, file);
    submit_event(event);
    return 0;
}

/* security_inode_create(dir, dentry, mode) -> int; path is resolved by file_open. */
SEC("fexit/security_inode_create")
int noland_security_inode_create(__u64 *ctx)
{
    struct pending_create_value value = {};
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    int result = (int)ctx[3];

    if (result < 0 || !ctx[1])
        return 0;
    value.dentry = ctx[1];
    value.timestamp_ns = bpf_ktime_get_ns();
    bpf_map_update_elem(&pending_create, &pid_tgid, &value, BPF_ANY);
    return 0;
}

/* security_path_mknod(dir, dentry, mode, dev) -> int */
SEC("fexit/security_path_mknod")
int noland_security_path_mknod(__u64 *ctx)
{
    return trace_mutation(NOLAND_EVENT_FILE_CREATE, (void *)ctx[0], (void *)ctx[1],
                          (__u32)ctx[2], (__u32)ctx[3], (int)ctx[4]);
}

/* security_path_truncate(path) -> int */
SEC("fexit/security_path_truncate")
int noland_security_path_truncate(__u64 *ctx)
{
    const struct noland_config_v1 *cfg;
    const struct path *path = (void *)ctx[0];
    int result = (int)ctx[1];
    struct noland_event_v1 *event;

    if (current_is_ignored(NOLAND_CLASS_MUTATE, &cfg))
        return 0;
    if (result < 0)
        return 0;
    event = new_event(NOLAND_EVENT_FILE_TRUNCATE, 0);
    if (!event)
        return 0;
    event->result = result;
    if (path) {
        long n = bpf_d_path((struct path *)path, event->path, sizeof(event->path));
        struct dentry *dentry = BPF_CORE_READ(path, dentry);
        fill_inode(event, dentry ? BPF_CORE_READ(dentry, d_inode) : NULL);
        if (n < 0) {
            event->flags |= NOLAND_F_PARTIAL_PATH;
            count_stat(NOLAND_STAT_PATH_ERRORS);
        }
    }
    submit_event(event);
    return 0;
}

/* Alternative used by kernels exposing security_file_truncate(file). */
SEC("fexit/security_file_truncate")
int noland_security_file_truncate(__u64 *ctx)
{
    const struct noland_config_v1 *cfg;
    struct file *file = (void *)ctx[0];
    int result = (int)ctx[1];
    struct noland_event_v1 *event;

    if (result < 0 || current_is_ignored(NOLAND_CLASS_MUTATE, &cfg) || file_is_ignored(file))
        return 0;
    event = new_event(NOLAND_EVENT_FILE_TRUNCATE, 0);
    if (!event)
        return 0;
    event->result = result;
    fill_file(event, file);
    submit_event(event);
    return 0;
}

/* security_path_rename(old_dir, old_dentry, new_dir, new_dentry, flags) -> int */
SEC("fexit/security_path_rename")
int noland_security_path_rename(__u64 *ctx)
{
    const struct noland_config_v1 *cfg;
    int result = (int)ctx[5];
    struct noland_event_v1 *event;

    if (current_is_ignored(NOLAND_CLASS_MUTATE, &cfg))
        return 0;
    if (result < 0)
        return 0;
    event = new_event(NOLAND_EVENT_FILE_RENAME, 0);
    if (!event)
        return 0;
    event->result = result;
    event->operation_flags = (__u32)ctx[4];
    fill_parent_and_name(event, (void *)ctx[0], (void *)ctx[1], 0);
    fill_parent_and_name(event, (void *)ctx[2], (void *)ctx[3], 1);
    submit_event(event);
    return 0;
}

/* security_path_unlink(dir, dentry) -> int */
SEC("fexit/security_path_unlink")
int noland_security_path_unlink(__u64 *ctx)
{
    return trace_mutation(NOLAND_EVENT_FILE_UNLINK, (void *)ctx[0], (void *)ctx[1],
                          0, 0, (int)ctx[2]);
}

/* security_path_mkdir(dir, dentry, mode) -> int */
SEC("fexit/security_path_mkdir")
int noland_security_path_mkdir(__u64 *ctx)
{
    return trace_mutation(NOLAND_EVENT_DIR_MKDIR, (void *)ctx[0], (void *)ctx[1],
                          (__u32)ctx[2], 0, (int)ctx[3]);
}

/* security_path_rmdir(dir, dentry) -> int */
SEC("fexit/security_path_rmdir")
int noland_security_path_rmdir(__u64 *ctx)
{
    return trace_mutation(NOLAND_EVENT_DIR_RMDIR, (void *)ctx[0], (void *)ctx[1],
                          0, 0, (int)ctx[2]);
}

/* security_path_symlink(dir, dentry, old_name) -> int */
SEC("fexit/security_path_symlink")
int noland_security_path_symlink(__u64 *ctx)
{
    const struct noland_config_v1 *cfg;
    int result = (int)ctx[3];
    struct noland_event_v1 *event;

    if (result < 0 || current_is_ignored(NOLAND_CLASS_MUTATE, &cfg))
        return 0;
    event = new_event(NOLAND_EVENT_SYMLINK, 0);
    if (!event)
        return 0;
    event->result = result;
    fill_parent_and_name(event, (void *)ctx[0], (void *)ctx[1], 0);
    if (ctx[2])
        bpf_probe_read_kernel_str(event->dest_path, sizeof(event->dest_path), (void *)ctx[2]);
    submit_event(event);
    return 0;
}

/* security_path_chmod(path, mode) -> int */
SEC("fexit/security_path_chmod")
int noland_security_path_chmod(__u64 *ctx)
{
    const struct noland_config_v1 *cfg;
    const struct path *path = (void *)ctx[0];
    int result = (int)ctx[2];
    struct noland_event_v1 *event;

    if (result < 0 || current_is_ignored(NOLAND_CLASS_MUTATE, &cfg))
        return 0;
    event = new_event(NOLAND_EVENT_CHMOD, 0);
    if (!event)
        return 0;
    event->result = result;
    event->mode = (__u32)ctx[1];
    if (path)
        bpf_d_path((struct path *)path, event->path, sizeof(event->path));
    submit_event(event);
    return 0;
}

/* security_path_chown(path, uid, gid) -> int */
SEC("fexit/security_path_chown")
int noland_security_path_chown(__u64 *ctx)
{
    const struct noland_config_v1 *cfg;
    const struct path *path = (void *)ctx[0];
    int result = (int)ctx[3];
    struct noland_event_v1 *event;

    if (result < 0 || current_is_ignored(NOLAND_CLASS_MUTATE, &cfg))
        return 0;
    event = new_event(NOLAND_EVENT_CHOWN, 0);
    if (!event)
        return 0;
    event->result = result;
    event->uid = (__u32)ctx[1];
    event->gid = (__u32)ctx[2];
    if (path)
        bpf_d_path((struct path *)path, event->path, sizeof(event->path));
    submit_event(event);
    return 0;
}

char LICENSE[] SEC("license") = "Dual MIT/GPL";
