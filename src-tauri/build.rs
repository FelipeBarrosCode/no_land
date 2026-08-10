use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

fn native_dep_prefixes(target: &str, manifest_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut prefixes = Vec::new();

    if let Ok(prefix) = env::var("NOLAND_NATIVE_DEPS_PREFIX") {
        if !prefix.trim().is_empty() {
            prefixes.push(PathBuf::from(prefix));
        }
    }

    prefixes.push(manifest_dir.join(".native-deps").join(target));
    prefixes
}

fn join_existing<I>(paths: I, separator: &str) -> Option<String>
where
    I: IntoIterator<Item = PathBuf>,
{
    let paths = paths
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    if paths.is_empty() {
        None
    } else {
        Some(paths.join(separator))
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=NOLAND_SKIP_APP_BUILD_RS");
    println!("cargo:rerun-if-env-changed=NOLAND_SKIP_NATIVE_BUILD");

    if env::var("NOLAND_SKIP_APP_BUILD_RS").ok().as_deref() == Some("1") {
        return;
    }

    let target = env::var("TARGET").expect("TARGET is not set");

    if matches!(env::var("NOLAND_SKIP_NATIVE_BUILD").as_deref(), Ok("1")) {
        if target.ends_with("apple-darwin") {
            cc::Build::new()
                .file("src/moonlight/platform/macos_display_detect.m")
                .flag("-fobjc-arc")
                .compile("noland_macos_display_detect");
            println!("cargo:rustc-link-lib=framework=AppKit");
            println!("cargo:rustc-link-lib=framework=ApplicationServices");
        }
        tauri_build::build();
        return;
    }

    prepare_gstreamer_bundle(&target).expect("failed to prepare bundled GStreamer runtime");
    ensure_managed_sidecar_bundle_artifacts()
        .expect("failed to prepare managed tool sidecar bundle artifacts");

    let native_root = PathBuf::from("native");
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"));
    let is_macos = target.ends_with("apple-darwin");
    let is_linux = target.contains("linux");
    let is_windows = target.contains("windows");
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
            .join("noland-moonlight/src/noland_video_renderer_linux.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_video_renderer_windows.cpp")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_desktop_input_sdl.c")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_audio_renderer_sdl.c")
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
        PathBuf::from("src/moonlight/platform/macos_stream_input.m").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        PathBuf::from("src/mic_client/macos_permissions.m").display()
    );

    let mut cmake_config = cmake::Config::new(&native_root);
    cmake_config
        .define("BUILD_NOLAND_MOONLIGHT_HARNESS", "OFF")
        .define("BUILD_NOLAND_MOONLIGHT_TESTS", "OFF");

    if is_macos || is_windows || is_linux {
        if is_macos {
            let arch = if target.starts_with("x86_64-") {
                "x86_64"
            } else if target.starts_with("aarch64-") {
                "arm64"
            } else {
                ""
            };
            if !arch.is_empty() {
                cmake_config.define("CMAKE_OSX_ARCHITECTURES", arch);
            }
            if let Ok(deployment_target) = env::var("MACOSX_DEPLOYMENT_TARGET") {
                if !deployment_target.trim().is_empty() {
                    cmake_config.define("CMAKE_OSX_DEPLOYMENT_TARGET", deployment_target);
                }
            }
        }

        let prefixes = native_dep_prefixes(&target, &manifest_dir);
        if let Some(primary_prefix) = prefixes.iter().find(|prefix| prefix.exists()) {
            cmake_config.define("NOLAND_NATIVE_PREFIX", primary_prefix.display().to_string());
            if is_windows {
                cmake_config.define("OPENSSL_ROOT_DIR", primary_prefix.display().to_string());
            }
        }
        if let Some(prefix_path) = join_existing(prefixes.clone(), ";") {
            cmake_config.define("CMAKE_PREFIX_PATH", prefix_path);
        }
        if let Some(pkg_config_path) = join_existing(
            prefixes
                .iter()
                .flat_map(|prefix| {
                    [
                        prefix.join("lib/pkgconfig"),
                        prefix.join("lib64/pkgconfig"),
                        prefix.join("share/pkgconfig"),
                    ]
                })
                .collect::<Vec<_>>(),
            ":",
        ) {
            let merged = match env::var("PKG_CONFIG_PATH") {
                Ok(existing) if !existing.trim().is_empty() => {
                    format!("{pkg_config_path}:{existing}")
                }
                _ => pkg_config_path,
            };
            cmake_config.env("PKG_CONFIG_PATH", merged);
        }
    }

    let dst = cmake_config.build();

    let lib_dir = dst.join("lib");
    let static_lib_dir = dst.join("lib/static");
    let moonlight_common_lib_dir = dst.join("build/moonlight-common-c");
    let enet_lib_dir = dst.join("build/moonlight-common-c/enet");
    let windows_config = if is_windows { Some("Release") } else { None };
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
    if let Some(config) = windows_config {
        println!(
            "cargo:rustc-link-search=native={}",
            lib_dir.join(config).display()
        );
        println!(
            "cargo:rustc-link-search=native={}",
            static_lib_dir.join(config).display()
        );
        println!(
            "cargo:rustc-link-search=native={}",
            moonlight_common_lib_dir.join(config).display()
        );
        println!(
            "cargo:rustc-link-search=native={}",
            enet_lib_dir.join(config).display()
        );
    }
    println!("cargo:rustc-link-lib=static=noland_moonlight");
    println!("cargo:rustc-link-lib=static=moonlight-common-c");
    println!("cargo:rustc-link-lib=static=enet");
    if is_macos {
        cc::Build::new()
            .file("src/moonlight/platform/macos_stream_input.m")
            .flag("-fobjc-arc")
            .compile("noland_macos_stream_input");
        cc::Build::new()
            .file("src/mic_client/macos_permissions.m")
            .flag("-fobjc-arc")
            .compile("noland_macos_permissions");

        for prefix in native_dep_prefixes(&target, &manifest_dir) {
            println!(
                "cargo:rustc-link-search=native={}",
                prefix.join("opt/openssl@3/lib").display()
            );
            println!(
                "cargo:rustc-link-search=native={}",
                prefix.join("lib").display()
            );
        }

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
    if is_windows {
        for prefix in native_dep_prefixes(&target, &manifest_dir) {
            println!(
                "cargo:rustc-link-search=native={}",
                prefix.join("lib").display()
            );
        }

        println!("cargo:rustc-link-lib=static=opus");
        println!("cargo:rustc-link-lib=static=SDL2-static");
        for library in [
            "advapi32", "bcrypt", "dinput8", "dxguid", "gdi32", "imm32", "mf", "mfplat", "mfuuid",
            "ole32", "oleaut32", "setupapi", "shell32", "user32", "uuid", "version", "winmm",
        ] {
            println!("cargo:rustc-link-lib={library}");
        }
    }
    if is_linux {
        for prefix in native_dep_prefixes(&target, &manifest_dir) {
            println!(
                "cargo:rustc-link-search=native={}",
                prefix.join("lib").display()
            );
            println!(
                "cargo:rustc-link-search=native={}",
                prefix.join("lib64").display()
            );
        }

        println!("cargo:rustc-link-lib=dylib=crypto");
        println!("cargo:rustc-link-lib=static=opus");
        println!("cargo:rustc-link-lib=static=SDL2");
        for library in [
            "dl",
            "m",
            "pthread",
            "rt",
            "gstreamer-1.0",
            "gstapp-1.0",
            "gstvideo-1.0",
            "gobject-2.0",
            "glib-2.0",
        ] {
            println!("cargo:rustc-link-lib={library}");
        }
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/noland-connect/resources/binaries/gstreamer/{target}/lib"
        );
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/noland-connect/resources/binaries/gstreamer/{target}/lib64"
        );
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

