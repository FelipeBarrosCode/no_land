//! Explicit native-app portability. Original assets are never opened for writing.
use crate::store::{atomic_json, private_dir, private_file, App, Store};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{symlink, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::Command,
};
const ASSET_MOUNT: &str = "/home/runtime/.noland-source";

pub fn lock(store: &Store, id: &str) -> Result<File> {
    anyhow::ensure!(crate::valid_id(id), "Invalid app ID");
    let file = private_file(&store.state.join(format!("app-{id}.lock")))?;
    file.try_lock_exclusive()
        .context("Application is running or another configuration/launch is active")?;
    Ok(file)
}

fn name(value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value != "."
            && value != ".."
            && !value.contains(['/', '\\'])
            && !value.chars().any(char::is_control),
        "Writable paths must be single relative top-level names"
    );
    Ok(())
}

fn validate(app: &App) -> Result<()> {
    anyhow::ensure!(crate::valid_id(&app.id), "Invalid app ID");
    anyhow::ensure!(
        !app.appimage,
        "Writable views currently support native ELF apps only"
    );
    let exe = app.file.file_name().context("Missing executable name")?;
    let mut seen = std::collections::BTreeSet::new();
    for entry in &app.writable_paths {
        name(entry)?;
        anyhow::ensure!(entry.as_str() != exe, "Executable cannot be writable");
        anyhow::ensure!(seen.insert(entry), "Duplicate writable path");
    }
    // No symlink in any source ancestor, including the executable.
    no_links(&app.file)?;
    anyhow::ensure!(
        fs::symlink_metadata(&app.file)?.is_file(),
        "Executable must be a regular file"
    );
    Ok(())
}

fn no_links(path: &Path) -> Result<()> {
    anyhow::ensure!(path.is_absolute(), "Application paths must be absolute");
    let mut current = PathBuf::new();
    for part in path.components() {
        anyhow::ensure!(
            matches!(part, Component::RootDir | Component::Normal(_)),
            "Invalid path component"
        );
        current.push(part);
        anyhow::ensure!(
            !fs::symlink_metadata(&current)?.file_type().is_symlink(),
            "Symlink is not permitted: {}",
            current.display()
        );
    }
    Ok(())
}

