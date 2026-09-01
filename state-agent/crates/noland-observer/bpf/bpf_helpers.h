/* SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause) */
#ifndef NOLAND_BPF_HELPERS_H
#define NOLAND_BPF_HELPERS_H

#include "vmlinux.h"

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int (*name)[val]
#define __type(name, val) val *name
#define __array(name, val) val *name[]
#define __always_inline inline __attribute__((always_inline))
#define __noinline __attribute__((noinline))

#define BPF_ANY 0
#define BPF_NOEXIST 1
#define BPF_EXIST 2
#define BPF_F_CURRENT_CPU 0xffffffffULL

#define BPF_MAP_TYPE_HASH 1
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_PERCPU_ARRAY 6
#define BPF_MAP_TYPE_LRU_HASH 9
#define BPF_MAP_TYPE_RINGBUF 27

static void *(*bpf_map_lookup_elem)(const void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(const void *map, const void *key, const void *value, __u64 flags) = (void *)2;
static long (*bpf_map_delete_elem)(const void *map, const void *key) = (void *)3;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static __u64 (*bpf_get_current_pid_tgid)(void) = (void *)14;
static __u64 (*bpf_get_current_uid_gid)(void) = (void *)15;
static long (*bpf_get_current_comm)(void *buf, __u32 size) = (void *)16;
static void *(*bpf_get_current_task_btf)(void) = (void *)158;
static __u64 (*bpf_get_current_cgroup_id)(void) = (void *)80;
static long (*bpf_probe_read_kernel)(void *dst, __u32 size, const void *unsafe_ptr) = (void *)113;
static long (*bpf_probe_read_kernel_str)(void *dst, __u32 size, const void *unsafe_ptr) = (void *)115;
static long (*bpf_probe_read_user_str)(void *dst, __u32 size, const void *unsafe_ptr) = (void *)114;
static long (*bpf_ringbuf_output)(void *ringbuf, void *data, __u64 size, __u64 flags) = (void *)130;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, __u64 size, __u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, __u64 flags) = (void *)132;
static void (*bpf_ringbuf_discard)(void *data, __u64 flags) = (void *)133;

#define BPF_CORE_READ(src, field) ({                                      \
    typeof((src)->field) __r;                                            \
    __builtin_memset((void *)&__r, 0, sizeof(__r));                      \
    __builtin_preserve_access_index(                                     \
        bpf_probe_read_kernel((void *)&__r, sizeof(__r), &(src)->field)); \
    __r;                                                                  \
})

#define BPF_CORE_READ_INTO(dst, src, field)                               \
    __builtin_preserve_access_index(                                     \
        bpf_probe_read_kernel((void *)(dst), sizeof(*(dst)), &(src)->field))

#endif
