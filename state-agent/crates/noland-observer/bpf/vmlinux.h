/* SPDX-License-Identifier: MIT */
/*
 * Deliberately small CO-RE type surface. libbpf relocates these access paths
 * against the running kernel's BTF; this is not a kernel-version layout copy.
 */
#ifndef NOLAND_MINIMAL_VMLINUX_H
#define NOLAND_MINIMAL_VMLINUX_H

typedef unsigned char __u8;
typedef signed char __s8;
typedef unsigned short __u16;
typedef signed short __s16;
typedef unsigned int __u32;
typedef signed int __s32;
typedef unsigned long long __u64;
typedef signed long long __s64;
typedef unsigned long size_t;
typedef long ssize_t;
typedef long long loff_t;
typedef unsigned short umode_t;

#ifndef NULL
#define NULL ((void *)0)
#endif

#define __user
#define __preserve_access_index __attribute__((preserve_access_index))

struct qstr {
    union {
        struct { __u32 hash; __u32 len; };
        __u64 hash_len;
    };
    const unsigned char *name;
} __preserve_access_index;

struct super_block { __u64 s_dev; } __preserve_access_index;
struct inode {
    umode_t i_mode;
    __u64 i_ino;
    struct super_block *i_sb;
} __preserve_access_index;
struct dentry {
    struct dentry *d_parent;
    struct qstr d_name;
    struct inode *d_inode;
} __preserve_access_index;
struct vfsmount;
struct path {
    struct vfsmount *mnt;
    struct dentry *dentry;
} __preserve_access_index;
struct file {
    struct path f_path;
    struct inode *f_inode;
    unsigned int f_flags;
} __preserve_access_index;
struct ns_common { unsigned int inum; } __preserve_access_index;
struct mnt_namespace { struct ns_common ns; } __preserve_access_index;
struct nsproxy { struct mnt_namespace *mnt_ns; } __preserve_access_index;
struct task_struct {
    int pid;
    int tgid;
    struct task_struct *real_parent;
    char comm[16];
    struct nsproxy *nsproxy;
} __preserve_access_index;
struct linux_binprm { const char *filename; } __preserve_access_index;

struct bpf_raw_tracepoint_args { __u64 args[0]; };

#endif
