//! Exact runtime registration -> application backup scope. Registry names and
//! executable basenames are never ownership evidence. These bindings are short
//! lived: revalidate before every complete reconciliation, including after exit.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use noland_state_core::{AppIdentity, Result, StateError};
use serde::Deserialize;

mod legacy_import;

#[derive(Debug, Clone)]
pub struct RuntimeBackupBinding {
    pub runtime_id: String,
    pub source_executable: PathBuf,
    pub private_home: PathBuf,
    pub view: Option<PathBuf>,
    /// Payload roots only: never the legacy placeholder or immutable aliases.
    pub data_roots: Vec<PathBuf>,
    /// Exact files, not their shared parent directories.
    pub control_files: Vec<PathBuf>,
    /// True for the legacy split view, whose executable remains the original.
    pub selected_view: bool,
    uid: u32,
    workspace_executable: Option<PathBuf>,
    generation: Vec<(PathBuf, Identity)>,
    excluded_view_entries: BTreeSet<String>,
    /// Exact historical keys covered by a pinned, verified native migration.
    /// This is not traversal or ownership authority over the old layout.
    retired_paths: BTreeSet<PathBuf>,
    /// A fully writable native workspace is the live installation. Its old
    /// import directory is not a second copy of the running application's data.
    /// The exact source ELF remains a registration/discovery anchor.
    superseded_source_root: Option<PathBuf>,
}

impl RuntimeBackupBinding {
    pub fn is_retired(&self, path: &Path) -> bool {
        self.retired_paths.contains(path)
    }

    pub fn is_superseded_source(&self, path: &Path) -> bool {
        literal_absolute(path)
            && path != self.source_executable
            && self
                .superseded_source_root
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
    }

    pub fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.data_roots.iter().chain(&self.control_files).chain(
            self.superseded_source_root
                .as_ref()
                .map(|_| &self.source_executable),
        )
    }

    /// Checks lexical scope and every existing ancestor without following links.
    /// Missing leaves are permitted for deletion/rename observations.
    pub fn permits(&self, path: &Path) -> bool {
        if !literal_absolute(path) {
            return false;
        }
        let root = self
            .data_roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .or_else(|| self.control_files.iter().find(|file| path == *file))
            .or_else(|| self.view.as_ref().filter(|_| self.contains(path)));
        root.is_some_and(|root| safe_descendant(path, root, self.uid).is_ok())
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.data_roots.iter().any(|root| path.starts_with(root))
            || self.control_files.iter().any(|file| path == file)
            || self.view.as_ref().is_some_and(|view| {
                path.strip_prefix(view).ok().is_some_and(|relative| {
                    relative.components().next().is_some_and(|first| {
                        first
                            .as_os_str()
                            .to_str()
                            .is_some_and(|name| !self.excluded_view_entries.contains(name))
                    })
                })
            })
    }

    pub fn same_generation(&self, other: &Self) -> bool {
        self.runtime_id == other.runtime_id
            && self.generation == other.generation
            && self.roots().collect::<Vec<_>>() == other.roots().collect::<Vec<_>>()
            && self.retired_paths == other.retired_paths
            && self.superseded_source_root == other.superseded_source_root
    }

    pub fn matches_executable(&self, path: &Path) -> bool {
        path == self.source_executable || self.workspace_executable.as_deref() == Some(path)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    id: String,
    file: PathBuf,
    profile: String,
    appimage: bool,
    #[serde(default)]
    writable_app: bool,
    #[serde(default)]
    writable_paths: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    mode: u32,
}

