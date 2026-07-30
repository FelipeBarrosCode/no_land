use std::path::{Path, PathBuf};
use std::process::Command;

use crate::errors::{AppError, AppResult};

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

    candidates.push(PathBuf::from("/Library/Frameworks/GStreamer.framework"));
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

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }

    #[allow(unreachable_code)]
    true
}

fn mic_sender_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => "",
    }
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
    let env_override = std::env::var("NOLAND_MIC_SENDER_BIN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = env_override.filter(|path| is_executable_file(path)) {
        return Ok(path);
    }

    let names = mic_sender_binary_names();
    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            for name in &names {
                candidates.push(exe_dir.join(name));
                candidates.push(exe_dir.join("binaries").join(name));
                candidates.push(exe_dir.join("..").join("Resources").join(name));
                candidates.push(
                    exe_dir
                        .join("..")
                        .join("Resources")
                        .join("binaries")
                        .join(name),
                );
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for name in &names {
            candidates.push(
                cwd.join("src-tauri")
                    .join("target")
                    .join("debug")
                    .join(name),
            );
            candidates.push(
                cwd.join("src-tauri")
                    .join("target")
                    .join("release")
                    .join(name),
            );
            candidates.push(cwd.join("src-tauri").join("binaries").join(name));
            candidates.push(cwd.join("target").join("debug").join(name));
            candidates.push(cwd.join("target").join("release").join(name));
            candidates.push(cwd.join("binaries").join(name));
        }
    }

    for candidate in candidates {
        if is_executable_file(&candidate) {
            return Ok(candidate);
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

pub fn configure_gstreamer_command(command: &mut Command, current_exe: &Path) {
    if cfg!(target_os = "macos") {
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
}
