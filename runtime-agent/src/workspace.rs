//! Persistent, fully writable application copies; never writable original packages.
use crate::store::{atomic_json, private_dir, App, Store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct Prepared {
    pub directory: PathBuf,
    pub executable: PathBuf,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileId {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    mode: u32,
}

fn file_id(meta: &fs::Metadata) -> FileId {
    FileId {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.len(),
        mtime: (meta.mtime(), meta.mtime_nsec()),
        ctime: (meta.ctime(), meta.ctime_nsec()),
        mode: meta.mode(),
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Node {
    source: PathBuf,
    identity: FileId,
    link: Option<PathBuf>,
    execute_bits: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Inventory {
    source_root: FileId,
    legacy_root: Option<FileId>,
    nodes: BTreeMap<PathBuf, Node>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyStats {
    files: u64,
    bytes: u64,
    reflink_files: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: u32,
    app_id: String,
    original_file: PathBuf,
    appimage: bool,
    executable: PathBuf,
    source: PathBuf,
    import_snapshot_sha256: String,
    legacy_paths: Vec<String>,
    stats: CopyStats,
}

fn relative(path: &Path) -> Result<()> {
    anyhow::ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|p| matches!(p, Component::Normal(_)))
            && path
                .to_str()
                .is_some_and(|p| !p.chars().any(char::is_control)),
        "Application entries must be literal relative paths"
    );
    Ok(())
}

fn no_links(path: &Path) -> Result<()> {
    anyhow::ensure!(path.is_absolute(), "Application paths must be absolute");
    let mut current = PathBuf::new();
    for part in path.components() {
        anyhow::ensure!(
            matches!(part, Component::RootDir | Component::Normal(_)),
            "Invalid application path component"
        );
        current.push(part);
        anyhow::ensure!(
            !fs::symlink_metadata(&current)?.file_type().is_symlink(),
            "Private storage/source ancestor must not be a symlink"
        );
    }
    Ok(())
}

fn present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn storage(store: &Store, app: &App) -> Result<(PathBuf, PathBuf)> {
    anyhow::ensure!(crate::valid_id(&app.id), "Invalid app ID");
    no_links(&store.data)?;
    let base = store.data.join("workspaces");
    if present(&base)? {
        no_links(&base)?;
    }
    private_dir(&base)?;
    File::open(&base)?.sync_all()?;
    File::open(&store.data)?.sync_all()?;
    let published = base.join(&app.id);
    if present(&published)? {
        no_links(&published)?;
    }
    Ok((base, published))
}

fn open_read(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

fn read_receipt(published: &Path, app: &App) -> Result<Receipt> {
    let path = published.join("manifest.json");
    no_links(&path)?;
    let receipt: Receipt = serde_json::from_reader(open_read(&path)?)?;
    anyhow::ensure!(
        receipt.schema == 1
            && receipt.app_id == app.id
            && receipt.original_file == app.file
            && receipt.appimage == app.appimage,
        "Private workspace belongs to another application; existing data preserved"
    );
    relative(&receipt.executable)?;
    Ok(receipt)
}

fn complete_publication(published: &Path) -> Result<()> {
    // A prior rename may have succeeded even if its final directory fsync failed.
    // Never accept such a visible publication without retrying its barriers.
    File::open(published)?.sync_all()?;
    let base = published.parent().context("Missing workspace base")?;
    File::open(base)?.sync_all()?;
    File::open(base.parent().context("Missing runtime data root")?)?.sync_all()?;
    Ok(())
}

fn prepared(published: &Path, receipt: &Receipt) -> Result<Prepared> {
    let directory = published.join("view");
    no_links(&directory)?;
    anyhow::ensure!(
        directory.is_dir(),
        "Private application directory is missing"
    );
    // Resolve composed link chains in the private root, never against host "/".
    let resolved = resolve_entry(&directory, &receipt.executable)
        .context("Private executable is missing; private saves are preserved, not reseeded")?;
    anyhow::ensure!(
        resolved.is_file() && fs::metadata(&resolved)?.mode() & 0o111 != 0,
        "Private executable must remain executable inside its application directory"
    );
    Ok(Prepared {
        directory,
        executable: Path::new("/app").join(&receipt.executable),
    })
}

pub(crate) fn resolve_entry(root: &Path, entry: &Path) -> Result<PathBuf> {
    relative(entry)?;
    let mut pending = VecDeque::from_iter(entry.components().map(|p| p.as_os_str().to_owned()));
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            anyhow::ensure!(
                resolved.pop(),
                "Application link escapes its private directory"
            );
            continue;
        }
        let candidate = root.join(&resolved).join(&part);
        if fs::symlink_metadata(&candidate)?.file_type().is_symlink() {
            links += 1;
            anyhow::ensure!(links <= 40, "Too many application symlinks");
            let target = fs::read_link(candidate)?;
            anyhow::ensure!(
                !target.is_absolute(),
                "Application link escapes its private directory"
            );
            let mut prefix =
                VecDeque::from_iter(target.components().map(|p| p.as_os_str().to_owned()));
            prefix.append(&mut pending);
            pending = prefix;
        } else {
            resolved.push(part);
        }
    }
    Ok(root.join(resolved))
}

fn validate_links(nodes: &BTreeMap<PathBuf, Node>) -> Result<()> {
    for (path, node) in nodes {
        if node.link.is_none() {
            continue;
        }
        let mut pending = VecDeque::from_iter(path.components().map(|p| p.as_os_str().to_owned()));
        let mut resolved = PathBuf::new();
        let mut links = 0;
        while let Some(part) = pending.pop_front() {
            if part == "." {
                continue;
            }
            if part == ".." {
                anyhow::ensure!(
                    resolved.pop(),
                    "Composed application symlinks escape the private directory"
                );
                continue;
            }
            let candidate = resolved.join(&part);
            if let Some(target) = nodes.get(&candidate).and_then(|node| node.link.as_ref()) {
                links += 1;
                anyhow::ensure!(
                    links <= 40,
                    "Application symlink cycle or excessive link depth"
                );
                let mut prefix =
                    VecDeque::from_iter(target.components().map(|p| p.as_os_str().to_owned()));
                prefix.append(&mut pending);
                pending = prefix;
            } else {
                resolved.push(part);
            }
        }
    }
    Ok(())
}

/// No source reads or reseeding on reuse, including after an application's own update.
pub(crate) fn existing(store: &Store, app: &App) -> Result<Option<Prepared>> {
    if !app.writable_app {
        return Ok(None);
    }
    let (_, published) = storage(store, app)?;
    if !present(&published)? {
        return Ok(None);
    }
    let receipt = read_receipt(&published, app)?;
    complete_publication(&published)?;
    Ok(Some(prepared(&published, &receipt)?))
}

fn check_link(_relative_path: &Path, target: &Path) -> Result<()> {
    anyhow::ensure!(
        !target.as_os_str().is_empty()
            && !target.is_absolute()
            && target
                .to_str()
                .is_some_and(|p| !p.chars().any(char::is_control)),
        "Only application-relative symlinks can be imported"
    );
    Ok(())
}

fn walk(source: &Path, logical: &Path, nodes: &mut BTreeMap<PathBuf, Node>) -> Result<()> {
    relative(logical)?;
    anyhow::ensure!(
        logical.components().count() <= 128,
        "Application directory nesting is too deep"
    );
    no_links(source.parent().context("Missing source parent")?)?;
    let meta = fs::symlink_metadata(source)?;
    let kind = meta.file_type();
    anyhow::ensure!(
        kind.is_file() || kind.is_dir() || kind.is_symlink(),
        "Application contains a device, socket or other unsupported special file"
    );
    let link = if kind.is_symlink() {
        let target = fs::read_link(source)?;
        check_link(logical, &target)?;
        Some(target)
    } else {
        None
    };
    if kind.is_dir() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            walk(&entry.path(), &logical.join(entry.file_name()), nodes)?;
        }
    }
    let identity = file_id(&meta);
    anyhow::ensure!(
        file_id(&fs::symlink_metadata(source)?) == identity,
        "Application source changed during inspection"
    );
    nodes.insert(
        logical.to_owned(),
        Node {
            source: source.to_owned(),
            identity,
            link,
            execute_bits: meta.mode() & 0o111,
        },
    );
    Ok(())
}

fn inventory(store: &Store, app: &App, source: &Path) -> Result<Inventory> {
    no_links(source)?;
    let source_root = file_id(&fs::metadata(source)?);
    let legacy = if !app.writable_app && !app.writable_paths.is_empty() {
        Some(crate::portable::prepare(store, app)?)
    } else {
        None
    };
    let legacy_root = legacy
        .as_ref()
        .map(|p| fs::metadata(p).map(|m| file_id(&m)))
        .transpose()?;
    let mut nodes = BTreeMap::new();
    for entry in fs::read_dir(legacy.as_deref().unwrap_or(source))? {
        let entry = entry?;
        let name = entry.file_name();
        let original = source.join(&name);
        // The legacy executable placeholder and immutable aliases are not payload.
        // Everything else comes from the current private view, including new files.
        // Iterating that view (not the original) preserves deleted selected entries.
        let from_original = legacy.is_some()
            && (Some(name.as_os_str()) == app.file.file_name()
                || (!app
                    .writable_paths
                    .iter()
                    .any(|p| Path::new(p) == Path::new(&name))
                    && present(&original)?));
        walk(
            &if from_original {
                original
            } else {
                entry.path()
            },
            Path::new(&name),
            &mut nodes,
        )?;
    }
    if let Some(legacy) = &legacy {
        for (relative, node) in &mut nodes {
            if node.source.starts_with(legacy)
                && node.link.is_none()
                && node.identity.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
                && node.execute_bits == 0
            {
                // The legacy seeder used 0600 for all selected files. Recover
                // helper executability from the still-validated original while
                // preserving every current private byte.
                let original = source.join(relative);
                if present(&original)? {
                    no_links(&original)?;
                    let meta = fs::symlink_metadata(original)?;
                    if meta.is_file() {
                        node.execute_bits = meta.mode() & 0o111;
                    }
                }
            }
        }
    }
    anyhow::ensure!(
        file_id(&fs::metadata(source)?) == source_root
            && legacy
                .as_ref()
                .map(|p| fs::metadata(p).map(|m| file_id(&m)))
                .transpose()?
                == legacy_root,
        "Application directory changed during inspection"
    );
    validate_links(&nodes)?;
    Ok(Inventory {
        source_root,
        legacy_root,
        nodes,
    })
}

fn snapshot_hash(inventory: &Inventory) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(inventory)?)))
}