#[derive(Deserialize)]
struct SelectedReceipt {
    source: PathBuf,
    source_identity: Identity,
    writable: Vec<String>,
    entries: BTreeMap<String, Identity>,
    seeds: BTreeMap<PathBuf, Identity>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceReceipt {
    schema: u32,
    app_id: String,
    original_file: PathBuf,
    appimage: bool,
    executable: PathBuf,
    source: PathBuf,
    import_snapshot_sha256: String,
    #[serde(default)]
    legacy_paths: Vec<String>,
    #[serde(default)]
    legacy_import_sha256: Option<String>,
}

fn invalid() -> StateError {
    // Do not put registry contents or application payloads in diagnostics.
    StateError::Invalid(
        "runtime application backup binding is invalid or changed; private data was not modified"
            .into(),
    )
}

fn literal_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|text| {
            !text.chars().any(char::is_control)
                && !text.split('/').any(|part| matches!(part, "." | ".."))
        })
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.to_str().is_some_and(|text| {
            !text.chars().any(char::is_control)
                && !text.split('/').any(|part| matches!(part, "." | ".."))
        })
}

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(unix)]
fn uid(meta: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.uid()
}

#[cfg(not(unix))]
fn uid(_: &Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn identity(meta: &Metadata) -> Identity {
    use std::os::unix::fs::MetadataExt;
    Identity {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.len(),
        mtime: (meta.mtime(), meta.mtime_nsec()),
        ctime: (meta.ctime(), meta.ctime_nsec()),
        mode: meta.mode(),
    }
}

#[cfg(not(unix))]
fn identity(meta: &Metadata) -> Identity {
    Identity {
        dev: 0,
        ino: 0,
        size: meta.len(),
        mtime: (0, 0),
        ctime: (0, 0),
        mode: 0,
    }
}

fn no_links(path: &Path) -> Result<Metadata> {
    if !literal_absolute(path) {
        return Err(invalid());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(invalid());
        }
    }
    Ok(fs::symlink_metadata(path)?)
}

fn owned(path: &Path, owner: u32) -> Result<Metadata> {
    let meta = no_links(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if uid(&meta) != owner || meta.mode() & 0o022 != 0 {
            return Err(invalid());
        }
    }
    if !meta.is_file() && !meta.is_dir() {
        return Err(invalid());
    }
    Ok(meta)
}

fn safe_descendant(path: &Path, root: &Path, owner: u32) -> Result<()> {
    let mut current = root.to_path_buf();
    owned(root, owner)?;
    for component in path.strip_prefix(root).map_err(|_| invalid())?.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid());
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                // Links are captured as text by reconciliation, never followed.
                // Their target must stay inside the owned root.
                if meta.file_type().is_symlink() {
                    if current != path || uid(&meta) != owner {
                        return Err(invalid());
                    }
                    let target = fs::read_link(&current)?;
                    let relative_path = current.strip_prefix(root).map_err(|_| invalid())?;
                    if target.is_absolute()
                        || noland_state_core::validate_symlink_target_within_root(
                            &relative_path.to_string_lossy(),
                            target.to_str().ok_or_else(invalid)?,
                        )
                        .is_err()
                    {
                        return Err(invalid());
                    }
                } else if uid(&meta) != owner || (!meta.is_dir() && !meta.is_file()) {
                    return Err(invalid());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, owner: u32, max_bytes: u64) -> Result<T> {
    serde_json::from_slice(&read_control_bytes(path, owner, max_bytes)?).map_err(|_| invalid())
}