// Nanosecond timestamps and inode identity detect replacement/mutation during seeding.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    mode: u32,
}
fn identity(path: &Path) -> Result<Identity> {
    let m = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        m.is_file() || m.is_dir(),
        "Only regular files/directories are supported: {}",
        path.display()
    );
    Ok(metadata_identity(&m))
}
fn metadata_identity(m: &fs::Metadata) -> Identity {
    Identity {
        dev: m.dev(),
        ino: m.ino(),
        size: m.len(),
        mtime: (m.mtime(), m.mtime_nsec()),
        ctime: (m.ctime(), m.ctime_nsec()),
        mode: m.mode(),
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    source: PathBuf,
    source_identity: Identity,
    writable: Vec<String>,
    entries: BTreeMap<String, Identity>,
    seeds: BTreeMap<PathBuf, Identity>,
}
fn tree_snapshot(
    root: &Path,
    relative: &Path,
    entries: &mut BTreeMap<PathBuf, Identity>,
) -> Result<()> {
    let path = root.join(relative);
    let id = identity(&path)?;
    if fs::symlink_metadata(&path)?.is_dir() {
        for entry in fs::read_dir(&path)? {
            tree_snapshot(root, &relative.join(entry?.file_name()), entries)?;
        }
    }
    anyhow::ensure!(
        identity(&path)? == id,
        "Source changed while inspecting writable paths"
    );
    entries.insert(relative.to_owned(), id);
    Ok(())
}
fn snapshot(app: &App) -> Result<Manifest> {
    validate(app)?;
    let source = app.file.parent().context("Missing source")?.to_owned();
    let source_identity = identity(&source)?;
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(&source)? {
        let entry = entry?;
        let key = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Non-UTF8 application entry"))?;
        name(&key)?;
        entries.insert(key, identity(&entry.path())?);
    }
    let mut seeds = BTreeMap::new();
    for key in &app.writable_paths {
        if entries.contains_key(key) {
            tree_snapshot(&source, Path::new(key), &mut seeds)?;
        }
    }
    anyhow::ensure!(
        identity(&source)? == source_identity,
        "Source directory changed during inspection"
    );
    Ok(Manifest {
        source,
        source_identity,
        writable: app.writable_paths.clone(),
        entries,
        seeds,
    })
}

pub fn configure(
    store: &Store,
    id: &str,
    paths: Vec<String>,
    approve: bool,
) -> Result<serde_json::Value> {
    anyhow::ensure!(approve, "Use --approve to seed private writable paths");
    anyhow::ensure!(!paths.is_empty(), "Select at least one writable path");
    let _lock = lock(store, id)?;
    let mut app = store.app(id)?;
    anyhow::ensure!(
        app.writable_paths.is_empty() || app.writable_paths == paths,
        "Existing writable selection cannot be changed automatically; private saves are preserved"
    );
    app.writable_paths = paths;
    let data_root = prepare(store, &app)?;
    atomic_json(&store.data.join("apps").join(format!("{id}.json")), &app)?;
    Ok(serde_json::json!({
        "appId": app.id,
        "policy": "private-writable-view-v1",
        "writablePaths": app.writable_paths,
        "dataRoot": data_root,
        "assetsMount": ASSET_MOUNT,
        "originalAssets": "read-only",
        "executable": "read-only-file-bind",
        "newTopLevelFiles": "private-view-only"
    }))
}

/// Caller must hold the app lock until the supervised process exits.
pub(crate) fn prepare(store: &Store, app: &App) -> Result<PathBuf> {
    anyhow::ensure!(
        !app.writable_paths.is_empty(),
        "Writable view requires explicit opt-in"
    );
    let manifest = snapshot(app)?;
    let base = store.data.join("portable");
    let home_mount = store
        .data
        .join("app-data")
        .join(&app.id)
        .join(".noland-source");
    anyhow::ensure!(
        !home_mount.starts_with(&manifest.source) && !manifest.source.starts_with(&home_mount),
        "Application source must not overlap its private asset mountpoint"
    );
    anyhow::ensure!(
        !base.starts_with(&manifest.source) && !manifest.source.starts_with(&base),
        "Application source must not overlap private writable-view storage"
    );
    no_links(&store.data)?;
    if base.try_exists()? {
        no_links(&base)?;
    }
    private_dir(&base)?;
    no_links(&base)?;
    let published = base.join(&app.id);
    if published.try_exists()? {
        no_links(&published)?;
        let manifest_file = published.join("manifest.json");
        no_links(&manifest_file)?;
        let saved: Manifest = serde_json::from_reader(open_read(&manifest_file)?)?;
        anyhow::ensure!(saved == manifest, "Application source/layout or writable selection changed; private data preserved, refusing automatic refresh");
        let view = published.join("view");
        no_links(&view)?;
        verify_view(&view, app, &manifest)?;
        File::open(&published)?.sync_all()?;
        File::open(&base)?.sync_all()?;
        return Ok(view);
    }
    let staging = base.join(format!("seed-{}", uuid::Uuid::new_v4()));
    private_dir(&staging)?;
    let result = (|| -> Result<PathBuf> {
        let view = staging.join("view");
        private_dir(&view)?;
        let exe = app.file.file_name().context("Missing executable")?;
        for key in manifest.entries.keys() {
            let target = view.join(key);
            if key.as_str() == exe {
                private_file(&target)?.sync_all()?;
            } else if app.writable_paths.contains(key) {
                copy_verified(&manifest.source.join(key), &target).with_context(|| {
                    format!(
                        "Cannot seed {key}; check free disk space. Original files are unchanged"
                    )
                })?;
            } else {
                symlink(Path::new(ASSET_MOUNT).join(key), &target)?;
            }
        }
        // Missing opted-in entries are allowed for new caches, but never aliases.
        anyhow::ensure!(
            snapshot(app)? == manifest,
            "Source changed during seeding; refusing to publish"
        );
        File::open(&view)?.sync_all()?;
        atomic_json(&staging.join("manifest.json"), &manifest)?;
        File::open(&staging)?.sync_all()?;
        fs::rename(&staging, &published)?;
        File::open(&base)?.sync_all()?;
        Ok(published.join("view"))
    })();
    if result.is_err() && staging.exists() {
        // Only this operation's unpublished, private staging directory.
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn verify_view(view: &Path, app: &App, manifest: &Manifest) -> Result<()> {
    let exe = app.file.file_name().context("Missing executable")?;
    for key in manifest.entries.keys() {
        let path = view.join(key);
        if key.as_str() == exe {
            let m = fs::symlink_metadata(path)?;
            anyhow::ensure!(
                m.is_file() && m.len() == 0 && m.nlink() == 1,
                "Executable placeholder changed"
            );
        } else if !app.writable_paths.contains(key) {
            anyhow::ensure!(
                fs::read_link(path)? == Path::new(ASSET_MOUNT).join(key),
                "Immutable asset alias changed"
            );
        }
    }
    Ok(())
}

fn open_read(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}
fn digest(file: &mut File) -> Result<Vec<u8>> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().to_vec())
}
fn copy_verified(source: &Path, target: &Path) -> Result<()> {
    let before = identity(source)?;
    if fs::symlink_metadata(source)?.is_dir() {
        private_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_verified(&entry.path(), &target.join(entry.file_name()))?;
        }
        File::open(target)?.sync_all()?;
    } else {
        let mut input = open_read(source)?;
        anyhow::ensure!(
            metadata_identity(&input.metadata()?) == before,
            "Source was replaced before copying"
        );
        let mut output = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(target)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
        }
        output.sync_all()?;
        anyhow::ensure!(
            metadata_identity(&input.metadata()?) == before,
            "Source changed while copying"
        );
        anyhow::ensure!(
            digest(&mut open_read(target)?)? == hash.finalize().to_vec(),
            "Seed SHA256 verification failed"
        );
    }
    anyhow::ensure!(
        identity(source)? == before,
        "Source changed during verified copy"
    );
    Ok(())
}