fn digest(file: &mut File) -> Result<Vec<u8>> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            return Ok(hash.finalize().to_vec());
        }
        hash.update(&buffer[..n]);
    }
}

fn reflink(input: &File, output: &File) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // FICLONE shares extents copy-on-write, not the source inode/permissions.
        if unsafe {
            libc::ioctl(
                output.as_raw_fd(),
                0x40049409 as libc::c_ulong,
                input.as_raw_fd(),
            )
        } == 0
        {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(
                libc::EXDEV
                    | libc::EOPNOTSUPP
                    | libc::ENOTTY
                    | libc::EINVAL
                    | libc::ENOSYS
                    | libc::EPERM
            )
        ) {
            return Err(error.into());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (input, output);
    // A failed unsupported clone never authorizes a hard link or a writable bind
    // of originals. Only this newly created private destination can be truncated.
    output.set_len(0)?;
    Ok(false)
}

fn copy_file(node: &Node, target: &Path, stats: &mut CopyStats) -> Result<()> {
    let mut input = open_read(&node.source)?;
    anyhow::ensure!(
        file_id(&input.metadata()?) == node.identity,
        "Source file was replaced before import"
    );
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)?;
    let cloned = reflink(&input, &output)?;
    let expected = if cloned {
        digest(&mut input)?
    } else {
        let mut hash = Sha256::new();
        let mut buffer = [0; 128 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
        }
        hash.finalize().to_vec()
    };
    // Owner write access is unconditional; preserve helper/library executable bits
    // but never import setuid, setgid, capabilities or source ownership.
    let execute = node.execute_bits;
    output.set_permissions(fs::Permissions::from_mode(
        0o600 | execute | if execute != 0 { 0o100 } else { 0 },
    ))?;
    output.sync_all()?;
    anyhow::ensure!(
        file_id(&input.metadata()?) == node.identity
            && file_id(&fs::symlink_metadata(&node.source)?) == node.identity
            && digest(&mut open_read(target)?)? == expected,
        "Application copy changed or failed SHA256 verification"
    );
    stats.files += 1;
    stats.bytes += node.identity.size;
    stats.reflink_files += u64::from(cloned);
    Ok(())
}