fn read_control_bytes(path: &Path, owner: u32, max_bytes: u64) -> Result<Vec<u8>> {
    let before = owned(path, owner)?;
    if !before.is_file() || before.len() > max_bytes {
        return Err(invalid());
    }
    let mut file = File::open(path)?;
    if identity(&file.metadata()?) != identity(&before) {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    (&mut file).take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes
        || identity(&file.metadata()?) != identity(&before)
        || identity(&owned(path, owner)?) != identity(&before)
    {
        return Err(invalid());
    }
    Ok(bytes)
}

fn validate_source(record: &Registration, owner: u32) -> Result<()> {
    let meta = owned(&record.file, owner)?;
    if !meta.is_file() {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o111 == 0 {
            return Err(invalid());
        }
    }
    // Match the runtime's registration contract using only a bounded ELF header,
    // not a hash/copy of the original application. A forged JSON file must not
    // turn an arbitrary credential or control file into a new application.
    let mut source = File::open(&record.file)?;
    if identity(&source.metadata()?) != identity(&meta) {
        return Err(invalid());
    }
    let mut header = [0u8; 20];
    source.read_exact(&mut header).map_err(|_| invalid())?;
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || header[18..20] != [62, 0]
        || record.appimage != (&header[8..11] == b"AI\x02")
        || identity(&source.metadata()?) != identity(&meta)
        || identity(&owned(&record.file, owner)?) != identity(&meta)
    {
        return Err(invalid());
    }
    Ok(())
}

fn existing_directory(path: &Path, owner: u32) -> Result<()> {
    if !owned(path, owner)?.is_dir() {
        return Err(invalid());
    }
    Ok(())
}

/// Discover source identities from bounded per-app registrations, without
/// walking another application's HOME/workspace. Call after repairing legacy
/// executable paths. Do not run the result through name-based normalization.
/// Publication/receipt validation remains mandatory before capturing any data.
pub fn registered_runtime_identities(
    home: &Path,
    existing: &[AppIdentity],
) -> Result<Vec<AppIdentity>> {
    let base = home.join(".local/share/noland-runtime");
    let registry = base.join("apps");
    let entries = match fs::read_dir(&registry) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let owner = uid(&no_links(home)?);
    existing_directory(&base, owner)?;
    existing_directory(&registry, owner)?;
    let mut identities = Vec::new();
    let mut sources = BTreeSet::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|part| part.to_str()) != Some("json") {
            continue;
        }
        let record: Registration = read_json(&path, owner, 64 * 1024)?;
        if !id(&record.id)
            || path.file_stem().and_then(|part| part.to_str()) != Some(record.id.as_str())
            || record.profile != "ubuntu24-x86_64"
            || !record.file.starts_with(home)
            || !literal_absolute(&record.file)
            || !sources.insert(record.file.clone())
        {
            return Err(invalid());
        }
        let source = match owned(&record.file, owner) {
            Ok(source) => source,
            // A stale registration must not prevent discovery of other apps.
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !source.is_file() {
            return Err(invalid());
        }
        validate_source(&record, owner)?;
        let owners = existing
            .iter()
            .filter(|app| app.canonical_executable.as_deref() == Some(record.file.as_path()))
            .collect::<Vec<_>>();
        let identity = match owners.as_slice() {
            [app] => (*app).clone(),
            [] => AppIdentity {
                canonical_executable: Some(record.file.clone()),
                launcher: Some(if record.appimage {
                    noland_state_core::LauncherKind::AppImage
                } else {
                    noland_state_core::LauncherKind::Native
                }),
                identity_confidence: 0.95,
                ..AppIdentity::new(
                    noland_state_core::AppId::launcher("runtime", &record.id),
                    record
                        .file
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Application"),
                )
            },
            _ => return Err(invalid()),
        };
        if existing.iter().any(|other| {
            other.app_id == identity.app_id
                && other.canonical_executable != identity.canonical_executable
        }) {
            return Err(invalid());
        }
        identities.push(identity);
    }
    Ok(identities)
}

/// Bounded lookup for a mount-verified private HOME. The returned source is not
/// attribution authority: the caller must find its exact indexed executable and
/// validate that application's complete `runtime_backup_binding`.
pub fn runtime_registration_source(home: &Path, private_home: &Path) -> Result<Option<PathBuf>> {
    let base = home.join(".local/share/noland-runtime");
    let Ok(relative) = private_home.strip_prefix(base.join("app-data")) else {
        return Ok(None);
    };
    let Some(runtime_id) = relative.to_str().filter(|value| id(value)) else {
        return Ok(None);
    };
    let owner = uid(&no_links(home)?);
    existing_directory(&base, owner)?;
    existing_directory(&base.join("apps"), owner)?;
    existing_directory(private_home, owner)?;
    let record: Registration = read_json(
        &base.join("apps").join(format!("{runtime_id}.json")),
        owner,
        64 * 1024,
    )?;
    if record.id != runtime_id
        || record.profile != "ubuntu24-x86_64"
        || !literal_absolute(&record.file)
        || !record.file.starts_with(home)
    {
        return Err(invalid());
    }
    Ok(Some(record.file))
}

