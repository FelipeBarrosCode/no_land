use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{
    errors::{AppError, AppResult},
    utils::managed_binaries::{is_executable_file, locate_bundled_binary},
};

fn framework_candidate_paths(current_exe: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(explicit) = std::env::var("NOLAND_GSTREAMER_FRAMEWORK") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(
            cwd.join("src-tauri")
                .join("bundled")
                .join("macos")
                .join("GStreamer.framework"),
        );
        candidates.push(
            cwd.join("bundled")
                .join("macos")
                .join("GStreamer.framework"),
        );
    }

    if let Some(exe_dir) = current_exe.parent() {
        candidates.push(
            exe_dir
                .join("..")
                .join("Resources")
                .join("gstreamer")
                .join("macos")
                .join("GStreamer.framework"),
        );
        candidates.push(
            exe_dir
                .join("..")
                .join("Resources")
                .join("GStreamer.framework"),
        );
        if is_app_bundle_executable(current_exe) {
            candidates.push(
                exe_dir
                    .join("..")
                    .join("Frameworks")
                    .join("GStreamer.framework"),
            );
        }
    }

    candidates
}

fn resolve_framework_root(current_exe: &Path) -> Option<PathBuf> {
    framework_candidate_paths(current_exe)
        .into_iter()
        .find(|path| {
            path.join("Versions/Current/lib/libgstreamer-1.0.dylib")
                .is_file()
        })
}

fn gstreamer_root_candidate_paths(current_exe: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let target_triple = mic_sender_target_triple();

    if let Ok(explicit) = std::env::var("NOLAND_GSTREAMER_ROOT") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let native_root = cwd
            .join("src-tauri")
            .join(".native-deps")
            .join(target_triple);
        if cfg!(target_os = "windows") {
            candidates.push(
                native_root
                    .join("gstreamer")
                    .join("1.0")
                    .join(windows_gstreamer_arch_dir()),
            );
            candidates.push(
                cwd.join("src-tauri")
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
            candidates.push(cwd.join("binaries").join("gstreamer").join(target_triple));
        } else if cfg!(target_os = "linux") {
            candidates.push(native_root.join("gstreamer"));
        }
    }

    if let Some(exe_dir) = current_exe.parent() {
        if cfg!(target_os = "windows") {
            candidates.push(exe_dir.join("gstreamer").join(target_triple));
            candidates.push(
                exe_dir
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
            candidates.push(
                exe_dir
                    .join("..")
                    .join("Resources")
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
            candidates.push(
                exe_dir
                    .join("..")
                    .join("Resources")
                    .join("gstreamer")
                    .join(target_triple),
            );
        } else if cfg!(target_os = "linux") {
            candidates.push(exe_dir.join("gstreamer").join(target_triple));
            candidates.push(
                exe_dir
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
            candidates.push(
                exe_dir
                    .join("..")
                    .join("lib")
                    .join("Noland Connect")
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
            candidates.push(
                exe_dir
                    .join("..")
                    .join("lib")
                    .join("noland-connect")
                    .join("resources")
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
            candidates.push(
                exe_dir
                    .join("..")
                    .join("lib")
                    .join("noland-connect")
                    .join("binaries")
                    .join("gstreamer")
                    .join(target_triple),
            );
        }
    }

    candidates
}

pub fn resolve_gstreamer_root_for_current_exe() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    resolve_gstreamer_root(&current_exe)
}

fn resolve_gstreamer_root(current_exe: &Path) -> Option<PathBuf> {
    gstreamer_root_candidate_paths(current_exe)
        .into_iter()
        .find(|path| is_valid_gstreamer_root(path))
}

fn is_valid_gstreamer_root(path: &Path) -> bool {
    if cfg!(target_os = "windows") {
        return path.join("bin/gstreamer-1.0-0.dll").is_file();
    }

    if cfg!(target_os = "linux") {
        return path.join("lib/libgstreamer-1.0.so").is_file()
            || path.join("lib/libgstreamer-1.0.so.0").is_file()
            || path.join("lib64/libgstreamer-1.0.so").is_file()
            || path.join("lib64/libgstreamer-1.0.so.0").is_file();
    }

    false
}

fn mic_sender_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => "",
    }
}

#[cfg(target_os = "windows")]
fn windows_gstreamer_arch_dir() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "msvc_x86_64",
        "aarch64" => "msvc_arm64",
        _ => "msvc_x86_64",
    }
}

