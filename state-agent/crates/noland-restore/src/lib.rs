//! Staged restore: verify, materialize, apply, roll back.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;
use noland_cas::blake3_file;
use noland_crypto::MasterKey;
use noland_pack::{extract_chunk, PackIndexEntry};
use noland_snapshot::{create_view, discard, SnapshotView};
use noland_state_core::*;
use noland_state_db::StateDb;
use noland_state_core::pack_key as remote_pack_key;
use noland_storage::{read_committed_manifest, RemoteKey, SharedStorageProvider};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub restore_id: Uuid,
    pub staging: PathBuf,
    pub manifest: BundleManifest,
    pub mode: RestoreMode,
}

pub async fn prepare_restore(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    paths: &AgentPaths,
    app_id: &AppId,
    bundle_id: Uuid,
    mode: RestoreMode,
) -> Result<RestorePlan> {
    let restore_id = Uuid::new_v4();
    let staging = paths.restore_dir(&restore_id.to_string());
    for child in ["manifest", "packs", "materialized", "pre_restore", "logs"] {
        fs::create_dir_all(staging.join(child))?;
    }
    let manifest = read_committed_manifest(provider, master, app_id, bundle_id).await?;
    fs::write(
        staging.join("manifest/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(RestorePlan {
        restore_id,
        staging,
        manifest,
        mode,
    })
}

pub async fn download_and_verify(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    pack_index: &[PackIndexEntry],
) -> Result<()> {
    let mut needed = std::collections::BTreeSet::new();
    for file in &plan.manifest.files {
        for chunk in &file.chunks {
            needed.insert(chunk.hash.clone());
        }
    }
    for entry in pack_index {
        if !needed.contains(&entry.chunk_hash) {
            continue;
        }
        let dest = plan.staging.join("packs").join(format!("{}.pack", entry.pack_id));
        if !dest.exists() {
            provider
                .download(&RemoteKey::new(remote_pack_key(&entry.pack_id)), &dest)
                .await?;
        }
        let plain = extract_chunk(&dest, entry, master)?;
        if noland_cas::blake3_hex(&plain) != entry.chunk_hash {
            return Err(StateError::Integrity(format!(
                "chunk {} failed BLAKE3 verification",
                entry.chunk_hash
            )));
        }
        let chunk_path = plan
            .staging
            .join("materialized")
            .join(".chunks")
            .join(entry.chunk_hash.trim_start_matches("blake3:"));
        if let Some(parent) = chunk_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(chunk_path, plain)?;
    }
    Ok(())
}

pub fn materialize_tree(plan: &RestorePlan, roots: &LogicalRootMap) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let tree = plan.staging.join("materialized/tree");
    fs::create_dir_all(&tree)?;
    for file in &plan.manifest.files {
        if !include_file(plan.mode, file) {
            continue;
        }
        validate_relative_path(&file.relative_path)?;
        let logical = file
            .logical_root_parsed()
            .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
        // Materialize under the staging tree using the logical token so apply can remap.
        let staged = join_validated(&tree.join(sanitize_token(&file.logical_root)), &file.relative_path)?;
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)?;
        }
        match file.file_type.as_str() {
            "directory" => {
                fs::create_dir_all(&staged)?;
            }
            "symlink" => {
                let target = file
                    .symlink_target
                    .as_deref()
                    .ok_or_else(|| StateError::Invalid("symlink missing target".into()))?;
                validate_symlink_target(target, roots, &logical)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &staged)?;
            }
            "file" => {
                if file.file_type == "other" {
                    return Err(StateError::UnsafePath("special files are not restored".into()));
                }
                let mut data = Vec::new();
                for chunk in &file.chunks {
                    let chunk_path = plan
                        .staging
                        .join("materialized/.chunks")
                        .join(chunk.hash.trim_start_matches("blake3:"));
                    data.extend(fs::read(chunk_path)?);
                }
                if noland_cas::blake3_hex(&data) != file.file_hash && !file.chunks.is_empty() {
                    return Err(StateError::Integrity(format!(
                        "file hash mismatch for {}",
                        file.relative_path
                    )));
                }
                fs::write(&staged, data)?;
                if let Some(mode) = file.mode {
                    let _ = fs::set_permissions(&staged, fs::Permissions::from_mode(mode));
                }
            }
            other => {
                return Err(StateError::UnsafePath(format!(
                    "unsupported file type '{other}'"
                )));
            }
        }
        written.push(staged);
    }
    let _ = roots;
    Ok(written)
}