/// Caller holds the per-app lock through publication and application exit.
pub(crate) fn prepare(
    store: &Store,
    app: &App,
    source: &Path,
    executable: &Path,
) -> Result<Prepared> {
    relative(executable)?;
    let (base, published) = storage(store, app)?;
    if present(&published)? && app.writable_app {
        let receipt = read_receipt(&published, app)?;
        complete_publication(&published)?;
        return prepared(&published, &receipt);
    }
    no_links(&app.file)?;
    no_links(source)?;
    let home = store.data.join("app-data").join(&app.id);
    anyhow::ensure!(
        !base.starts_with(source)
            && !source.starts_with(&base)
            && !home.starts_with(source)
            && !source.starts_with(&home),
        "Place software in its own directory, outside its private runtime data"
    );
    let initial = inventory(store, app, source)?;
    let import_snapshot_sha256 = snapshot_hash(&initial)?;
    if present(&published)? {
        let receipt = read_receipt(&published, app)?;
        anyhow::ensure!(
            receipt.import_snapshot_sha256 == import_snapshot_sha256
                && receipt.legacy_paths == app.writable_paths,
            "Previously prepared migration no longer matches private data; both copies preserved"
        );
        complete_publication(&published)?;
        return prepared(&published, &receipt);
    }
    let staging = base.join(format!("seed-{}", uuid::Uuid::new_v4()));
    private_dir(&staging)?;
    eprintln!(
        "{}",
        serde_json::json!({
            "phase": "preparing-private-application",
            "appId": app.id,
            "entries": initial.nodes.len(),
            "message": "First launch copies application files; existing packages and saves stay unchanged"
        })
    );
    let result = (|| -> Result<Prepared> {
        let view = staging.join("view");
        private_dir(&view)?;
        let mut stats = CopyStats::default();
        for (relative, node) in &initial.nodes {
            let target = view.join(relative);
            if let Some(link) = &node.link {
                symlink(link, &target)?;
            } else if node.identity.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
                private_dir(&target)?;
            } else {
                copy_file(node, &target, &mut stats).with_context(|| {
                    format!("Cannot prepare {}; check free space. Original files and existing saves are unchanged", relative.display())
                })?;
            }
        }
        anyhow::ensure!(
            inventory(store, app, source)? == initial,
            "Source/private saves changed during import; refusing publication"
        );
        for (relative, node) in initial.nodes.iter().rev() {
            if node.identity.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
                File::open(view.join(relative))?.sync_all()?;
            }
        }
        File::open(&view)?.sync_all()?;
        let receipt = Receipt {
            schema: 1,
            app_id: app.id.clone(),
            original_file: app.file.clone(),
            appimage: app.appimage,
            executable: executable.to_owned(),
            source: source.to_owned(),
            import_snapshot_sha256,
            legacy_paths: app.writable_paths.clone(),
            stats,
        };
        // Validate the actual copied executable (including in-tree AppRun links)
        // before making a durable workspace visible.
        prepared(&staging, &receipt)?;
        atomic_json(&staging.join("manifest.json"), &receipt)?;
        File::open(&staging)?.sync_all()?;
        fs::rename(&staging, &published)?;
        complete_publication(&published)?;
        eprintln!(
            "{}",
            serde_json::json!({
                "phase": "private-application-ready",
                "appId": app.id,
                "dataRoot": published.join("view"),
                "copy": receipt.stats
            })
        );
        prepared(&published, &receipt)
    })();
    if result.is_err() && present(&staging)? {
        // Only our own unpublished staging data, never a published workspace.
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub fn configure(store: &Store, id: &str, approve: bool) -> Result<serde_json::Value> {
    anyhow::ensure!(
        approve,
        "Use --approve to import a fully writable private application"
    );
    let _lock = crate::portable::lock(store, id)?;
    let mut app = store.app(id)?;
    let workspace = if let Some(workspace) = existing(store, &app)? {
        workspace
    } else {
        let (source, entry) = crate::engine::source_files(store, &app)?;
        let relative = entry.strip_prefix(&source)?;
        prepare(store, &app, &source, relative)?
    };
    app.writable_app = true;
    app.writable_paths.clear();
    atomic_json(&store.data.join("apps").join(format!("{id}.json")), &app)?;
    Ok(serde_json::json!({
        "appId": app.id,
        "policy": "private-writable-application-v1",
        "dataRoot": workspace.directory,
        "executable": workspace.executable,
        "originalAssets": "unchanged",
        "runtimeImage": "read-only",
        "appDirectory": "fully-writable"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store, App, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let store = Store {
            data: root.join("data"),
            state: root.join("state"),
        };
        store.init().unwrap();
        let source = root.join("software");
        fs::create_dir_all(source.join("user")).unwrap();
        fs::write(source.join("main"), b"original-executable").unwrap();
        fs::set_permissions(source.join("main"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(source.join("assets.bin"), b"original-assets").unwrap();
        fs::write(source.join("user/save"), b"original-save").unwrap();
        fs::write(source.join("cache.bin"), b"original-cache").unwrap();
        let app = App {
            id: "full-app".into(),
            name: "Software".into(),
            file: source.join("main"),
            profile: crate::PROFILE.into(),
            appimage: false,
            writable_app: true,
            writable_paths: Vec::new(),
        };
        atomic_json(&store.data.join("apps/full-app.json"), &app).unwrap();
        (temp, store, app, source)
    }

    #[test]
    fn every_application_file_is_private_writable_and_executables_stay_executable() {
        let (_temp, store, app, source) = fixture();
        fs::set_permissions(source.join("main"), fs::Permissions::from_mode(0o4555)).unwrap();
        fs::set_permissions(source.join("assets.bin"), fs::Permissions::from_mode(0o400)).unwrap();
        let view = prepare(&store, &app, &source, Path::new("main")).unwrap();
        assert_eq!(view.executable, Path::new("/app/main"));
        for name in ["main", "assets.bin", "user/save", "cache.bin"] {
            let original = fs::symlink_metadata(source.join(name)).unwrap();
            let private = fs::symlink_metadata(view.directory.join(name)).unwrap();
            assert!(private.is_file() && !private.file_type().is_symlink());
            assert_ne!(
                (original.dev(), original.ino()),
                (private.dev(), private.ino())
            );
            assert_eq!(private.nlink(), 1);
            assert_ne!(private.mode() & 0o200, 0);
            assert_eq!(private.mode() & 0o7000, 0);
        }
        assert_ne!(
            fs::metadata(view.directory.join("main")).unwrap().mode() & 0o100,
            0
        );
        fs::write(view.directory.join("assets.bin"), b"app-can-modify-assets").unwrap();
        fs::write(view.directory.join("main"), b"app-can-update-executable").unwrap();
        fs::write(view.directory.join("new-file"), b"created").unwrap();
        assert_eq!(
            fs::read(source.join("assets.bin")).unwrap(),
            b"original-assets"
        );
        assert_eq!(
            fs::read(source.join("main")).unwrap(),
            b"original-executable"
        );
        assert!(!source.join("new-file").exists());
    }

    #[test]
    fn relaunch_and_dependency_inspection_survive_self_updates_and_missing_originals() {
        let (_temp, store, app, source) = fixture();
        let view = prepare(&store, &app, &source, Path::new("main")).unwrap();
        fs::write(view.directory.join("main"), b"updated-private-executable").unwrap();
        fs::write(view.directory.join("user/save"), b"new-progress").unwrap();
        fs::remove_file(view.directory.join("cache.bin")).unwrap();
        fs::rename(&source, source.with_extension("retained-original")).unwrap();
        let reused = existing(&store, &app).unwrap().unwrap();
        assert_eq!(reused.directory, view.directory);
        let (directory, entry) = crate::engine::dependency_files(&store, &app).unwrap();
        assert_eq!(directory, view.directory);
        assert_eq!(fs::read(entry).unwrap(), b"updated-private-executable");
        assert_eq!(
            fs::read(directory.join("user/save")).unwrap(),
            b"new-progress"
        );
        assert!(!directory.join("cache.bin").exists());
        assert!(configure(&store, &app.id, true).is_ok());
    }

    #[test]
    fn appimage_payloads_preserve_internal_links_and_helper_modes() {
        let (_temp, store, mut app, source) = fixture();
        app.appimage = true;
        app.file = source.parent().unwrap().join("package.AppImage");
        fs::write(&app.file, b"original-package").unwrap();
        fs::create_dir_all(source.join("usr/bin")).unwrap();
        fs::create_dir_all(source.join("usr/lib")).unwrap();
        fs::rename(source.join("main"), source.join("usr/bin/main")).unwrap();
        symlink("usr/bin/main", source.join("AppRun")).unwrap();
        symlink("../lib", source.join("usr/bin/libraries")).unwrap();
        let view = prepare(&store, &app, &source, Path::new("AppRun")).unwrap();
        assert_eq!(
            fs::read_link(view.directory.join("AppRun")).unwrap(),
            Path::new("usr/bin/main")
        );
        assert!(resolve_entry(&view.directory, Path::new("AppRun"))
            .unwrap()
            .is_file());
        assert!(existing(&store, &app).unwrap().is_some());
        assert_eq!(fs::read(&app.file).unwrap(), b"original-package");
    }

    #[test]
    fn escaping_composed_and_cyclic_links_fail_without_publication() {
        for (alias, target) in [
            (".", "alias/../outside"),
            ("inside", "../../outside"),
            (".", "/etc/passwd"),
            ("loop", "alias"),
        ] {
            let (_temp, store, app, source) = fixture();
            fs::create_dir(source.join("inside")).unwrap();
            symlink(alias, source.join("alias")).unwrap();
            symlink(target, source.join("loop")).unwrap();
            assert!(
                prepare(&store, &app, &source, Path::new("main")).is_err(),
                "{alias} {target}"
            );
            assert!(!store.data.join("workspaces/full-app").exists());
            assert_eq!(
                fs::read(source.join("user/save")).unwrap(),
                b"original-save"
            );
        }
    }

    #[test]
    fn mutated_private_entry_links_are_resolved_without_host_reads() {
        let (_temp, store, app, source) = fixture();
        let view = prepare(&store, &app, &source, Path::new("main")).unwrap();
        fs::remove_file(view.directory.join("main")).unwrap();
        symlink(".", view.directory.join("alias")).unwrap();
        symlink("alias/../outside", view.directory.join("main")).unwrap();
        assert!(existing(&store, &app).is_err());
        assert!(crate::engine::dependency_files(&store, &app).is_err());
        assert_eq!(
            fs::read(view.directory.join("user/save")).unwrap(),
            b"original-save"
        );
    }

    #[test]
    fn approved_legacy_migration_preserves_private_saves_deletions_and_new_files() {
        let (_temp, store, mut app, source) = fixture();
        app.writable_app = false;
        app.writable_paths = vec!["user".into(), "cache.bin".into()];
        fs::write(source.join("user/helper"), b"original-helper").unwrap();
        fs::set_permissions(
            source.join("user/helper"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        atomic_json(&store.data.join("apps/full-app.json"), &app).unwrap();
        let old = crate::portable::prepare(&store, &app).unwrap();
        fs::write(old.join("user/save"), b"current-private-progress").unwrap();
        fs::write(old.join("user/helper"), b"current-private-helper").unwrap();
        assert_eq!(
            fs::metadata(old.join("user/helper")).unwrap().mode() & 0o111,
            0
        );
        fs::write(old.join("future-cache"), b"new-private-cache").unwrap();
        fs::remove_file(old.join("cache.bin")).unwrap();
        assert!(configure(&store, &app.id, false).is_err());
        let result = configure(&store, &app.id, true).unwrap();
        assert_eq!(result["policy"], "private-writable-application-v1");
        let app = store.app(&app.id).unwrap();
        assert!(app.writable_app && app.writable_paths.is_empty());
        let view = existing(&store, &app).unwrap().unwrap().directory;
        assert_eq!(
            fs::read(view.join("user/save")).unwrap(),
            b"current-private-progress"
        );
        assert_eq!(
            fs::read(view.join("user/helper")).unwrap(),
            b"current-private-helper"
        );
        assert_ne!(
            fs::metadata(view.join("user/helper")).unwrap().mode() & 0o100,
            0
        );
        assert_eq!(
            fs::read(view.join("future-cache")).unwrap(),
            b"new-private-cache"
        );
        assert_eq!(fs::read(view.join("main")).unwrap(), b"original-executable");
        assert_eq!(
            fs::read(view.join("assets.bin")).unwrap(),
            b"original-assets"
        );
        assert!(!view.join("cache.bin").exists());
        assert_eq!(
            fs::read(old.join("user/save")).unwrap(),
            b"current-private-progress"
        );
        assert_eq!(
            fs::read(source.join("user/save")).unwrap(),
            b"original-save"
        );
        assert!(crate::portable::configure(&store, &app.id, vec!["user".into()], true).is_err());
    }

    #[test]
    fn migration_retry_never_hides_newer_legacy_progress_after_a_crash_gap() {
        let (_temp, store, mut app, source) = fixture();
        app.writable_app = false;
        app.writable_paths = vec!["user".into()];
        atomic_json(&store.data.join("apps/full-app.json"), &app).unwrap();
        let old = crate::portable::prepare(&store, &app).unwrap();
        let staged = prepare(&store, &app, &source, Path::new("main")).unwrap();
        // Simulate publication before the app record was durably switched.
        fs::write(old.join("user/save"), b"newer-legacy-progress").unwrap();
        assert!(configure(&store, &app.id, true).is_err());
        assert!(!store.app(&app.id).unwrap().writable_app);
        assert_eq!(
            fs::read(old.join("user/save")).unwrap(),
            b"newer-legacy-progress"
        );
        assert_eq!(
            fs::read(staged.directory.join("user/save")).unwrap(),
            b"original-save"
        );
    }

    #[test]
    fn unchanged_migration_can_complete_after_publication_before_policy_save() {
        let (_temp, store, mut app, source) = fixture();
        app.writable_app = false;
        atomic_json(&store.data.join("apps/full-app.json"), &app).unwrap();
        prepare(&store, &app, &source, Path::new("main")).unwrap();
        assert!(configure(&store, &app.id, true).is_ok());
        assert!(store.app(&app.id).unwrap().writable_app);
    }

    #[test]
    fn large_file_fallback_is_verified_and_source_replacement_is_rejected() {
        let (_temp, store, app, source) = fixture();
        let bytes: Vec<u8> = (0..400_001).map(|n| (n % 251) as u8).collect();
        fs::write(source.join("assets.bin"), &bytes).unwrap();
        let view = prepare(&store, &app, &source, Path::new("main")).unwrap();
        assert_eq!(fs::read(view.directory.join("assets.bin")).unwrap(), bytes);
        let initial = inventory(&store, &app, &source).unwrap();
        fs::write(source.join("assets.bin"), b"source-changed").unwrap();
        assert!(copy_file(
            &initial.nodes[Path::new("assets.bin")],
            &store.data.join("copy"),
            &mut CopyStats::default()
        )
        .is_err());
        assert!(!store.data.join("copy").exists());
    }

    #[test]
    fn storage_symlinks_overlaps_and_invalid_ids_fail_closed() {
        let (_temp, store, mut app, source) = fixture();
        let other = source.parent().unwrap().join("other");
        fs::create_dir(&other).unwrap();
        symlink(&other, store.data.join("workspaces")).unwrap();
        let mode = fs::metadata(&other).unwrap().mode();
        assert!(prepare(&store, &app, &source, Path::new("main")).is_err());
        assert_eq!(fs::metadata(&other).unwrap().mode(), mode);
        fs::remove_file(store.data.join("workspaces")).unwrap();
        assert!(prepare(
            &store,
            &app,
            source.parent().unwrap(),
            Path::new("software/main")
        )
        .is_err());
        app.id = "../escape".into();
        assert!(existing(&store, &app).is_err());
    }

    #[test]
    fn failed_import_never_publishes_or_overwrites_an_existing_workspace() {
        let (_temp, store, app, source) = fixture();
        assert!(prepare(&store, &app, &source, Path::new("missing")).is_err());
        assert!(!store.data.join("workspaces/full-app").exists());
        let view = prepare(&store, &app, &source, Path::new("main")).unwrap();
        fs::write(view.directory.join("user/save"), b"keep-this").unwrap();
        let mut changed = app.clone();
        changed.file = source.join("other");
        assert!(existing(&store, &changed).is_err());
        assert_eq!(
            fs::read(view.directory.join("user/save")).unwrap(),
            b"keep-this"
        );
    }

    #[test]
    fn full_configuration_uses_the_same_supervisor_lock() {
        let (_temp, store, app, _source) = fixture();
        let held = crate::portable::lock(&store, &app.id).unwrap();
        assert!(configure(&store, &app.id, true).is_err());
        drop(held);
        assert!(configure(&store, &app.id, true).is_ok());
    }
}
