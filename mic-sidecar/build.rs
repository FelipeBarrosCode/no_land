fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.ends_with("apple-darwin") {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let source = std::path::Path::new(&manifest_dir)
        .join("..")
        .join("src-tauri")
        .join("src")
        .join("mic_client")
        .join("macos_permissions.m");

    if source.exists() {
        cc::Build::new()
            .file(source)
            .flag("-fobjc-arc")
            .compile("noland_mic_sender_permissions");

        // Link the frameworks needed by macos_permissions.m
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
