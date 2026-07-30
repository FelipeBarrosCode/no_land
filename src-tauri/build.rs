use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-env-changed=NOLAND_SKIP_APP_BUILD_RS");

    if env::var("NOLAND_SKIP_APP_BUILD_RS").ok().as_deref() == Some("1") {
        return;
    }

    prepare_gstreamer_bundle().expect("failed to prepare bundled GStreamer runtime");
    ensure_mic_sender_external_bin().expect("failed to prepare mic sender sidecar bundle artifact");

    let native_root = PathBuf::from("native");
    let wrapper_header = native_root.join("noland-moonlight/include/noland_moonlight.h");

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        native_root.join("CMakeLists.txt").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/CMakeLists.txt")
            .display()
    );
    println!("cargo:rerun-if-changed={}", wrapper_header.display());
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_moonlight.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_controller_manager.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_controller_manager.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_video_renderer.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_video_renderer.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_audio_renderer.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_video_renderer_macos.m")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_audio_renderer_macos.m")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_audio_renderer_linux.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_audio_renderer_windows.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        PathBuf::from("src/moonlight/platform/macos_stream_input.m").display()
    );

    let dst = cmake::Config::new(&native_root)
        .define("BUILD_NOLAND_MOONLIGHT_HARNESS", "OFF")
        .define("BUILD_NOLAND_MOONLIGHT_TESTS", "OFF")
        .build();

    let lib_dir = dst.join("lib");
    let static_lib_dir = dst.join("lib/static");
    let moonlight_common_lib_dir = dst.join("build/moonlight-common-c");
    let enet_lib_dir = dst.join("build/moonlight-common-c/enet");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        static_lib_dir.display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        moonlight_common_lib_dir.display()
    );
    println!("cargo:rustc-link-search=native={}", enet_lib_dir.display());
    println!("cargo:rustc-link-lib=static=noland_moonlight");
    println!("cargo:rustc-link-lib=static=moonlight-common-c");
    println!("cargo:rustc-link-lib=static=enet");
    if cfg!(target_os = "macos") {
        cc::Build::new()
            .file("src/moonlight/platform/macos_stream_input.m")
            .flag("-fobjc-arc")
            .compile("noland_macos_stream_input");
        println!("cargo:rustc-link-search=native=/opt/homebrew/opt/openssl@3/lib");
        println!("cargo:rustc-link-search=native=/usr/local/opt/openssl@3/lib");
        println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
        println!("cargo:rustc-link-search=native=/usr/local/lib");
        println!("cargo:rustc-link-search=native=/opt/homebrew/lib/pkgconfig/../");
        println!("cargo:rustc-link-lib=dylib=crypto");
        println!("cargo:rustc-link-lib=dylib=opus");
        println!("cargo:rustc-link-lib=dylib=SDL2");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
    }
    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=opus");
        println!("cargo:rustc-link-lib=dylib=pulse-simple");
        println!("cargo:rustc-link-lib=dylib=pulse");
    }

    let bindings = bindgen::Builder::default()
        .header(wrapper_header.display().to_string())
        .allowlist_function("nl_.*")
        .allowlist_type("nl_.*")
        .allowlist_var("NL_.*")
        .generate()
        .expect("failed to generate noland moonlight bindings");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is not set"));
    bindings
        .write_to_file(out_dir.join("noland_moonlight_bindings.rs"))
        .expect("failed to write noland moonlight bindings");

    tauri_build::build()
}

fn ensure_mic_sender_external_bin() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=TAURI_ENV_TARGET_TRIPLE");
    println!("cargo:rerun-if-env-changed=PROFILE");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let binaries_dir = manifest_dir.join("binaries");
    fs::create_dir_all(&binaries_dir)?;

    let target_triple = env::var("TAURI_ENV_TARGET_TRIPLE")
        .or_else(|_| env::var("TARGET"))
        .unwrap_or_else(|_| format!("{}-{}", env::consts::ARCH, env::consts::OS));
    let staged_name = if cfg!(target_os = "windows") {
        format!("noland-mic-sender-{target_triple}.exe")
    } else {
        format!("noland-mic-sender-{target_triple}")
    };
    let staged_path = binaries_dir.join(staged_name);

    if staged_path.exists() {
        return Ok(());
    }

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    if profile == "release" {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "missing packaged mic sender sidecar '{}'; run the Tauri build through the npm wrapper so the sidecar is staged first",
                staged_path.display()
            ),
        ));
    }

    write_debug_sidecar_placeholder(&staged_path)
}

fn write_debug_sidecar_placeholder(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        fs::write(
            path,
            b"@echo off\r\necho noland-mic-sender debug placeholder. Run npm run tauri:dev or npm run prepare:mic-sidecar before launching the app.\r\nexit /b 1\r\n",
        )?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::write(
            path,
            b"#!/bin/sh\necho 'noland-mic-sender debug placeholder. Run npm run tauri:dev or npm run prepare:mic-sidecar before launching the app.' >&2\nexit 1\n",
        )?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        return Ok(());
    }
}