/// Only registrations with an exact canonical executable match are considered.
/// A malformed matching publication is an error, not permission to fall back to
/// an apparently successful backup of the immutable original alone.
pub fn runtime_backup_binding(
    home: &Path,
    app: &AppIdentity,
) -> Result<Option<RuntimeBackupBinding>> {
    let Some(executable) = app.canonical_executable.as_deref() else {
        return Ok(None);
    };
    if !literal_absolute(executable) || !executable.starts_with(home) {
        return Ok(None);
    }
    let base = home.join(".local/share/noland-runtime");
    let registry = base.join("apps");
    let entries = match fs::read_dir(&registry) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let home = fs::canonicalize(home)?;
    let owner = uid(&fs::metadata(&home)?);
    existing_directory(&base, owner)?;
    existing_directory(&registry, owner)?;
    let mut found = None;
    for entry in entries {
        let entry = entry?;
        let record_path = entry.path();
        if record_path.extension().and_then(|part| part.to_str()) != Some("json") {
            continue;
        }
        // Read only bounded control records of other apps, never their payload.
        let record: Registration = read_json(&record_path, owner, 64 * 1024)?;
        if record.file != executable {
            continue;
        }
        let executable_meta = no_links(executable)?;
        if !executable_meta.is_file() || uid(&executable_meta) != owner {
            return Err(invalid());
        }
        validate_source(&record, owner)?;
        if found.is_some()
            || !id(&record.id)
            || record_path.file_stem().and_then(|part| part.to_str()) != Some(&record.id)
            || record.profile != "ubuntu24-x86_64"
        {
            return Err(invalid());
        }
        let mut binding = RuntimeBackupBinding {
            runtime_id: record.id.clone(),
            source_executable: executable.to_path_buf(),
            private_home: base.join("app-data").join(&record.id),
            view: None,
            data_roots: Vec::new(),
            control_files: vec![record_path],
            selected_view: !record.writable_app && !record.writable_paths.is_empty(),
            uid: owner,
            workspace_executable: None,
            generation: Vec::new(),
            excluded_view_entries: BTreeSet::new(),
            retired_paths: BTreeSet::new(),
            superseded_source_root: None,
        };
        let private_base = base.join("app-data");
        existing_directory(&private_base, owner)?;
        existing_directory(&binding.private_home, owner)?;
        binding.data_roots.push(binding.private_home.clone());
        if record.writable_app {
            workspace(&base, &record, &mut binding)?;
        } else if binding.selected_view {
            selected(&base, &record, &mut binding)?;
        }
        // A source must be a narrow, existing app installation (or AppImage),
        // never an arbitrary user's shared home/config/infrastructure directory.
        let source = record.file.parent().ok_or_else(invalid)?;
        if binding
            .view
            .as_ref()
            .is_some_and(|view| source.starts_with(view) || view.starts_with(source))
            || source == binding.private_home
            || binding.private_home.starts_with(source)
            || source.starts_with(&binding.private_home)
        {
            return Err(invalid());
        }
        binding.data_roots.sort();
        binding.data_roots.dedup();
        for control in &binding.control_files {
            binding
                .generation
                .push((control.clone(), identity(&owned(control, owner)?)));
        }
        binding
            .generation
            .push((record.file.clone(), identity(&no_links(&record.file)?)));
        for boundary in [Some(&binding.private_home), binding.view.as_ref()]
            .into_iter()
            .flatten()
        {
            let mut stamp = identity(&owned(boundary, owner)?);
            // Applications may create/delete files while running. Only boundary
            // replacement, not directory mtime, invalidates the binding.
            stamp.size = 0;
            stamp.mtime = (0, 0);
            stamp.ctime = (0, 0);
            binding.generation.push((boundary.clone(), stamp));
        }
        found = Some(binding);
    }
    Ok(found)
}

