use std::{env, path::PathBuf};

fn main() {
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
        println!("cargo:rustc-link-lib=dylib=crypto");
        println!("cargo:rustc-link-lib=dylib=opus");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
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
