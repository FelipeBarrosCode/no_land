use std::{fs, process::Command};

fn cli(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_noland-runtime"))
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn status_works_before_runtime_download_without_touching_parent_os() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path(), &["status", "--json"]);
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["installed"], false);
    assert_eq!(status["compatibility"], "unvalidated");
    assert!(status["apps"].as_array().unwrap().is_empty());
}

#[test]
fn rejects_unapproved_install_and_arbitrary_app_ids() {
    let home = tempfile::tempdir().unwrap();
    assert!(!cli(home.path(), &["ensure"]).status.success());
    assert!(!cli(home.path(), &["update"]).status.success());
    assert!(!cli(
        home.path(),
        &["install-prerequisites", "--app-id", "fixture"]
    )
    .status
    .success());
    assert!(
        !cli(home.path(), &["launch", "--app-id", "../../etc/passwd"])
            .status
            .success()
    );
    assert!(!cli(
        home.path(),
        &["create-shortcut", "--app-id", "bad;touch owned"]
    )
    .status
    .success());
    assert!(!home.path().join("owned").exists());
}

#[test]
fn windows_or_32bit_files_are_not_registered() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("game.exe");
    fs::write(&file, [0; 64]).unwrap();
    assert!(
        !cli(home.path(), &["register", "--file", file.to_str().unwrap()])
            .status
            .success()
    );
    let mut elf = [0; 64];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 1;
    elf[5] = 1;
    fs::write(&file, elf).unwrap();
    assert!(
        !cli(home.path(), &["register", "--file", file.to_str().unwrap()])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn registers_literal_metacharacter_path_idempotently_and_uses_id_in_shortcut() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("O'Brien; $(touch owned) App.AppImage");
    let mut elf = [0; 64];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf[8..11].copy_from_slice(b"AI\x02");
    elf[18] = 62;
    fs::write(&file, elf).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    let first = cli(home.path(), &["register", "--file", file.to_str().unwrap()]);
    assert!(first.status.success());
    let app: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let second = cli(home.path(), &["register", "--file", file.to_str().unwrap()]);
    assert_eq!(first.stdout, second.stdout);
    assert!(cli(
        home.path(),
        &["create-shortcut", "--app-id", app["id"].as_str().unwrap()]
    )
    .status
    .success());
    let shortcut = fs::read_to_string(home.path().join(format!(
        ".local/share/applications/noland-runtime-{}.desktop",
        app["id"].as_str().unwrap()
    )))
    .unwrap();
    let exec = shortcut.lines().find(|s| s.starts_with("Exec=")).unwrap();
    assert!(!exec.contains("O'Brien"));
    assert!(!exec.contains("touch"));
    assert!(exec.contains(app["id"].as_str().unwrap()));
    assert!(!home.path().join("owned").exists());
}

#[cfg(unix)]
#[test]
fn approved_linux_file_gets_only_owner_execute_permission() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("Trusted App.AppImage");
    let mut elf = [0; 64];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf[18] = 62;
    fs::write(&file, elf).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        !cli(home.path(), &["register", "--file", file.to_str().unwrap()])
            .status
            .success()
    );
    assert!(cli(
        home.path(),
        &[
            "register",
            "--file",
            file.to_str().unwrap(),
            "--make-executable"
        ]
    )
    .status
    .success());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn invalid_drop_is_not_made_executable() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("not-an-app.exe");
    fs::write(&file, [0; 64]).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!cli(
        home.path(),
        &[
            "register",
            "--file",
            file.to_str().unwrap(),
            "--make-executable"
        ]
    )
    .status
    .success());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn provisioning_shortcut_appears_on_the_desktop_without_a_release_configuration() {
    let home = tempfile::tempdir().unwrap();
    assert!(cli(home.path(), &["create-shortcut"]).status.success());
    let desktop = home.path().join("Desktop/noland-runtime.desktop");
    let entry = fs::read_to_string(desktop).unwrap();
    assert!(entry.contains("Name=No Land Runtime"));
    assert!(entry.contains("Exec=/usr/local/bin/noland-runtime gui"));
    assert!(!entry.contains("sudo"));
}

#[test]
fn runtime_plan_has_no_signing_or_published_release_prerequisite() {
    let home = tempfile::tempdir().unwrap();
    let result = cli(home.path(), &["plan"]);
    assert!(result.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(plan["source"], "cloud-pc-provisioning");
}
