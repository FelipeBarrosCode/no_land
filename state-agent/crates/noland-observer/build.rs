use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=bpf/noland_observer.bpf.c");
    println!("cargo:rerun-if-changed=bpf/noland_observer.h");
    println!("cargo:rerun-if-changed=bpf/bpf_helpers.h");
    println!("cargo:rerun-if-changed=bpf/vmlinux.h");
    println!("cargo:rerun-if-env-changed=BPF_CLANG");
    println!("cargo:rerun-if-env-changed=TARGET");

    // Build scripts execute on the host, so cfg!(target_os) is insufficient for
    // cross builds. TARGET is the Cargo target whose artifact will contain BPF.
    let target = env::var("TARGET").expect("Cargo did not provide TARGET");
    if !target.contains("linux") {
        return;
    }

    validate_tracing_architecture();

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86",
        Ok("aarch64") => "arm64",
        Ok("arm") => "arm",
        Ok("riscv64") => "riscv",
        Ok("powerpc64") => "powerpc",
        Ok("s390x") => "s390",
        Ok(other) => panic!("unsupported Linux architecture for eBPF CO-RE: {other}"),
        Err(error) => panic!("Cargo did not provide CARGO_CFG_TARGET_ARCH: {error}"),
    };
    let bpf_target = match env::var("CARGO_CFG_TARGET_ENDIAN").as_deref() {
        Ok("big") => "bpfeb",
        _ => "bpfel",
    };
    let clang = env::var("BPF_CLANG").unwrap_or_else(|_| "clang".to_owned());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not provide OUT_DIR"));
    let object = out_dir.join("noland_observer.bpf.o");

    let status = Command::new(&clang)
        .args([
            "-g",
            "-O2",
            "-Wall",
            "-Werror",
            "-target",
            bpf_target,
            &format!("-D__TARGET_ARCH_{arch}"),
            "-Ibpf",
            "-c",
            "bpf/noland_observer.bpf.c",
            "-o",
        ])
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("failed to execute BPF compiler {clang:?}: {error}"));

    if !status.success() {
        panic!(
            "BPF compilation failed; install a clang build with the BPF backend or set BPF_CLANG (status {status})"
        );
    }

    // Export both the loader's stable name and the crate-specific alias.
    println!("cargo:rustc-env=NOLAND_BPF_OBJECT={}", object.display());
    let object_size = fs::metadata(&object)
        .unwrap_or_else(|error| panic!("BPF compiler did not create {}: {error}", object.display()))
        .len();
    if object_size == 0 {
        panic!(
            "BPF compiler created an empty object at {}",
            object.display()
        );
    }

    println!(
        "cargo:rustc-env=NOLAND_OBSERVER_BPF_OBJECT={}",
        object.display()
    );
}

fn validate_tracing_architecture() {
    const SOURCE: &str = "bpf/noland_observer.bpf.c";
    const REQUIRED: &[&str] = &[
        "fexit/security_file_open",
        "fexit/security_mmap_file",
        "fexit/security_inode_create",
        "fexit/security_path_mknod",
        "fexit/security_path_truncate",
        "fexit/security_file_truncate",
        "fexit/security_path_rename",
        "fexit/security_path_unlink",
        "fexit/security_path_mkdir",
        "fexit/security_path_rmdir",
        "fexit/security_path_symlink",
        "fexit/security_path_chmod",
        "fexit/security_path_chown",
        "fexit/vfs_iter_read",
        "fexit/vfs_iter_write",
    ];

    let source = fs::read_to_string(SOURCE).unwrap_or_else(|error| {
        panic!("failed to read {SOURCE} for architecture validation: {error}")
    });
    if source.contains("SEC(\"lsm/") {
        panic!("{SOURCE} must not contain BPF LSM programs; use security_* tracing hooks");
    }
    for section in REQUIRED {
        if !source.contains(&format!("SEC(\"{section}\")")) {
            panic!("{SOURCE} is missing required tracing section {section}");
        }
    }
}