fn prepare_gstreamer_bundle() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=NOLAND_GSTREAMER_FRAMEWORK");
    println!("cargo:rerun-if-env-changed=NOLAND_GSTREAMER_HOMEBREW_PREFIX");

    if !cfg!(target_os = "macos") {
        return Ok(());
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let bundled_root = manifest_dir.join("bundled").join("macos");
    let bundled_framework = bundled_root.join("GStreamer.framework");

    if bundled_framework.exists() {
        fs::remove_dir_all(&bundled_framework)?;
    }
    fs::create_dir_all(&bundled_root)?;

    if let Some(source) = resolve_macos_gstreamer_framework_source() {
        copy_dir_all(&source, &bundled_framework)?;
        println!(
            "cargo:warning=Bundling GStreamer framework from {}",
            source.display()
        );
        return Ok(());
    }

    if let Some(homebrew_prefix) = resolve_macos_gstreamer_homebrew_prefix() {
        synthesize_framework_from_homebrew(&homebrew_prefix, &bundled_framework)?;
        println!(
            "cargo:warning=Bundling synthetic GStreamer framework from Homebrew prefix {}",
            homebrew_prefix.display()
        );
        return Ok(());
    }

    fs::create_dir_all(bundled_framework.join("Versions").join("Current"))?;
    println!("cargo:warning=No macOS GStreamer framework or Homebrew prefix found for bundling; install official GStreamer.framework or `brew install gstreamer`");

    Ok(())
}

fn resolve_macos_gstreamer_framework_source() -> Option<PathBuf> {
    env::var("NOLAND_GSTREAMER_FRAMEWORK")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| {
            let default_path = PathBuf::from("/Library/Frameworks/GStreamer.framework");
            default_path.exists().then_some(default_path)
        })
}

fn resolve_macos_gstreamer_homebrew_prefix() -> Option<PathBuf> {
    let env_override = env::var("NOLAND_GSTREAMER_HOMEBREW_PREFIX")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.exists());
    if env_override.is_some() {
        return env_override;
    }

    let candidates = [
        PathBuf::from("/opt/homebrew/opt/gstreamer"),
        PathBuf::from("/usr/local/opt/gstreamer"),
    ];
    for candidate in candidates {
        if candidate.join("lib/libgstreamer-1.0.dylib").is_file() {
            return Some(candidate);
        }
    }

    let output = std::process::Command::new("brew")
        .args(["--prefix", "gstreamer"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = PathBuf::from(prefix);
    path.join("lib/libgstreamer-1.0.dylib")
        .is_file()
        .then_some(path)
}

fn synthesize_framework_from_homebrew(prefix: &Path, bundled_framework: &Path) -> io::Result<()> {
    let versions_dir = bundled_framework.join("Versions");
    let current_dir = versions_dir.join("Current");
    fs::create_dir_all(&current_dir)?;

    copy_dir_all(&prefix.join("lib"), &current_dir.join("lib"))?;

    let libexec = prefix.join("libexec");
    if libexec.exists() {
        copy_dir_all(&libexec, &current_dir.join("libexec"))?;
    }

    let share = prefix.join("share");
    if share.exists() {
        copy_dir_all(&share, &current_dir.join("share"))?;
    }

    create_symlink_or_copy(
        Path::new("Versions/Current/lib"),
        &bundled_framework.join("lib"),
        true,
    )?;
    create_symlink_or_copy(
        Path::new("Versions/Current/libexec"),
        &bundled_framework.join("libexec"),
        true,
    )?;
    create_symlink_or_copy(
        Path::new("Versions/Current/share"),
        &bundled_framework.join("share"),
        true,
    )?;
    create_symlink_or_copy(Path::new("Current"), &versions_dir.join("A"), true)?;

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = dst.join(entry.file_name());
        if destination.exists() {
            if destination.is_dir() {
                fs::remove_dir_all(&destination)?;
            } else {
                fs::remove_file(&destination)?;
            }
        }
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &destination)?;
        } else if file_type.is_symlink() {
            let Ok(resolved) = entry.path().canonicalize() else {
                continue;
            };
            if resolved.is_dir() {
                copy_dir_all(&resolved, &destination)?;
            } else {
                fs::copy(&resolved, &destination)?;
            }
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn create_symlink_or_copy(target: &Path, destination: &Path, relative: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        if destination.exists() {
            if destination.is_dir() {
                fs::remove_dir_all(destination)?;
            } else {
                fs::remove_file(destination)?;
            }
        }
        let link_target = if relative {
            target.to_path_buf()
        } else {
            target.to_path_buf()
        };
        std::os::unix::fs::symlink(link_target, destination)?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        let source = if relative {
            destination
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(target)
        } else {
            target.to_path_buf()
        };
        let resolved = source.canonicalize()?;
        if resolved.is_dir() {
            copy_dir_all(&resolved, destination)?;
        } else {
            fs::copy(resolved, destination)?;
        }
        Ok(())
    }
}
