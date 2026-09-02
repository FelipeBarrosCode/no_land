//! Staged restore: prioritize, verify, materialize, apply, and roll back.

mod download;
mod planner;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;
use noland_cas::blake3_file;
use noland_crypto::MasterKey;
use noland_pack::PackIndexEntry;
use noland_snapshot::{create_view, discard, SnapshotView};
use noland_state_core::*;
use noland_state_db::StateDb;
use noland_storage::{read_committed_manifest, SharedStorageProvider};
use uuid::Uuid;

pub use download::{
    download_and_verify_to, prune_local_pack_cache, DownloadJournal, DownloadOptions,
    DownloadReport, PackCacheGcOptions, PackCacheGcReport, DEFAULT_MAX_PARALLEL_PACK_DOWNLOADS,
};
pub use planner::{
    embed_restore_plan, plan_restore_priorities, restore_priority, RestorePlanEntry,
    RestorePriority, RestorePriorityPlan, RestoreTarget, READY_TO_LAUNCH,
};

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub restore_id: Uuid,
    pub staging: PathBuf,
    /// Provider-agnostic immutable pack cache rooted under [`AgentPaths::cache`].
    pub pack_cache: PathBuf,
    pub manifest: BundleManifest,
    pub mode: RestoreMode,
}

impl RestorePlan {
    pub fn priority_plan(&self) -> RestorePriorityPlan {
        plan_restore_priorities(&self.manifest, self.mode)
    }
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
    let manifest = match read_committed_manifest(provider, master, app_id, bundle_id).await {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    fs::write(
        staging.join("manifest/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(RestorePlan {
        restore_id,
        staging,
        pack_cache: paths.cache.join("restore-packs"),
        manifest,
        mode,
    })
}

/// Downloads and verifies all selected restore data.
///
/// This compatibility entry point retains full-completion behavior while using the bounded,
/// cache-aware downloader.
pub async fn download_and_verify(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    pack_index: &[PackIndexEntry],
) -> Result<()> {
    download_and_verify_to(
        provider,
        master,
        plan,
        pack_index,
        RestoreTarget::Complete,
        DownloadOptions::default(),
        None,
    )
    .await
    .map(|_| ())
}

pub fn materialize_tree(plan: &RestorePlan, roots: &LogicalRootMap) -> Result<Vec<PathBuf>> {
    materialize_tree_to(plan, roots, RestoreTarget::Complete)
}

/// Materializes files through the requested milestone without buffering whole files in memory.
pub fn materialize_tree_to(
    plan: &RestorePlan,
    roots: &LogicalRootMap,
    target: RestoreTarget,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let tree = plan.staging.join("materialized/tree");
    fs::create_dir_all(&tree)?;
    let priority_plan = plan.priority_plan();
    for planned in priority_plan.entries_for(target) {
        let file = &plan.manifest.files[planned.manifest_index];
        validate_relative_path(&file.relative_path)?;
        let logical = file
            .logical_root_parsed()
            .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
        let staged = join_validated(
            &tree.join(sanitize_token(&file.logical_root)),
            &file.relative_path,
        )?;
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)?;
        }
        match file.file_type.as_str() {
            "directory" => fs::create_dir_all(&staged)?,
            "symlink" => {
                let symlink_target = file
                    .symlink_target
                    .as_deref()
                    .ok_or_else(|| StateError::Invalid("symlink missing target".into()))?;
                validate_symlink_target(symlink_target, roots, &logical)?;
                let already_materialized = fs::read_link(&staged)
                    .map(|existing| existing == Path::new(symlink_target))
                    .unwrap_or(false);
                if !already_materialized {
                    remove_staged_path(&staged)?;
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(symlink_target, &staged)?;
                }
            }
            "file" => {
                if materialized_file_is_valid(&staged, file) {
                    written.push(staged);
                    continue;
                }
                remove_staged_path(&staged)?;
                let partial = staged.with_extension(format!("noland-{}.partial", plan.restore_id));
                remove_staged_path(&partial)?;
                let mut output = fs::File::create(&partial)?;
                for chunk in &file.chunks {
                    let chunk_path = plan
                        .staging
                        .join("materialized/.chunks")
                        .join(chunk.hash.trim_start_matches("blake3:"));
                    let mut input = fs::File::open(chunk_path)?;
                    std::io::copy(&mut input, &mut output)?;
                }
                output.flush()?;
                drop(output);
                if !file.chunks.is_empty() && blake3_file(&partial)? != file.file_hash {
                    let _ = fs::remove_file(&partial);
                    return Err(StateError::Integrity(format!(
                        "file hash mismatch for {}",
                        file.relative_path
                    )));
                }
                fs::rename(&partial, &staged)?;
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
    Ok(written)
}

pub fn apply_restore(
    plan: &RestorePlan,
    roots: &LogicalRootMap,
    db: Option<&StateDb>,
) -> Result<Option<SnapshotView>> {
    apply_restore_to(plan, roots, db, RestoreTarget::Complete)
}

/// Applies files through the requested milestone. Tombstones remain deferred until full completion.
pub fn apply_restore_to(
    plan: &RestorePlan,
    roots: &LogicalRootMap,
    db: Option<&StateDb>,
    target: RestoreTarget,
) -> Result<Option<SnapshotView>> {
    let priority_plan = plan.priority_plan();
    let selected = priority_plan.entries_for(target).collect::<Vec<_>>();
    let mut identical = BTreeSet::new();
    let mut targets = Vec::new();
    for planned in &selected {
        let file = &plan.manifest.files[planned.manifest_index];
        validate_relative_path(&file.relative_path)?;
        let logical = file
            .logical_root_parsed()
            .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
        let dest_root = roots
            .resolve(&logical)
            .ok_or_else(|| StateError::NotFound(format!("logical root {}", file.logical_root)))?;
        let dest = join_validated(&dest_root, &file.relative_path)?;
        if target_file_is_identical(&dest, file) {
            identical.insert(planned.manifest_index);
        } else if dest.exists() {
            targets.push(dest);
        }
    }
    let rollback = if targets.is_empty() {
        None
    } else {
        Some(create_view(
            &plan.staging.join("pre_restore"),
            &targets,
            false,
        )?)
    };

    for planned in selected {
        let file = &plan.manifest.files[planned.manifest_index];
        if identical.contains(&planned.manifest_index) {
            continue;
        }
        let logical = file
            .logical_root_parsed()
            .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
        let dest_root = roots
            .resolve(&logical)
            .ok_or_else(|| StateError::NotFound(format!("logical root {}", file.logical_root)))?;
        let dest = join_validated(&dest_root, &file.relative_path)?;
        let staged = plan
            .staging
            .join("materialized/tree")
            .join(sanitize_token(&file.logical_root))
            .join(&file.relative_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if staged.is_dir() {
            fs::create_dir_all(&dest)?;
        } else if staged.exists() {
            if dest.exists() && !dest.is_dir() {
                fs::remove_file(&dest)?;
            }
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

    if target == RestoreTarget::Complete {
        for tomb in &plan.manifest.tombstones {
            validate_relative_path(&tomb.relative_path)?;
            if let Some(root) =
                LogicalRoot::parse(&tomb.logical_root).and_then(|root| roots.resolve(&root))
            {
                let dest = join_validated(&root, &tomb.relative_path)?;
                if dest.exists() && dest.is_file() {
                    let _ = fs::remove_file(dest);
                }
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

fn materialized_file_is_valid(path: &Path, file: &ManifestFile) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() == file.size)
        .unwrap_or(false)
        && (file.chunks.is_empty()
            || blake3_file(path)
                .map(|hash| hash == file.file_hash)
                .unwrap_or(false))
}

fn target_file_is_identical(path: &Path, file: &ManifestFile) -> bool {
    file.file_type == "file"
        && fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.len() == file.size)
            .unwrap_or(false)
        && blake3_file(path)
            .map(|hash| hash == file.file_hash)
            .unwrap_or(false)
}

fn remove_staged_path(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn include_file(mode: RestoreMode, file: &ManifestFile) -> bool {
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

fn validate_symlink_target(
    target: &str,
    _roots: &LogicalRootMap,
    _root: &LogicalRoot,
) -> Result<()> {
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
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    use noland_pack::pack_chunks;
    use noland_storage::LocalStorage;

    use super::*;

    #[test]
    fn rejects_traversal_entries() {
        assert!(validate_relative_path("../etc/passwd").is_err());
        assert!(validate_relative_path("ok/file").is_ok());
    }

    #[test]
    fn priority_plan_is_tiered_and_deterministic() {
        let mut manifest = test_manifest(vec![
            test_file(
                "z-cache",
                PersistenceClass::PersistentState,
                SemanticRole::Cache,
                b"cache",
            ),
            test_file(
                "b-user",
                PersistenceClass::PersistentState,
                SemanticRole::UserState,
                b"user",
            ),
            test_file(
                "a-runtime",
                PersistenceClass::SharedState,
                SemanticRole::SharedRuntime,
                b"runtime",
            ),
            test_file(
                "c-unknown",
                PersistenceClass::SharedState,
                SemanticRole::Unknown,
                b"soon",
            ),
        ]);
        let first = plan_restore_priorities(&manifest, RestoreMode::CompleteApplication);
        let order = first
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.priority,
                    manifest.files[entry.manifest_index].relative_path.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![
                (RestorePriority::Prerequisite, "a-runtime".to_string()),
                (RestorePriority::Critical, "b-user".to_string()),
                (RestorePriority::Soon, "c-unknown".to_string()),
                (RestorePriority::Background, "z-cache".to_string()),
            ]
        );

        manifest.files.reverse();
        let second = plan_restore_priorities(&manifest, RestoreMode::CompleteApplication);
        let second_order = second
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.priority,
                    manifest.files[entry.manifest_index].relative_path.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(order, second_order);
    }

    #[test]
    fn ready_to_launch_materializes_and_applies_only_critical_tiers() {
        let root = test_dir("ready");
        let critical = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let background = test_file(
            "game/cache.bin",
            PersistenceClass::Ephemeral,
            SemanticRole::Cache,
            b"cache",
        );
        let manifest = test_manifest(vec![background.clone(), critical.clone()]);
        let plan = test_plan(&root, manifest);
        seed_chunks(&plan, &[&critical, &background]);
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);

        let written = materialize_tree_to(&plan, &roots, READY_TO_LAUNCH).unwrap();
        assert_eq!(written.len(), 1);
        assert!(written[0].ends_with("game/save.dat"));
        assert!(!plan
            .staging
            .join("materialized/tree/_XDG_DATA_HOME/game/cache.bin")
            .exists());

        apply_restore_to(&plan, &roots, None, READY_TO_LAUNCH).unwrap();
        assert_eq!(
            fs::read(home.join(".local/share/game/save.dat")).unwrap(),
            b"save"
        );
        assert!(!home.join(".local/share/game/cache.bin").exists());

        materialize_tree(&plan, &roots).unwrap();
        apply_restore(&plan, &roots, None).unwrap();
        assert_eq!(
            fs::read(home.join(".local/share/game/cache.bin")).unwrap(),
            b"cache"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn identical_target_skips_snapshot_and_rewrite() {
        let root = test_dir("identical-target");
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = test_plan(&root, test_manifest(vec![file]));
        let home = root.join("home");
        let target = home.join(".local/share/game/save.dat");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"save").unwrap();
        let before = fs::metadata(&target).unwrap().modified().unwrap();

        let rollback = apply_restore(&plan, &LogicalRootMap::from_home(&home), None).unwrap();

        assert!(rollback.is_none());
        assert_eq!(fs::read(&target).unwrap(), b"save");
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), before);
        assert!(fs::read_dir(plan.staging.join("pre_restore"))
            .unwrap()
            .next()
            .is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn materialization_reuses_verified_file_when_chunks_are_gone() {
        let root = test_dir("materialize-resume");
        let payload = [vec![b'a'; 1024 * 1024], vec![b'b'; 1024 * 1024]].concat();
        let mut file = test_file(
            "game/large.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            &payload,
        );
        file.chunks = vec![
            ChunkRef {
                hash: noland_cas::blake3_hex(&payload[..1024 * 1024]),
                size: 1024 * 1024,
            },
            ChunkRef {
                hash: noland_cas::blake3_hex(&payload[1024 * 1024..]),
                size: 1024 * 1024,
            },
        ];
        let plan = test_plan(&root, test_manifest(vec![file.clone()]));
        seed_chunks(&plan, &[&file]);
        let roots = LogicalRootMap::from_home(root.join("home"));

        materialize_tree(&plan, &roots).unwrap();
        fs::remove_dir_all(plan.staging.join("materialized/.chunks")).unwrap();
        materialize_tree(&plan, &roots).unwrap();

        let staged = plan
            .staging
            .join("materialized/tree/_XDG_DATA_HOME/game/large.dat");
        assert_eq!(fs::metadata(&staged).unwrap().len(), payload.len() as u64);
        assert_eq!(
            blake3_file(&staged).unwrap(),
            noland_cas::blake3_hex(&payload)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn downloaded_pack_cache_is_reused_after_remote_disappears() {
        let root = test_dir("pack-cache");
        let remote = root.join("remote");
        let built = root.join("built");
        let master = MasterKey::generate();
        let payload = b"portable-state".to_vec();
        let background_payload = b"rebuildable-cache".to_vec();
        let hash = noland_cas::blake3_hex(&payload);
        let background_hash = noland_cas::blake3_hex(&background_payload);
        let packs = pack_chunks(
            &built,
            &master,
            vec![
                (hash.clone(), payload.clone()),
                (background_hash.clone(), background_payload.clone()),
            ],
            |_| false,
        )
        .unwrap();
        let pack = &packs[0];
        let remote_pack = remote.join(noland_state_core::pack_key(&pack.pack_id));
        fs::create_dir_all(remote_pack.parent().unwrap()).unwrap();
        fs::copy(&pack.path, &remote_pack).unwrap();

        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            &payload,
        );
        let background = test_file(
            "game/cache.bin",
            PersistenceClass::Ephemeral,
            SemanticRole::Cache,
            &background_payload,
        );
        let manifest = test_manifest(vec![background, file]);
        let first = test_plan(&root.join("first"), manifest.clone());
        let storage = LocalStorage::new(&remote);
        let first_report = block_on(download_and_verify_to(
            &storage,
            &master,
            &first,
            &pack.entries,
            READY_TO_LAUNCH,
            DownloadOptions {
                max_parallel_packs: 2,
            },
            None,
        ))
        .unwrap();
        assert_eq!(first_report.packs_downloaded, 1);
        assert_eq!(first_report.chunks_extracted, 1);
        assert!(!first
            .staging
            .join("materialized/.chunks")
            .join(background_hash.trim_start_matches("blake3:"))
            .exists());

        fs::remove_file(remote_pack).unwrap();
        let mut second = test_plan(&root.join("second"), manifest);
        second.pack_cache = first.pack_cache.clone();
        let second_report = block_on(download_and_verify_to(
            &storage,
            &master,
            &second,
            &pack.entries,
            RestoreTarget::Complete,
            DownloadOptions::default(),
            None,
        ))
        .unwrap();
        assert_eq!(second_report.packs_downloaded, 0);
        assert_eq!(second_report.packs_reused, 1);
        assert_eq!(second_report.chunks_extracted, 2);
        let _ = fs::remove_dir_all(root);
    }

    fn test_manifest(files: Vec<ManifestFile>) -> BundleManifest {
        let mut manifest = BundleManifest::new(
            ManifestApp {
                app_id: AppId::desktop("restore-test"),
                display_name: "Restore Test".into(),
                aliases: Vec::new(),
                desktop_entry_id: None,
                steam_app_id: None,
                launcher: None,
                canonical_executable: None,
                icon_path: None,
            },
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "test".into(),
                captured_at: Utc::now(),
            },
            BackupMode::CompleteApplication,
        );
        manifest.files = files;
        manifest
    }

    fn test_file(
        path: &str,
        persistence_class: PersistenceClass,
        semantic_role: SemanticRole,
        payload: &[u8],
    ) -> ManifestFile {
        let hash = noland_cas::blake3_hex(payload);
        ManifestFile {
            logical_root: "$XDG_DATA_HOME".into(),
            relative_path: path.into(),
            source_path_hint: None,
            file_type: "file".into(),
            size: payload.len() as u64,
            file_hash: hash.clone(),
            chunks: vec![ChunkRef {
                hash,
                size: payload.len() as u64,
            }],
            mode: None,
            mtime_ns: None,
            uid: None,
            gid: None,
            symlink_target: None,
            persistence_class,
            semantic_role,
            association_confidence: 1.0,
            shared_app_ids: Vec::new(),
        }
    }

    fn test_plan(root: &Path, manifest: BundleManifest) -> RestorePlan {
        let staging = root.join("restore");
        for child in [
            "packs",
            "materialized/.chunks",
            "materialized/tree",
            "pre_restore",
        ] {
            fs::create_dir_all(staging.join(child)).unwrap();
        }
        RestorePlan {
            restore_id: Uuid::new_v4(),
            staging,
            pack_cache: root.join("cache/restore-packs"),
            manifest,
            mode: RestoreMode::CompleteApplication,
        }
    }

    fn seed_chunks(plan: &RestorePlan, files: &[&ManifestFile]) {
        for file in files {
            let payload = match file.relative_path.as_str() {
                "game/save.dat" => b"save".to_vec(),
                "game/cache.bin" => b"cache".to_vec(),
                "game/large.dat" => [vec![b'a'; 1024 * 1024], vec![b'b'; 1024 * 1024]].concat(),
                other => panic!("no test payload for {other}"),
            };
            let mut offset = 0;
            for chunk in &file.chunks {
                let end = offset + chunk.size as usize;
                let bytes = &payload[offset..end];
                assert_eq!(chunk.hash, noland_cas::blake3_hex(bytes));
                fs::write(
                    plan.staging
                        .join("materialized/.chunks")
                        .join(chunk.hash.trim_start_matches("blake3:")),
                    bytes,
                )
                .unwrap();
                offset = end;
            }
            assert_eq!(offset, payload.len());
        }
    }

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noland-restore-{label}-{}", Uuid::new_v4()))
    }

    struct NoopWaker;

    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