fn workspace(base: &Path, record: &Registration, binding: &mut RuntimeBackupBinding) -> Result<()> {
    let parent = base.join("workspaces");
    let published = parent.join(&record.id);
    existing_directory(&parent, binding.uid)?;
    existing_directory(&published, binding.uid)?;
    let control = published.join("manifest.json");
    let receipt: WorkspaceReceipt = read_json(&control, binding.uid, 64 * 1024)?;
    let expected_source = record.file.parent().ok_or_else(invalid)?;
    let appimage_source = receipt
        .source
        .strip_prefix(base.join("cache"))
        .ok()
        .and_then(|path| {
            let mut parts = path.components();
            let first = parts.next()?.as_os_str().to_str()?;
            let hash = first.strip_prefix("appimage-")?;
            (hash.len() == 64
                && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                && parts.next()?.as_os_str() == "squashfs-root"
                && parts.next().is_none())
            .then_some(())
        })
        .is_some();
    if receipt.schema != 1
        || receipt.app_id != record.id
        || receipt.original_file != record.file
        || receipt.appimage != record.appimage
        || !literal_absolute(&receipt.source)
        || (!record.appimage && receipt.source != expected_source)
        || (record.appimage && !appimage_source)
        || !relative(&receipt.executable)
        || receipt.import_snapshot_sha256.len() != 64
        || !receipt
            .import_snapshot_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !record.writable_paths.is_empty()
    {
        return Err(invalid());
    }
    let view = published.join("view");
    existing_directory(&view, binding.uid)?;
    let executable = view.join(&receipt.executable);
    // Source-independent reuse intentionally supports application updates.
    // Relative AppRun links are resolved only within this workspace.
    let resolved = resolve_owned_entry(
        &view,
        executable.strip_prefix(&view).map_err(|_| invalid())?,
        binding.uid,
    )?;
    if !owned(&resolved, binding.uid)?.is_file() {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if fs::metadata(&resolved)?.mode() & 0o111 == 0 {
            return Err(invalid());
        }
    }
    binding.view = Some(view.clone());
    binding.workspace_executable = Some(resolved);
    binding.data_roots.push(view);
    if !record.appimage {
        // Only the already-validated native import directory is superseded.
        // An AppImage's package directory is not its extracted workspace and
        // may contain unrelated software; never suppress that broad directory.
        binding.superseded_source_root = Some(receipt.source.clone());
    }
    legacy_import::validate(base, record, &receipt, binding)?;
    binding.control_files.push(control);
    Ok(())
}