pub(crate) fn bind_view(command: &mut Command, app: &App, view: &Path, data: &Path) -> Result<()> {
    let source = app.file.parent().context("Missing source")?;
    let mountpoint = data.join(".noland-source");
    anyhow::ensure!(
        !mountpoint.starts_with(source) && !source.starts_with(&mountpoint),
        "Application source overlaps its private asset mountpoint"
    );
    no_links(data)?;
    if mountpoint.try_exists()? {
        no_links(&mountpoint)?;
    }
    private_dir(&mountpoint)?;
    no_links(&mountpoint)?;
    let executable = Path::new("/app").join(app.file.file_name().context("Missing executable")?);
    command
        .arg("--ro-bind")
        .arg(source)
        .arg(ASSET_MOUNT)
        .arg("--bind")
        .arg(view)
        .arg("/app")
        .arg("--ro-bind")
        .arg(&app.file)
        .arg(executable);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, App) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let store = Store {
            data: root.join("data"),
            state: root.join("state"),
        };
        store.init().unwrap();
        let source = root.join("game");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("main"), b"executable-original").unwrap();
        fs::write(source.join("assets.bin"), b"immutable-assets").unwrap();
        fs::create_dir(source.join("user")).unwrap();
        fs::write(source.join("user/save"), b"original-save").unwrap();
        fs::write(source.join("cache.bin"), b"original-cache").unwrap();
        let app = App {
            id: "test-app".into(),
            name: "Test".into(),
            file: source.join("main"),
            profile: crate::PROFILE.into(),
            appimage: false,
            writable_paths: vec!["user".into(), "cache.bin".into(), "new-cache.bin".into()],
        };
        atomic_json(&store.data.join("apps/test-app.json"), &app).unwrap();
        (temp, store, app)
    }

    #[test]
    fn old_records_default_to_read_only() {
        let app: App = serde_json::from_value(serde_json::json!({
            "id":"old", "name":"old", "file":"/game/main", "profile":crate::PROFILE, "appimage":false
        })).unwrap();
        assert!(app.writable_paths.is_empty());
        assert!(serde_json::to_value(app)
            .unwrap()
            .get("writablePaths")
            .is_none());
    }

    #[test]
    fn validates_explicit_top_level_names_and_protects_executable() {
        let (_temp, _store, mut app) = fixture();
        for invalid in [
            "",
            ".",
            "..",
            "../user",
            "/user",
            "user/save",
            "a\\b",
            "a\nb",
            "a\0b",
            "main",
        ] {
            app.writable_paths = vec![invalid.into()];
            assert!(validate(&app).is_err(), "{invalid:?}");
        }
        app.writable_paths = vec!["user".into(), "user".into()];
        assert!(validate(&app).is_err());
        app.writable_paths = vec!["user".into()];
        app.appimage = true;
        assert!(validate(&app).is_err());
    }

    #[test]
    fn verified_seed_and_relaunch_preserve_private_saves_and_originals() {
        let (_temp, store, app) = fixture();
        let _lock = lock(&store, &app.id).unwrap();
        let before = snapshot(&app).unwrap();
        let view = prepare(&store, &app).unwrap();
        assert_eq!(fs::read(view.join("user/save")).unwrap(), b"original-save");
        assert_eq!(fs::read(view.join("cache.bin")).unwrap(), b"original-cache");
        assert_eq!(
            fs::read_link(view.join("assets.bin")).unwrap(),
            Path::new("/home/runtime/.noland-source/assets.bin")
        );
        assert!(!fs::symlink_metadata(view.join("main"))
            .unwrap()
            .file_type()
            .is_symlink());
        fs::write(view.join("user/save"), b"new-save").unwrap();
        fs::write(view.join("cache.bin"), b"new-cache").unwrap();
        fs::write(view.join("new-cache.bin"), b"created-cache").unwrap();
        fs::write(view.join("dynamic-cache.bin"), b"future-cache-name").unwrap();
        assert_eq!(prepare(&store, &app).unwrap(), view);
        assert_eq!(fs::read(view.join("user/save")).unwrap(), b"new-save");
        assert_eq!(fs::read(view.join("cache.bin")).unwrap(), b"new-cache");
        assert_eq!(
            fs::read(app.file.parent().unwrap().join("user/save")).unwrap(),
            b"original-save"
        );
        assert!(!app
            .file
            .parent()
            .unwrap()
            .join("dynamic-cache.bin")
            .exists());
        assert_eq!(snapshot(&app).unwrap(), before);
    }

    #[test]
    fn source_or_layout_changes_fail_closed_without_erasing_private_data() {
        let (_temp, store, app) = fixture();
        let view = prepare(&store, &app).unwrap();
        fs::write(view.join("user/save"), b"keep-me").unwrap();
        fs::write(
            app.file.parent().unwrap().join("user/save"),
            b"changed-original",
        )
        .unwrap();
        assert!(prepare(&store, &app).is_err());
        assert_eq!(fs::read(view.join("user/save")).unwrap(), b"keep-me");
    }

    #[test]
    fn symlinks_and_partial_seeds_are_never_published() {
        let (_temp, store, app) = fixture();
        let source = app.file.parent().unwrap();
        symlink(source.join("assets.bin"), source.join("user/link")).unwrap();
        assert!(prepare(&store, &app).is_err());
        assert!(!store.data.join("portable/test-app").exists());
        // The copying primitive itself also rejects nested symlinks.
        let target = store.data.join("partial");
        assert!(copy_verified(&source.join("user"), &target).is_err());
        assert_eq!(
            fs::read(source.join("assets.bin")).unwrap(),
            b"immutable-assets"
        );
    }

    #[test]
    fn streaming_seed_verifies_multiple_buffers_and_never_overwrites_targets() {
        let (_temp, store, app) = fixture();
        let bytes: Vec<u8> = (0..400_001).map(|n| (n % 251) as u8).collect();
        let source = app.file.parent().unwrap().join("cache.bin");
        fs::write(&source, &bytes).unwrap();
        let target = store.data.join("copy");
        copy_verified(&source, &target).unwrap();
        assert_eq!(
            digest(&mut open_read(&source).unwrap()).unwrap(),
            digest(&mut open_read(&target).unwrap()).unwrap()
        );
        fs::write(&target, b"preserve-existing").unwrap();
        assert!(copy_verified(&source, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"preserve-existing");
        assert_eq!(fs::read(&source).unwrap(), bytes);
    }

    #[test]
    fn source_mountpoint_symlink_and_overlap_fail_without_touching_originals() {
        let (_temp, store, app) = fixture();
        let view = prepare(&store, &app).unwrap();
        let data = store.data.join("app-data").join(&app.id);
        private_dir(&data).unwrap();
        symlink(app.file.parent().unwrap(), data.join(".noland-source")).unwrap();
        assert!(bind_view(&mut Command::new("bwrap"), &app, &view, &data).is_err());
        assert!(bind_view(
            &mut Command::new("bwrap"),
            &app,
            &view,
            app.file.parent().unwrap()
        )
        .is_err());
        assert_eq!(fs::read(&app.file).unwrap(), b"executable-original");
        assert!(!app.file.parent().unwrap().join(".noland-source").exists());
    }

    #[test]
    fn top_level_and_executable_symlinks_are_rejected() {
        let (_temp, store, app) = fixture();
        let source = app.file.parent().unwrap();
        symlink("assets.bin", source.join("alias")).unwrap();
        assert!(prepare(&store, &app).is_err());
        fs::remove_file(source.join("alias")).unwrap();
        fs::remove_file(&app.file).unwrap();
        symlink("assets.bin", &app.file).unwrap();
        assert!(prepare(&store, &app).is_err());
    }

    #[test]
    fn app_lock_excludes_configuration_and_concurrent_launches() {
        let (_temp, store, app) = fixture();
        let held = lock(&store, &app.id).unwrap();
        assert!(lock(&store, &app.id).is_err());
        assert!(configure(&store, &app.id, app.writable_paths.clone(), true).is_err());
        drop(held);
        assert!(configure(&store, &app.id, app.writable_paths.clone(), true).is_ok());
        assert!(lock(&store, "../escape").is_err());
    }

    #[test]
    fn configure_requires_approval_and_preserves_existing_selection() {
        let (_temp, store, app) = fixture();
        assert!(configure(&store, &app.id, app.writable_paths.clone(), false).is_err());
        let result = configure(&store, &app.id, app.writable_paths.clone(), true).unwrap();
        assert_eq!(result["policy"], "private-writable-view-v1");
        let view = PathBuf::from(result["dataRoot"].as_str().unwrap());
        fs::write(view.join("user/save"), b"persistent").unwrap();
        configure(&store, &app.id, app.writable_paths.clone(), true).unwrap();
        assert_eq!(fs::read(view.join("user/save")).unwrap(), b"persistent");
        assert!(configure(&store, &app.id, vec!["user".into()], true).is_err());
    }

    #[test]
    fn executable_is_a_read_only_file_bind_not_an_asset_symlink() {
        let (_temp, store, app) = fixture();
        let view = prepare(&store, &app).unwrap();
        let mut command = Command::new("bwrap");
        let data = store.data.join("app-data").join(&app.id);
        private_dir(&data).unwrap();
        bind_view(&mut command, &app, &view, &data).unwrap();
        assert!(data.join(".noland-source").is_dir());
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[0..3],
            &[
                "--ro-bind",
                app.file.parent().unwrap().to_str().unwrap(),
                ASSET_MOUNT
            ]
        );
        assert_eq!(&args[3..6], &["--bind", view.to_str().unwrap(), "/app"]);
        assert_eq!(
            &args[6..9],
            &["--ro-bind", app.file.to_str().unwrap(), "/app/main"]
        );
        assert_eq!(fs::read(&app.file).unwrap(), b"executable-original");
    }

    #[test]
    fn changed_alias_or_private_storage_symlink_is_rejected() {
        let (_temp, store, app) = fixture();
        let view = prepare(&store, &app).unwrap();
        fs::remove_file(view.join("assets.bin")).unwrap();
        fs::write(view.join("assets.bin"), b"not-an-alias").unwrap();
        assert!(prepare(&store, &app).is_err());
        fs::rename(
            store.data.join("portable"),
            store.data.join("saved-portable"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let original_mode = 0o755;
        fs::set_permissions(
            store.data.join("saved-portable"),
            fs::Permissions::from_mode(original_mode),
        )
        .unwrap();
        symlink(
            store.data.join("saved-portable"),
            store.data.join("portable"),
        )
        .unwrap();
        assert!(prepare(&store, &app).is_err());
        assert_eq!(
            fs::metadata(store.data.join("saved-portable"))
                .unwrap()
                .mode()
                & 0o777,
            original_mode
        );
    }
}