pub fn apply_restore(
    plan: &RestorePlan,
    roots: &LogicalRootMap,
    db: Option<&StateDb>,
) -> Result<Option<SnapshotView>> {
    let mut targets = Vec::new();
    for file in &plan.manifest.files {
        if !include_file(plan.mode, file) {
            continue;
        }
        validate_relative_path(&file.relative_path)?;
        let logical = file
            .logical_root_parsed()
            .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
        let dest_root = roots
            .resolve(&logical)
            .ok_or_else(|| StateError::NotFound(format!("logical root {}", file.logical_root)))?;
        let dest = join_validated(&dest_root, &file.relative_path)?;
        if dest.exists() {
            targets.push(dest);
        }
    }
    let rollback = if targets.is_empty() {
        None
    } else {
        Some(create_view(&plan.staging.join("pre_restore"), &targets, false)?)
    };

    for file in &plan.manifest.files {
        if !include_file(plan.mode, file) {
            continue;
        }
        let logical = file.logical_root_parsed().unwrap();
        let dest_root = roots.resolve(&logical).unwrap();
        let dest = join_validated(&dest_root, &file.relative_path)?;
        let staged = plan
            .staging
            .join("materialized/tree")
            .join(sanitize_token(&file.logical_root))
            .join(&file.relative_path);
        if dest.exists() {
            if dest.is_file() {
                if let Ok(hash) = blake3_file(&dest) {
                    if hash == file.file_hash {
                        continue;
                    }
                }
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if dest.exists() {
            if dest.is_dir() {
                // overlay into existing dir
            } else {
                fs::remove_file(&dest)?;
            }
        }
        if staged.is_dir() {
            fs::create_dir_all(&dest)?;
        } else if staged.exists() {
            fs::copy(&staged, &dest)?;
        }
        if let Some(db) = db {
            let path_id = db.upsert_path(&dest.to_string_lossy())?;
            let now = Utc::now();
            db.upsert_association(&PathAssociation {
                app_id: plan.manifest.app.app_id.clone(),
                path_id,
                confidence: CONF_EXPLICIT,
                evidence: vec![Evidence::new(EvidenceKind::RestoredFromCommittedBundle)],
                persistence_class: file.persistence_class,
                semantic_role: file.semantic_role,
                first_seen_at: now,
                last_seen_at: now,
            })?;
        }
    }

    for tomb in &plan.manifest.tombstones {
        validate_relative_path(&tomb.relative_path)?;
        if let Some(root) = LogicalRoot::parse(&tomb.logical_root).and_then(|r| roots.resolve(&r)) {
            let dest = join_validated(&root, &tomb.relative_path)?;
            if dest.exists() && dest.is_file() {
                let _ = fs::remove_file(dest);
            }
        }
    }
    Ok(rollback)
}

pub fn rollback_restore(rollback: &SnapshotView) -> Result<()> {
    for mapping in &rollback.mappings {
        if mapping.staged.exists() {
            if let Some(parent) = mapping.source.parent() {
                fs::create_dir_all(parent)?;
            }
            if mapping.staged.is_dir() {
                if mapping.source.exists() {
                    fs::remove_dir_all(&mapping.source)?;
                }
                copy_dir(&mapping.staged, &mapping.source)?;
            } else {
                fs::copy(&mapping.staged, &mapping.source)?;
            }
        }
    }
    Ok(())
}

pub fn cleanup_restore(plan: &RestorePlan, rollback: Option<SnapshotView>) -> Result<()> {
    if let Some(view) = rollback {
        discard(&view)?;
    }
    if plan.staging.exists() {
        fs::remove_dir_all(&plan.staging)?;
    }
    Ok(())
}

fn include_file(mode: RestoreMode, file: &ManifestFile) -> bool {
    match mode {
        RestoreMode::CompleteApplication | RestoreMode::Custom => true,
        RestoreMode::PersonalState => {
            matches!(
                file.persistence_class,
                PersistenceClass::PersistentState | PersistenceClass::SharedState
            ) || file.semantic_role == SemanticRole::UserState
                || file.semantic_role == SemanticRole::Secret
        }
    }
}

fn sanitize_token(token: &str) -> String {
    token.replace(['$', ':', '/'], "_")
}

fn validate_symlink_target(target: &str, _roots: &LogicalRootMap, _root: &LogicalRoot) -> Result<()> {
    if target.starts_with("/proc")
        || target.starts_with("/sys")
        || target.starts_with("/dev")
        || target.contains("..")
    {
        return Err(StateError::UnsafePath(format!(
            "symlink target rejected: {target}"
        )));
    }
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_entries() {
        assert!(validate_relative_path("../etc/passwd").is_err());
        assert!(validate_relative_path("ok/file").is_ok());
    }
}