#[cfg(not(target_os = "windows"))]
fn windows_gstreamer_arch_dir() -> &'static str {
    "msvc_x86_64"
}

fn mic_sender_binary_names() -> Vec<String> {
    let mut names = vec![
        "noland-mic-sender".to_string(),
        "noland_mic_sender".to_string(),
    ];
    let triple = mic_sender_target_triple();
    if !triple.is_empty() {
        names.push(format!("noland-mic-sender-{triple}"));
        names.push(format!("noland_mic_sender-{triple}"));
    }

    #[cfg(target_os = "windows")]
    {
        let mut windows = Vec::new();
        for name in &names {
            windows.push(format!("{name}.exe"));
        }
        names.extend(windows);
    }

    names
}

pub fn resolve_mic_sender_binary() -> AppResult<PathBuf> {
    if let Ok(explicit) = std::env::var("NOLAND_MIC_SENDER_BIN") {
        let path = PathBuf::from(explicit.trim());
        if is_executable_file(&path) {
            return Ok(path);
        }
    }

    // `prepare-mic-sidecar.mjs dev` intentionally builds into .native-deps
    // without replacing the release-packaging asset under src-tauri/binaries.
    // Prefer that fresh development binary so `cargo run` cannot accidentally
    // launch an older bundled sidecar with an incompatible IPC protocol.
    #[cfg(debug_assertions)]
    {
        let executable = if cfg!(target_os = "windows") {
            "noland-mic-sender.exe"
        } else {
            "noland-mic-sender"
        };
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".native-deps")
            .join("mic-sidecar-target")
            .join(mic_sender_target_triple())
            .join("debug")
            .join(executable);
        if is_executable_file(&development) {
            return Ok(development);
        }
    }

    if let Some(path) = locate_bundled_binary(
        "noland-mic-sender",
        "NOLAND_MIC_SENDER_BIN",
        cfg!(target_os = "windows"),
        mic_sender_target_triple(),
    ) {
        return Ok(path);
    }

    let names = mic_sender_binary_names();
    if let Ok(cwd) = std::env::current_dir() {
        for name in &names {
            for candidate in [
                cwd.join("src-tauri")
                    .join("target")
                    .join("debug")
                    .join(name),
                cwd.join("src-tauri")
                    .join("target")
                    .join("release")
                    .join(name),
                cwd.join("target").join("debug").join(name),
                cwd.join("target").join("release").join(name),
            ] {
                if std::fs::metadata(&candidate)
                    .map(|meta| meta.is_file())
                    .unwrap_or(false)
                {
                    return Ok(candidate);
                }
            }
        }
    }

    Err(AppError::Command(
        "Could not find the noland-mic-sender sidecar. Build the sidecar binary or set NOLAND_MIC_SENDER_BIN to its full path."
            .to_string(),
    ))
}

fn is_app_bundle_executable(current_exe: &Path) -> bool {
    current_exe
        .components()
        .any(|component| component.as_os_str().to_string_lossy().ends_with(".app"))
}

fn prepend_env_path(command: &mut Command, key: &str, prefix: &Path, separator: &str) {
    let existing = std::env::var_os(key).unwrap_or_default();
    let mut value = prefix.as_os_str().to_os_string();
    if !existing.is_empty() {
        value.push(separator);
        value.push(existing);
    }
    command.env(key, value);
}