fn ensure_managed_sidecar_bundle_artifacts() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=TAURI_ENV_TARGET_TRIPLE");
    println!("cargo:rerun-if-env-changed=PROFILE");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let binaries_dir = manifest_dir.join("binaries");
    fs::create_dir_all(&binaries_dir)?;

    let target_triple = env::var("TAURI_ENV_TARGET_TRIPLE")
        .or_else(|_| env::var("TARGET"))
        .unwrap_or_else(|_| format!("{}-{}", env::consts::ARCH, env::consts::OS));
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let is_release = profile == "release";
    let target_is_windows = target_triple.contains("windows");

    ensure_staged_bundle_binary(
        &binaries_dir,
        "noland-mic-sender",
        &target_triple,
        target_is_windows,
        is_release,
        true,
        "run the Tauri build through the npm wrapper so the mic sidecar is staged first",
    )?;
    ensure_staged_bundle_binary(
        &binaries_dir,
        "noland-net-helper",
        &target_triple,
        target_is_windows,
        is_release,
        true,
        "run the Tauri build through the npm wrapper so the embedded GotaTun helper is staged first",
    )?;
    if target_is_windows && is_release {
        let wintun = binaries_dir.join(format!("wintun-{target_triple}.dll"));
        if !wintun.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "missing packaged Wintun adapter library '{}'; run the managed-tool staging step first",
                    wintun.display()
                ),
            ));
        }
        let wintun_license = binaries_dir.join("wintun-LICENSE.txt");
        if !wintun_license.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "missing packaged Wintun license '{}'; run the managed-tool staging step first",
                    wintun_license.display()
                ),
            ));
        }
    }
    ensure_staged_bundle_binary(
        &binaries_dir,
        "ssh",
        &target_triple,
        target_is_windows,
        is_release,
        true,
        "run the Tauri build through the npm wrapper so the bundled OpenSSH client is staged first",
    )?;
    ensure_staged_bundle_binary(
        &binaries_dir,
        "scp",
        &target_triple,
        target_is_windows,
        is_release,
        true,
        "run the Tauri build through the npm wrapper so the bundled OpenSSH scp client is staged first",
    )?;
    ensure_staged_bundle_binary(
        &binaries_dir,
        "ssh-keygen",
        &target_triple,
        target_is_windows,
        is_release,
        true,
        "run the Tauri build through the npm wrapper so the bundled OpenSSH keygen client is staged first",
    )?;

    Ok(())
}

