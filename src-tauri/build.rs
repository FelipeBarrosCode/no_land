use std::{env, path::PathBuf};

fn main() {
    let native_root = PathBuf::from("native");
    let wrapper_header = native_root.join("noland-moonlight/include/noland_moonlight.h");

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        native_root.join("CMakeLists.txt").display()
    );
    println!("cargo:rerun-if-changed={}", wrapper_header.display());
    println!(
        "cargo:rerun-if-changed={}",
        native_root
            .join("noland-moonlight/src/noland_moonlight.c")
            .display()
    );

    let dst = cmake::Config::new(&native_root)
        .define("BUILD_NOLAND_MOONLIGHT_HARNESS", "OFF")
        .define("BUILD_NOLAND_MOONLIGHT_TESTS", "OFF")
        .build();

    let lib_dir = dst.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=noland_moonlight");

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