fn selected(base: &Path, record: &Registration, binding: &mut RuntimeBackupBinding) -> Result<()> {
    if record.appimage {
        return Err(invalid());
    }
    let parent = base.join("portable");
    let published = parent.join(&record.id);
    existing_directory(&parent, binding.uid)?;
    existing_directory(&published, binding.uid)?;
    let control = published.join("manifest.json");
    let receipt: SelectedReceipt = read_json(&control, binding.uid, 16 * 1024 * 1024)?;
    let source = record.file.parent().ok_or_else(invalid)?;
    let source_identity = AppIdentity {
        canonical_executable: Some(record.file.clone()),
        launcher: Some(noland_state_core::LauncherKind::Native),
        ..AppIdentity::new(noland_state_core::AppId::desktop("runtime-source"), "")
    };
    let home = base
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(invalid)?;
    if crate::derive_owned_install_root(home, &source_identity).as_deref() != Some(source) {
        return Err(invalid());
    }
    let names = record.writable_paths.iter().collect::<BTreeSet<_>>();
    let executable_name = record.file.file_name().ok_or_else(invalid)?;
    if receipt.source != source
        || receipt.writable != record.writable_paths
        || names.len() != record.writable_paths.len()
        || record.writable_paths.iter().any(|name| {
            !relative(Path::new(name))
                || Path::new(name).components().count() != 1
                || Path::new(name).as_os_str() == executable_name
        })
        || identity(&no_links(source)?) != receipt.source_identity
    {
        return Err(invalid());
    }
    let mut actual = BTreeMap::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| invalid())?;
        actual.insert(name, identity(&no_links(&entry.path())?));
    }
    if actual != receipt.entries {
        return Err(invalid());
    }
    fn seed_tree(source: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Identity>) -> Result<()> {
        let meta = no_links(&source.join(path))?;
        if !meta.is_file() && !meta.is_dir() {
            return Err(invalid());
        }
        if meta.is_dir() {
            for entry in fs::read_dir(source.join(path))? {
                seed_tree(source, &path.join(entry?.file_name()), out)?;
            }
        }
        out.insert(path.to_path_buf(), identity(&meta));
        Ok(())
    }
    let mut seeds = BTreeMap::new();
    for name in &record.writable_paths {
        if receipt.entries.contains_key(name) {
            seed_tree(source, Path::new(name), &mut seeds)?;
        }
    }
    if seeds != receipt.seeds {
        return Err(invalid());
    }
    let view = published.join("view");
    existing_directory(&view, binding.uid)?;
    binding
        .excluded_view_entries
        .insert(executable_name.to_string_lossy().into_owned());
    for name in receipt.entries.keys() {
        let path = view.join(name);
        if Path::new(name).as_os_str() == executable_name {
            let meta = owned(&path, binding.uid)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err(invalid());
                }
            }
            if !meta.is_file() || meta.len() != 0 {
                return Err(invalid());
            }
        } else if !record.writable_paths.contains(name) {
            binding.excluded_view_entries.insert(name.clone());
            let meta = fs::symlink_metadata(&path)?;
            if !meta.file_type().is_symlink()
                || uid(&meta) != binding.uid
                || fs::read_link(path)? != Path::new("/home/runtime/.noland-source").join(name)
            {
                return Err(invalid());
            }
        }
    }
    binding
        .data_roots
        .extend(record.writable_paths.iter().map(|name| view.join(name)));
    for entry in fs::read_dir(&view)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| invalid())?;
        if Path::new(&name).as_os_str() == executable_name
            || (receipt.entries.contains_key(&name) && !record.writable_paths.contains(&name))
        {
            continue;
        }
        // Selected entries may have been deleted by the application. New private
        // entries are included too; never recover old bytes from the source.
        owned(&entry.path(), binding.uid)?;
        binding.data_roots.push(entry.path());
    }
    if identity(&no_links(source)?) != receipt.source_identity {
        return Err(invalid());
    }
    binding.view = Some(view);
    binding.control_files.push(control);
    Ok(())
}

fn resolve_owned_entry(root: &Path, entry: &Path, owner: u32) -> Result<PathBuf> {
    let mut pending =
        VecDeque::from_iter(entry.components().map(|part| part.as_os_str().to_owned()));
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            if !resolved.pop() {
                return Err(invalid());
            }
            continue;
        }
        let candidate = root.join(&resolved).join(&part);
        let meta = fs::symlink_metadata(&candidate)?;
        if uid(&meta) != owner {
            return Err(invalid());
        }
        if meta.file_type().is_symlink() {
            links += 1;
            let target = fs::read_link(&candidate)?;
            if links > 40 || target.is_absolute() || target.as_os_str().is_empty() {
                return Err(invalid());
            }
            let mut prefix =
                VecDeque::from_iter(target.components().map(|part| part.as_os_str().to_owned()));
            prefix.append(&mut pending);
            pending = prefix;
        } else {
            resolved.push(part);
        }
    }
    Ok(root.join(resolved))
}

#[cfg(test)]
mod tests;
