use std::{env, path::PathBuf};

fn native_dep_prefixes(target: &str, manifest_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut prefixes = Vec::new();

    if let Ok(prefix) = env::var("NOLAND_NATIVE_DEPS_PREFIX") {
        if !prefix.trim().is_empty() {
            prefixes.push(PathBuf::from(prefix));
        }
    }

    prefixes.push(manifest_dir.join(".native-deps").join(target));

    if target == "x86_64-apple-darwin" {
        prefixes.push(PathBuf::from("/usr/local"));
        prefixes.push(PathBuf::from("/opt/homebrew"));
    } else if target == "aarch64-apple-darwin" {
        prefixes.push(PathBuf::from("/opt/homebrew"));
        prefixes.push(PathBuf::from("/usr/local"));
    }

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
    let target = env::var("TARGET").expect("TARGET is not set");

    if matches!(env::var("NOLAND_SKIP_NATIVE_BUILD").as_deref(), Ok("1")) {
        println!("cargo:rerun-if-env-changed=NOLAND_SKIP_NATIVE_BUILD");
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

    println!("cargo:rerun-if-env-changed=NOLAND_SKIP_NATIVE_BUILD");

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

    let mut cmake_config = cmake::Config::new(&native_root);
    cmake_config
        .define("BUILD_NOLAND_MOONLIGHT_HARNESS", "OFF")
        .define("BUILD_NOLAND_MOONLIGHT_TESTS", "OFF");

    if is_macos || is_windows {
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
    if is_macos {
        cc::Build::new()
            .file("src/moonlight/platform/macos_stream_input.m")
            .flag("-fobjc-arc")
            .compile("noland_macos_stream_input");

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
        println!("cargo:rustc-link-lib=bcrypt");
        println!("cargo:rustc-link-lib=winmm");
    }
    if is_linux {
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
