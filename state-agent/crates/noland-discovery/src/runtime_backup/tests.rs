use super::*;
use noland_state_core::{AppId, LauncherKind};
use serde_json::json;
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