fn ensure_staged_bundle_binary(
    binaries_dir: &Path,
    stem: &str,
    target_triple: &str,
    uses_exe_suffix: bool,
    is_release: bool,
    required_in_release: bool,
    release_hint: &str,
) -> io::Result<()> {
    let staged_name = if uses_exe_suffix {
        format!("{stem}-{target_triple}.exe")
    } else {
        format!("{stem}-{target_triple}")
    };
    let staged_path = binaries_dir.join(staged_name);

    if staged_path.exists() {
        return Ok(());
    }

    if is_release && required_in_release {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "missing packaged managed tool sidecar '{}'; {}",
                staged_path.display(),
                release_hint,
            ),
        ));
    }

    write_debug_sidecar_placeholder(&staged_path, target_triple.contains("windows"))
}

fn write_debug_sidecar_placeholder(path: &Path, target_is_windows: bool) -> io::Result<()> {
    if target_is_windows {
        fs::write(
            path,
            b"@echo off\r\necho noland-mic-sender debug placeholder. Run npm run tauri:dev or npm run prepare:mic-sidecar before launching the app.\r\nexit /b 1\r\n",
        )?;
        return Ok(());
    }

    #[cfg(unix)]
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

    #[cfg(not(unix))]
    {
        fs::write(
            path,
            b"noland-mic-sender debug placeholder. Run npm run tauri:dev or npm run prepare:mic-sidecar before launching the app.\n",
        )?;
        Ok(())
    }
}

fn prepare_gstreamer_bundle(target: &str) -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=NOLAND_GSTREAMER_FRAMEWORK");

    if !target.ends_with("apple-darwin") {
        return Ok(());
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let bundled_framework = manifest_dir
        .join("bundled")
        .join("macos")
        .join("GStreamer.framework");

    if has_macos_gstreamer_framework(&bundled_framework) {
        println!(
            "cargo:warning=Using staged project GStreamer framework {}",
            bundled_framework.display()
        );
        return Ok(());
    }

    if let Some(source) = resolve_macos_gstreamer_framework_source() {
        if let Some(parent) = bundled_framework.parent() {
            fs::create_dir_all(parent)?;
        }
        if bundled_framework.exists() {
            fs::remove_dir_all(&bundled_framework)?;
        }
        copy_dir_all(&source, &bundled_framework)?;
        println!(
            "cargo:warning=Staged GStreamer framework from {}",
            source.display()
        );
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "missing staged macOS GStreamer.framework; run node scripts/bootstrap-native-deps.mjs --target <triple> before building",
    ))
}

fn resolve_macos_gstreamer_framework_source() -> Option<PathBuf> {
    env::var("NOLAND_GSTREAMER_FRAMEWORK")
        .ok()
        .map(PathBuf::from)
        .filter(|path| has_macos_gstreamer_framework(path))
}

fn has_macos_gstreamer_framework(path: &Path) -> bool {
    [
        path.join("Versions/Current/lib/GStreamer"),
        path.join("Versions/Current/lib/libgstreamer-1.0.dylib"),
        path.join("Versions/Current/lib/libgstreamer-1.0.0.dylib"),
        path.join("Versions/1.0/lib/GStreamer"),
        path.join("Versions/1.0/lib/libgstreamer-1.0.dylib"),
        path.join("Versions/1.0/lib/libgstreamer-1.0.0.dylib"),
        path.join("Versions/Current/Libraries/GStreamer"),
        path.join("Versions/1.0/Libraries/GStreamer"),
    ]
    .iter()
    .any(|candidate| candidate.is_file())
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