fn configure_macos_gstreamer_command(command: &mut Command, current_exe: &Path) {
    if let Some(cache_dir) = dirs::cache_dir() {
        let registry_dir = cache_dir.join("noland-connect").join("gstreamer");
        let _ = std::fs::create_dir_all(&registry_dir);
        command.env(
            "GST_REGISTRY_1_0",
            registry_dir.join("registry-macos.bin").as_os_str(),
        );
    }

    if !is_app_bundle_executable(current_exe) {
        if let Some(framework) = resolve_framework_root(current_exe) {
            let versions_current = framework.join("Versions/Current");
            let framework_parent = framework.parent().map(Path::to_path_buf);
            let lib_dir = versions_current.join("lib");
            let plugin_dir = lib_dir.join("gstreamer-1.0");
            let scanner = versions_current
                .join("libexec")
                .join("gstreamer-1.0")
                .join("gst-plugin-scanner");
            let typelib_dir = lib_dir.join("girepository-1.0");

            if let Some(parent) = framework_parent {
                command.env("DYLD_FRAMEWORK_PATH", parent.as_os_str());
            }
            command.env("GST_PLUGIN_SYSTEM_PATH_1_0", plugin_dir.as_os_str());
            command.env("GST_PLUGIN_PATH_1_0", plugin_dir.as_os_str());
            command.env("DYLD_FALLBACK_LIBRARY_PATH", lib_dir.as_os_str());
            if typelib_dir.is_dir() {
                command.env("GI_TYPELIB_PATH", typelib_dir.as_os_str());
            }
            if scanner.is_file() {
                command.env("GST_PLUGIN_SCANNER_1_0", scanner.as_os_str());
            }
            return;
        }

        command.env_remove("DYLD_FRAMEWORK_PATH");
        command.env_remove("GST_PLUGIN_SYSTEM_PATH_1_0");
        command.env_remove("GST_PLUGIN_PATH_1_0");
        command.env_remove("GST_PLUGIN_SCANNER_1_0");
        command.env_remove("GI_TYPELIB_PATH");
        command.env_remove("DYLD_FALLBACK_LIBRARY_PATH");
        return;
    }

    if let Some(framework) = resolve_framework_root(current_exe) {
        let versions_current = framework.join("Versions/Current");
        let framework_parent = framework.parent().map(Path::to_path_buf);
        let lib_dir = versions_current.join("lib");
        let plugin_dir = lib_dir.join("gstreamer-1.0");
        let scanner = versions_current
            .join("libexec")
            .join("gstreamer-1.0")
            .join("gst-plugin-scanner");
        let typelib_dir = lib_dir.join("girepository-1.0");

        if let Some(parent) = framework_parent {
            command.env("DYLD_FRAMEWORK_PATH", parent.as_os_str());
        }
        command.env("GST_PLUGIN_SYSTEM_PATH_1_0", plugin_dir.as_os_str());
        command.env("GST_PLUGIN_PATH_1_0", plugin_dir.as_os_str());
        command.env("DYLD_FALLBACK_LIBRARY_PATH", lib_dir.as_os_str());
        if typelib_dir.is_dir() {
            command.env("GI_TYPELIB_PATH", typelib_dir.as_os_str());
        }
        if scanner.is_file() {
            command.env("GST_PLUGIN_SCANNER_1_0", scanner.as_os_str());
        }
    }
}

fn configure_windows_gstreamer_command(command: &mut Command, current_exe: &Path) {
    // The external Windows plugin scanner can deadlock during gst::init(),
    // leaving the sidecar alive but unable to answer startSession. Scanning
    // the small managed plugin set in-process avoids that child-process hang.
    command.env("GST_REGISTRY_FORK", "no");

    if let Some(cache_dir) = dirs::cache_dir() {
        let registry_dir = cache_dir.join("noland-connect").join("gstreamer");
        let _ = std::fs::create_dir_all(&registry_dir);
        command.env(
            "GST_REGISTRY_1_0",
            registry_dir.join("registry-windows.bin").as_os_str(),
        );
    }

    if let Some(root) = resolve_gstreamer_root(current_exe) {
        let bin_dir = root.join("bin");
        let plugin_dir = root.join("lib").join("gstreamer-1.0");
        let typelib_dir = root.join("lib").join("girepository-1.0");
        let scanner_candidates = [
            root.join("libexec")
                .join("gstreamer-1.0")
                .join("gst-plugin-scanner.exe"),
            root.join("bin").join("gst-plugin-scanner.exe"),
        ];

        prepend_env_path(command, "PATH", &bin_dir, ";");
        command.env("GST_PLUGIN_SYSTEM_PATH_1_0", plugin_dir.as_os_str());
        command.env("GST_PLUGIN_PATH_1_0", plugin_dir.as_os_str());
        if let Some(scanner) = scanner_candidates.into_iter().find(|path| path.is_file()) {
            command.env("GST_PLUGIN_SCANNER_1_0", scanner.as_os_str());
        }
        if typelib_dir.is_dir() {
            command.env("GI_TYPELIB_PATH", typelib_dir.as_os_str());
        }
    }
}

