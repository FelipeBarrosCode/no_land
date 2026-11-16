//! A native migration can retire exact old index keys without deleting history
//! or allowing those paths to authorize a future scan. The current workspace
//! receipt pins this proof; neither an old registration nor a basename suffices.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyImport {
    schema: u32,
    app_id: String,
    original_file: PathBuf,
    import_snapshot_sha256: String,
    legacy_manifest_identity: Identity,
    legacy_view_identity: Identity,
    entries: BTreeMap<PathBuf, Identity>,
}

fn optional_owned(path: &Path, owner: u32) -> Result<Option<Metadata>> {
    match owned(path, owner) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn verify_directory_entries(
    view: &Path,
    manifest: &Path,
    record: &Registration,
    workspace: &WorkspaceReceipt,
    proof: &LegacyImport,
    owner: u32,
) -> Result<()> {
    // Directory timestamps may have coarse resolution. Comparing identities
    // alone cannot prove that no extra file was created in the same clock tick.
    // Check exact child sets without following immutable aliases or reading
    // anything outside this already-recorded application's old view.
    let selected: SelectedReceipt = read_json(manifest, owner, 16 * 1024 * 1024)?;
    if selected.source != record.file.parent().ok_or_else(invalid)?
        || selected.writable != workspace.legacy_paths
    {
        return Err(invalid());
    }
    let executable = record.file.file_name().ok_or_else(invalid)?;
    let mut children = BTreeMap::<PathBuf, BTreeSet<std::ffi::OsString>>::new();
    children.insert(PathBuf::new(), BTreeSet::new());
    for name in selected.entries.keys() {
        if !relative(Path::new(name)) || Path::new(name).components().count() != 1 {
            return Err(invalid());
        }
        if selected.writable.contains(name) {
            continue;
        }
        let path = view.join(name);
        let meta = fs::symlink_metadata(&path)?;
        if uid(&meta) != owner {
            return Err(invalid());
        }
        if Path::new(name).as_os_str() == executable {
            if !meta.is_file() || meta.len() != 0 {
                return Err(invalid());
            }
        } else if !meta.file_type().is_symlink()
            || fs::read_link(&path)? != Path::new("/home/runtime/.noland-source").join(name)
        {
            return Err(invalid());
        }
        children
            .get_mut(Path::new(""))
            .ok_or_else(invalid)?
            .insert(name.as_str().into());
    }
    for path in proof.entries.keys() {
        let parent = path.parent().ok_or_else(invalid)?;
        if !parent.as_os_str().is_empty() && !proof.entries.contains_key(parent) {
            return Err(invalid());
        }
        if parent.as_os_str().is_empty()
            && children
                .get(Path::new(""))
                .is_some_and(|names| names.contains(path.as_os_str()))
        {
            return Err(invalid());
        }
        children
            .entry(parent.to_path_buf())
            .or_default()
            .insert(path.file_name().ok_or_else(invalid)?.to_owned());
        if fs::symlink_metadata(view.join(path))?.is_dir() {
            children.entry(path.clone()).or_default();
        }
    }
    for (relative, expected) in children {
        let path = view.join(relative);
        safe_descendant(&path, view, owner)?;
        let before = owned(&path, owner)?;
        if !before.is_dir() {
            return Err(invalid());
        }
        let actual = fs::read_dir(&path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        if actual != expected || identity(&owned(&path, owner)?) != identity(&before) {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn validate(
    base: &Path,
    record: &Registration,
    workspace: &WorkspaceReceipt,
    binding: &mut RuntimeBackupBinding,
) -> Result<()> {
    let Some(expected) = &workspace.legacy_import_sha256 else {
        // Legacy workspaces remain readable, but cannot retire historical data.
        return Ok(());
    };
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let proof_path = base
        .join("workspaces")
        .join(&record.id)
        .join("legacy-import.json");
    let bytes = read_control_bytes(&proof_path, binding.uid, 16 * 1024 * 1024)?;
    if format!("{:x}", Sha256::digest(&bytes)) != *expected {
        return Err(invalid());
    }
    let proof: LegacyImport = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if proof.schema != 1
        || proof.app_id != record.id
        || proof.original_file != record.file
        || proof.import_snapshot_sha256 != workspace.import_snapshot_sha256
        || record.appimage
        || proof.entries.len() > 65_536
        || proof.entries.keys().any(|path| !relative(path))
    {
        return Err(invalid());
    }
    let legacy = base.join("portable").join(&record.id);
    let view = legacy.join("view");
    let manifest = legacy.join("manifest.json");
    let view_metadata = optional_owned(&view, binding.uid)?;
    let manifest_metadata = optional_owned(&manifest, binding.uid)?;
    match (view_metadata, manifest_metadata) {
        (Some(view_metadata), Some(manifest_metadata)) => {
            // An application writing to the retired view after migration is a
            // genuine unresolved change, not permission to silently omit it.
            if !view_metadata.is_dir()
                || !manifest_metadata.is_file()
                || identity(&view_metadata) != proof.legacy_view_identity
                || identity(&manifest_metadata) != proof.legacy_manifest_identity
            {
                return Err(invalid());
            }
            binding
                .generation
                .push((view.clone(), identity(&view_metadata)));
            binding
                .generation
                .push((manifest.clone(), identity(&manifest_metadata)));
            for (relative, expected) in &proof.entries {
                let path = view.join(relative);
                safe_descendant(&path, &view, binding.uid)?;
                let actual = fs::symlink_metadata(&path)?;
                if identity(&actual) != *expected {
                    return Err(invalid());
                }
                binding.generation.push((path, identity(&actual)));
            }
            verify_directory_entries(&view, &manifest, record, workspace, &proof, binding.uid)?;
        }
        // New backups deliberately omit retired payload and the obsolete
        // receipt. Its absence after restore must not require recreating it.
        (None, None) => {}
        _ => return Err(invalid()),
    }
    binding.retired_paths.insert(view.clone());
    binding.retired_paths.insert(manifest);
    binding
        .retired_paths
        .extend(proof.entries.keys().map(|path| view.join(path)));
    binding.control_files.push(proof_path);
    Ok(())
}
