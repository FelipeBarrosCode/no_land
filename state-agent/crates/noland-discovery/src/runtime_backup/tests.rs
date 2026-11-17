use super::*;
use noland_state_core::{AppId, LauncherKind};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    source: PathBuf,
    base: PathBuf,
    view: PathBuf,
    private_home: PathBuf,
    record: PathBuf,
    receipt: PathBuf,
    app: AppIdentity,
}

fn directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn write(path: &Path, bytes: &[u8]) {
    directory(path.parent().unwrap());
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn elf(appimage: bool) -> Vec<u8> {
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[18] = 62;
    if appimage {
        bytes[8..11].copy_from_slice(b"AI\x02");
    }
    bytes.extend_from_slice(b"original executable");
    bytes
}

impl Fixture {
    fn pin_legacy_import(&self) -> Vec<PathBuf> {
        let legacy = self.base.join("portable/current");
        let view = legacy.join("view");
        let mut entries = BTreeMap::new();
        fn walk(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Identity>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            entries.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                identity(&metadata),
            );
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    walk(root, &entry.unwrap().path(), entries);
                }
            }
        }
        for name in ["user", "cache.bin"] {
            walk(&view, &view.join(name), &mut entries);
        }
        let proof = serde_json::to_vec(&json!({
            "schema": 1, "appId": "current", "originalFile": self.app.canonical_executable,
            "importSnapshotSha256": "a".repeat(64),
            "legacyManifestIdentity": identity(&fs::metadata(legacy.join("manifest.json")).unwrap()),
            "legacyViewIdentity": identity(&fs::metadata(&view).unwrap()),
            "entries": entries,
        })).unwrap();
        write(
            &self.receipt.parent().unwrap().join("legacy-import.json"),
            &proof,
        );
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&self.receipt).unwrap()).unwrap();
        receipt["legacyImportSha256"] = json!(format!("{:x}", Sha256::digest(&proof)));
        receipt["legacyPaths"] = json!(["user", "cache.bin"]);
        write(&self.receipt, &serde_json::to_vec(&receipt).unwrap());
        entries
            .keys()
            .map(|path| view.join(path))
            .chain([view.clone(), legacy.join("manifest.json")])
            .collect()
    }

    fn selected() -> Self {
        let root = std::env::temp_dir().join(format!("runtime-binding-{}", uuid::Uuid::new_v4()));
        directory(&root);
        let root = fs::canonicalize(root).unwrap();
        let home = root.join("home");
        let base = home.join(".local/share/noland-runtime");
        let source = base.join("app-data/original/Games/Hookshot");
        let private_home = base.join("app-data/current");
        let view = base.join("portable/current/view");
        let record = base.join("apps/current.json");
        let receipt = view.parent().unwrap().join("manifest.json");
        directory(&private_home);
        directory(&view);
        write(&source.join("hookshot"), &elf(false));
        write(&source.join("user/save/progress.sav"), b"original save");
        write(&source.join("romfs/asset.bin"), b"immutable asset");
        write(&source.join("cache.bin"), b"original cache");
        fs::set_permissions(source.join("hookshot"), fs::Permissions::from_mode(0o700)).unwrap();
        write(&view.join("hookshot"), b"");
        write(
            &view.join("user/save/progress.sav"),
            b"private changed save",
        );
        write(&view.join("cache.bin"), b"private changed cache");
        symlink("/home/runtime/.noland-source/romfs", view.join("romfs")).unwrap();
        let app = AppIdentity {
            canonical_executable: Some(source.join("hookshot")),
            launcher: Some(LauncherKind::Native),
            ..AppIdentity::new(AppId::exe("hookshot", "historical"), "Hookshot")
        };
        let fixture = Self {
            root,
            home,
            source,
            base,
            view,
            private_home,
            record,
            receipt,
            app,
        };
        fixture.registration(false, false);
        fixture.selected_receipt();
        fixture
    }

    fn registration(&self, workspace: bool, appimage: bool) {
        write(
            &self.record,
            &serde_json::to_vec(&json!({
                "id": "current",
                "name": "This name is not ownership evidence",
                "file": self.app.canonical_executable,
                "profile": "ubuntu24-x86_64",
                "appimage": appimage,
                "writableApp": workspace,
                "writablePaths": if workspace { Vec::<&str>::new() } else { vec!["user", "cache.bin"] }
            })).unwrap(),
        );
    }

    fn selected_receipt(&self) {
        fn seed(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Identity>) {
            let meta = fs::metadata(root.join(path)).unwrap();
            if meta.is_dir() {
                for entry in fs::read_dir(root.join(path)).unwrap() {
                    seed(root, &path.join(entry.unwrap().file_name()), out);
                }
            }
            out.insert(path.to_path_buf(), identity(&meta));
        }
        let mut seeds = BTreeMap::new();
        for name in ["user", "cache.bin"] {
            seed(&self.source, Path::new(name), &mut seeds);
        }
        let entries = fs::read_dir(&self.source)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    identity(&entry.metadata().unwrap()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        write(
            &self.receipt,
            &serde_json::to_vec(&json!({
                "source": self.source,
                "source_identity": identity(&fs::metadata(&self.source).unwrap()),
                "writable": ["user", "cache.bin"],
                "entries": entries,
                "seeds": seeds
            }))
            .unwrap(),
        );
    }

    fn workspace(mut self, appimage: bool) -> Self {
        write(&self.source.join("hookshot"), &elf(appimage));
        fs::set_permissions(
            self.source.join("hookshot"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        self.view = self.base.join("workspaces/current/view");
        self.receipt = self.view.parent().unwrap().join("manifest.json");
        directory(&self.view);
        write(&self.view.join("bin/main"), b"updated private executable");
        fs::set_permissions(
            self.view.join("bin/main"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        if appimage {
            symlink("bin/main", self.view.join("AppRun")).unwrap();
        }
        write(&self.view.join("user/save/progress.sav"), b"workspace save");
        self.registration(true, appimage);
        write(&self.receipt, &serde_json::to_vec(&json!({
            "schema": 1,
            "appId": "current",
            "originalFile": self.app.canonical_executable,
            "appimage": appimage,
            "executable": if appimage { "AppRun" } else { "bin/main" },
            "source": if appimage { self.base.join("cache").join(format!("appimage-{}/squashfs-root", "c".repeat(64))) } else { self.source.clone() },
            "importSnapshotSha256": "a".repeat(64),
            "legacyPaths": [],
            "stats": {"files": 2, "bytes": 40, "reflinkFiles": 0}
        })).unwrap());
        self
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn native_workspace_supersedes_only_its_import_tree_and_keeps_exact_source_anchor() {
    let f = Fixture::selected().workspace(false);
    let binding = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    assert!(binding.is_superseded_source(&f.source));
    for name in [
        "romfs/asset.bin",
        "user/save/progress.sav",
        "new-original-only-file",
    ] {
        assert!(binding.is_superseded_source(&f.source.join(name)));
    }
    assert!(!binding.is_superseded_source(&binding.source_executable));
    assert!(binding
        .roots()
        .any(|root| root == &binding.source_executable));
    assert!(!binding.is_superseded_source(&f.view.join("romfs/asset.bin")));
    assert!(!binding.is_superseded_source(&f.private_home.join(".config/save")));
    assert!(!binding.is_superseded_source(&f.source.with_file_name("OtherGame").join("data")));
    assert!(!binding.is_superseded_source(&f.source.join("../OtherGame/data")));
    assert!(binding.permits(&f.view.join("user/save/progress.sav")));
}

#[test]
fn selected_views_and_appimage_packages_do_not_suppress_original_installations() {
    for f in [Fixture::selected(), Fixture::selected().workspace(true)] {
        let binding = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
        assert!(!binding.is_superseded_source(&f.source));
        assert!(!binding.is_superseded_source(&f.source.join("romfs/asset.bin")));
        assert!(!binding.is_superseded_source(&binding.source_executable));
    }
}

#[test]
fn lost_or_invalid_workspace_never_authorizes_source_suppression() {
    for case in [
        "missing-receipt",
        "missing-workspace-executable",
        "missing-source-anchor",
        "source-not-elf",
        "mismatched-source",
    ] {
        let f = Fixture::selected().workspace(false);
        match case {
            "missing-receipt" => fs::remove_file(&f.receipt).unwrap(),
            "missing-workspace-executable" => fs::remove_file(f.view.join("bin/main")).unwrap(),
            "missing-source-anchor" => fs::remove_file(f.source.join("hookshot")).unwrap(),
            "source-not-elf" => write(&f.source.join("hookshot"), b"not an application source"),
            "mismatched-source" => {
                let mut receipt: serde_json::Value =
                    serde_json::from_slice(&fs::read(&f.receipt).unwrap()).unwrap();
                receipt["source"] = json!(f.source.with_file_name("OtherGame"));
                write(&f.receipt, &serde_json::to_vec(&receipt).unwrap());
            }
            _ => unreachable!(),
        }
        assert!(runtime_backup_binding(&f.home, &f.app).is_err(), "{case}");
    }
}

#[test]
fn pinned_native_import_retires_only_exact_old_keys_and_preserves_current_scope() {
    let f = Fixture::selected().workspace(false);
    let retired = f.pin_legacy_import();
    let before = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    for path in &retired {
        assert!(before.is_retired(path), "{}", path.display());
        assert!(!before.contains(path));
        assert!(!before.permits(path));
    }
    assert!(!before.is_retired(&f.base.join("portable/other/view/user/save")));
    assert!(!before.is_retired(&f.base.join("portable/current/view/unproved-save")));
    assert!(before.contains(&f.view.join("user/save/progress.sav")));
    assert!(before
        .control_files
        .contains(&f.receipt.parent().unwrap().join("legacy-import.json")));
    write(
        &f.view.join("user/save/progress.sav"),
        b"newer current workspace save",
    );
    let after = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    assert!(before.same_generation(&after));
    assert_eq!(f.app.app_id, AppId::exe("hookshot", "historical"));
}

#[test]
fn changed_or_new_legacy_data_cannot_be_silently_retired() {
    for case in [
        "save",
        "new-top-level",
        "new-nested",
        "coarse-top-level",
        "coarse-nested",
        "link",
        "manifest",
        "root",
        "partial-loss",
    ] {
        let f = Fixture::selected().workspace(false);
        f.pin_legacy_import();
        let old = f.base.join("portable/current");
        match case {
            "save" => write(
                &old.join("view/user/save/progress.sav"),
                b"later legacy progress",
            ),
            "new-top-level" => write(&old.join("view/later-save"), b"unmigrated"),
            "new-nested" => write(&old.join("view/user/save/later"), b"unmigrated"),
            "coarse-top-level" | "coarse-nested" => {
                let added = if case == "coarse-top-level" {
                    old.join("view/later-save")
                } else {
                    old.join("view/user/save/later-save")
                };
                write(&added, b"unmigrated");
                // Deterministically model a filesystem whose directory clock
                // ticks did not distinguish this addition. Exact child sets
                // must reject it even when all recorded identities match.
                let proof_path = f.receipt.parent().unwrap().join("legacy-import.json");
                let mut proof: serde_json::Value =
                    serde_json::from_slice(&fs::read(&proof_path).unwrap()).unwrap();
                proof["legacyViewIdentity"] =
                    json!(identity(&fs::metadata(old.join("view")).unwrap()));
                for name in ["user", "user/save"] {
                    proof["entries"][name] = json!(identity(
                        &fs::metadata(old.join("view").join(name)).unwrap()
                    ));
                }
                let bytes = serde_json::to_vec(&proof).unwrap();
                write(&proof_path, &bytes);
                let mut receipt: serde_json::Value =
                    serde_json::from_slice(&fs::read(&f.receipt).unwrap()).unwrap();
                receipt["legacyImportSha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
                write(&f.receipt, &serde_json::to_vec(&receipt).unwrap());
            }
            "link" => {
                fs::remove_file(old.join("view/user/save/progress.sav")).unwrap();
                symlink(&f.view, old.join("view/user/save/progress.sav")).unwrap();
            }
            "manifest" => write(&old.join("manifest.json"), b"{}"),
            "root" => {
                fs::rename(old.join("view"), old.join("archived")).unwrap();
                directory(&old.join("view"));
            }
            "partial-loss" => {
                fs::rename(old.join("manifest.json"), old.join("old-manifest.json")).unwrap()
            }
            _ => unreachable!(),
        }
        assert!(runtime_backup_binding(&f.home, &f.app).is_err(), "{case}");
        assert_eq!(
            fs::read(f.view.join("user/save/progress.sav")).unwrap(),
            b"workspace save"
        );
    }
}

#[test]
fn retired_payload_absence_after_restore_is_not_new_root_authority() {
    let f = Fixture::selected().workspace(false);
    let retired = f.pin_legacy_import();
    let before = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    fs::rename(
        f.base.join("portable/current"),
        f.base.join("portable/archived"),
    )
    .unwrap();
    let restored = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    assert!(
        !before.same_generation(&restored),
        "boundary loss during capture must invalidate its generation"
    );
    for path in retired {
        assert!(restored.is_retired(&path));
        assert!(!restored.contains(&path));
    }
    assert!(!restored.contains(&f.base.join("portable/archived/view/user")));
    assert!(restored.permits(&f.view.join("user/save/progress.sav")));
}

#[test]
fn native_import_proof_is_bounded_pinned_and_correlated() {
    for case in [
        "missing",
        "unpinned",
        "other-app",
        "other-source",
        "other-snapshot",
        "escape",
        "absolute",
        "oversized",
    ] {
        let f = Fixture::selected().workspace(false);
        f.pin_legacy_import();
        let proof = f.receipt.parent().unwrap().join("legacy-import.json");
        if case == "missing" {
            fs::remove_file(&proof).unwrap();
        } else if case == "oversized" {
            File::create(&proof)
                .unwrap()
                .set_len(16 * 1024 * 1024 + 1)
                .unwrap();
        } else {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&proof).unwrap()).unwrap();
            match case {
                "unpinned" => value["schema"] = json!(2),
                "other-app" => value["appId"] = json!("other"),
                "other-source" => value["originalFile"] = json!(f.home.join("OtherGame/hookshot")),
                "other-snapshot" => value["importSnapshotSha256"] = json!("f".repeat(64)),
                "escape" | "absolute" => {
                    let identity = value["entries"]["cache.bin"].clone();
                    value["entries"][if case == "escape" {
                        "../other/save"
                    } else {
                        "/etc/passwd"
                    }] = identity;
                }
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            write(&proof, &bytes);
            if case != "unpinned" {
                let mut receipt: serde_json::Value =
                    serde_json::from_slice(&fs::read(&f.receipt).unwrap()).unwrap();
                receipt["legacyImportSha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
                write(&f.receipt, &serde_json::to_vec(&receipt).unwrap());
            }
        }
        assert!(runtime_backup_binding(&f.home, &f.app).is_err(), "{case}");
    }
}

#[test]
fn exact_selected_binding_has_three_distinct_roots_and_no_alias_payload() {
    let f = Fixture::selected();
    write(&f.view.join("new-private.txt"), b"new private file");
    write(
        &f.private_home.join(".cache/shaders/program.bin"),
        b"owned home cache",
    );
    let binding = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    assert_eq!(binding.private_home, f.private_home);
    assert!(binding.permits(&f.view.join("user/save/progress.sav")));
    assert!(binding.permits(&f.view.join("cache.bin")));
    assert!(binding.permits(&f.view.join("new-private.txt")));
    assert!(binding.permits(&f.private_home.join(".cache/shaders/program.bin")));
    assert!(!binding.permits(&f.view.join("hookshot")));
    assert!(!binding.permits(&f.view.join("romfs/asset.bin")));
    assert!(!binding.permits(&f.base.join("app-data/another/private.dat")));
    assert!(!binding.permits(&f.base.join("apps/another.json")));
    assert!(binding.control_files.contains(&f.record));
    assert!(binding.control_files.contains(&f.receipt));
    assert_eq!(
        fs::read(f.source.join("user/save/progress.sav")).unwrap(),
        b"original save"
    );
    assert_eq!(f.app.app_id, AppId::exe("hookshot", "historical"));
}

#[test]
fn deleted_selected_entries_remain_authorized_deletion_scope() {
    let f = Fixture::selected();
    fs::remove_file(f.view.join("cache.bin")).unwrap();
    let binding = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    assert!(binding.data_roots.contains(&f.view.join("cache.bin")));
    assert!(binding.contains(&f.view.join("cache.bin")));
}

#[test]
fn same_name_is_not_an_exact_executable_match() {
    let f = Fixture::selected();
    let other = AppIdentity {
        canonical_executable: Some(f.home.join("Games/Other/hookshot")),
        ..f.app.clone()
    };
    assert!(runtime_backup_binding(&f.home, &other).unwrap().is_none());
}

#[test]
fn source_generation_placeholder_and_alias_forgery_fail_closed() {
    for case in [
        "source",
        "placeholder",
        "alias",
        "receipt",
        "registry",
        "mode",
    ] {
        let f = Fixture::selected();
        match case {
            "source" => write(
                &f.source.join("user/save/progress.sav"),
                b"replacement original",
            ),
            "placeholder" => write(&f.view.join("hookshot"), b"not placeholder"),
            "alias" => {
                fs::remove_file(f.view.join("romfs")).unwrap();
                symlink(f.home.join("another-app"), f.view.join("romfs")).unwrap();
            }
            "receipt" => fs::remove_file(&f.receipt).unwrap(),
            "registry" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&f.record).unwrap()).unwrap();
                value["id"] = json!("another-app");
                write(&f.record, &serde_json::to_vec(&value).unwrap());
            }
            "mode" => fs::set_permissions(&f.record, fs::Permissions::from_mode(0o666)).unwrap(),
            _ => unreachable!(),
        }
        assert!(runtime_backup_binding(&f.home, &f.app).is_err(), "{case}");
    }
}

#[test]
fn symlinked_private_home_and_escaping_private_link_are_not_authority() {
    let f = Fixture::selected();
    let foreign = f.base.join("app-data/another-app");
    directory(&foreign);
    fs::remove_dir(&f.private_home).unwrap();
    symlink(&foreign, &f.private_home).unwrap();
    assert!(runtime_backup_binding(&f.home, &f.app).is_err());
    fs::remove_file(&f.private_home).unwrap();
    directory(&f.private_home);
    let binding = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    symlink(&foreign, f.view.join("user/escape")).unwrap();
    assert!(!binding.permits(&f.view.join("user/escape")));
    assert!(!binding.permits(&f.view.join("user/escape/private.dat")));
}

#[test]
fn duplicate_exact_source_records_are_ambiguous_not_more_scope() {
    let f = Fixture::selected();
    write(
        &f.base.join("apps/duplicate.json"),
        &fs::read(&f.record).unwrap(),
    );
    assert!(runtime_backup_binding(&f.home, &f.app).is_err());
}

#[test]
fn native_and_appimage_workspaces_use_private_executable_and_exact_control_files() {
    for appimage in [false, true] {
        let f = Fixture::selected().workspace(appimage);
        let binding = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
        assert!(!binding.selected_view);
        assert!(binding.permits(&f.view.join("user/save/progress.sav")));
        assert!(binding.matches_executable(&f.view.join("bin/main")));
        assert!(!binding.matches_executable(&f.view.join("user/save/progress.sav")));
        assert!(binding.control_files.contains(&f.receipt));
        assert!(!binding.data_roots.contains(&f.base.join("cache")));
        assert!(!binding
            .data_roots
            .contains(&f.base.join("portable/current/view")));
        let source = runtime_registration_source(&f.home, &f.private_home)
            .unwrap()
            .unwrap();
        assert_eq!(Some(source), f.app.canonical_executable);
    }
}

#[test]
fn full_workspace_receipt_mismatch_and_executable_escape_fail_closed() {
    let f = Fixture::selected().workspace(true);
    fs::remove_file(f.view.join("AppRun")).unwrap();
    symlink(f.source.join("hookshot"), f.view.join("AppRun")).unwrap();
    assert!(runtime_backup_binding(&f.home, &f.app).is_err());
    fs::remove_file(f.view.join("AppRun")).unwrap();
    symlink("bin/main", f.view.join("AppRun")).unwrap();
    let before = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&f.receipt).unwrap()).unwrap();
    receipt["appId"] = json!("foreign");
    write(&f.receipt, &serde_json::to_vec(&receipt).unwrap());
    assert!(runtime_backup_binding(&f.home, &f.app).is_err());
    receipt["appId"] = json!("current");
    write(&f.receipt, &serde_json::to_vec(&receipt).unwrap());
    let after = runtime_backup_binding(&f.home, &f.app).unwrap().unwrap();
    assert!(!before.same_generation(&after));
}

#[test]
fn runtime_identity_discovery_preserves_exact_id_not_name_and_does_not_scan_payload() {
    let f = Fixture::selected();
    let discovered = registered_runtime_identities(&f.home, std::slice::from_ref(&f.app)).unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].app_id, f.app.app_id);
    let namesake = AppIdentity {
        canonical_executable: Some(f.home.join("Games/Other/hookshot")),
        ..f.app.clone()
    };
    let discovered = registered_runtime_identities(&f.home, &[namesake]).unwrap();
    assert_eq!(discovered[0].app_id, AppId::launcher("runtime", "current"));
    assert_eq!(
        discovered[0].canonical_executable,
        f.app.canonical_executable
    );
    // Discovery does not open private payload or require a first-launch
    // publication. Complete backup still refuses that unprepared registration.
    fs::remove_file(&f.receipt).unwrap();
    assert_eq!(
        registered_runtime_identities(&f.home, &[]).unwrap().len(),
        1
    );
    assert!(runtime_backup_binding(&f.home, &discovered[0]).is_err());
}

#[test]
fn forged_plain_control_file_cannot_be_registered_as_an_application_source() {
    let f = Fixture::selected();
    write(
        &f.source.join("hookshot"),
        b"not an ELF application or AppImage",
    );
    fs::set_permissions(f.source.join("hookshot"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(registered_runtime_identities(&f.home, &[]).is_err());
    assert!(runtime_backup_binding(&f.home, &f.app).is_err());
}