fn configure_linux_gstreamer_command(command: &mut Command, current_exe: &Path) {
    if let Some(cache_dir) = dirs::cache_dir() {
        let registry_dir = cache_dir.join("noland-connect").join("gstreamer");
        let _ = std::fs::create_dir_all(&registry_dir);
        command.env(
            "GST_REGISTRY_1_0",
            registry_dir.join("registry-linux.bin").as_os_str(),
        );
    }

    if let Some(root) = resolve_gstreamer_root(current_exe) {
        let lib_dir = if root.join("lib").is_dir() {
            root.join("lib")
        } else {
            root.join("lib64")
        };
        let plugin_dir = lib_dir.join("gstreamer-1.0");
        let typelib_dir = lib_dir.join("girepository-1.0");
        let scanner_candidates = [
            root.join("libexec")
                .join("gstreamer-1.0")
                .join("gst-plugin-scanner"),
            lib_dir.join("gstreamer-1.0").join("gst-plugin-scanner"),
        ];

        // Do not inherit LD_LIBRARY_PATH into the mic sidecar. The bundled
        // GStreamer runtime is sanitized to exclude GLib/GTK/AT-SPI, and the OS
        // loader should resolve those distro-owned libraries from the system.
        command.env("LD_LIBRARY_PATH", lib_dir.as_os_str());
        command.env("GST_PLUGIN_SYSTEM_PATH_1_0", plugin_dir.as_os_str());
        command.env("GST_PLUGIN_PATH_1_0", plugin_dir.as_os_str());
        if let Some(scanner) = scanner_candidates.into_iter().find(|path| path.is_file()) {
            command.env("GST_PLUGIN_SCANNER_1_0", scanner.as_os_str());
        }
        if typelib_dir.is_dir() {
            command.env("GI_TYPELIB_PATH", typelib_dir.as_os_str());
        }
    }
}

pub fn configure_embedded_stream_runtime() {
    if !cfg!(target_os = "linux") {
        return;
    }

    let Ok(current_exe) = std::env::current_exe() else {
        return;
    };
    let Some(root) = resolve_gstreamer_root(&current_exe) else {
        return;
    };
    let lib_dir = if root.join("lib").is_dir() {
        root.join("lib")
    } else {
        root.join("lib64")
    };
    let plugin_dir = lib_dir.join("gstreamer-1.0");
    let scanner = root
        .join("libexec")
        .join("gstreamer-1.0")
        .join("gst-plugin-scanner");

    // Bundled plugins guarantee the baseline decoder/sinks. Keep GStreamer's
    // default system path available so hardware decoder plugins can be used
    // opportunistically when the OS already provides them.
    std::env::set_var("GST_PLUGIN_PATH_1_0", &plugin_dir);
    if scanner.is_file() {
        std::env::set_var("GST_PLUGIN_SCANNER_1_0", scanner);
    }
    if let Some(cache_dir) = dirs::cache_dir() {
        let registry_dir = cache_dir.join("noland-connect").join("gstreamer");
        let _ = std::fs::create_dir_all(&registry_dir);
        std::env::set_var(
            "GST_REGISTRY_1_0",
            registry_dir.join("registry-video-linux.bin"),
        );
    }
}

pub fn configure_gstreamer_command(command: &mut Command, current_exe: &Path) {
    if cfg!(target_os = "macos") {
        configure_macos_gstreamer_command(command, current_exe);
        return;
    }

    if cfg!(target_os = "windows") {
        configure_windows_gstreamer_command(command, current_exe);
        return;
    }

    if cfg!(target_os = "linux") {
        configure_linux_gstreamer_command(command, current_exe);
    }
}
