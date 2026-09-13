//! Storage-efficient restore: prioritize, verify, atomically publish, and roll back.

mod download;
mod planner;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use noland_cas::blake3_file;
use noland_crypto::MasterKey;
use noland_pack::PackIndexEntry;
use noland_snapshot::{create_view, SnapshotView};
use noland_state_core::*;
use noland_state_db::{RestoredPathAssociationInput, StateDb};
use noland_storage::{read_committed_manifest, SharedStorageProvider};
use uuid::Uuid;

pub use download::{
    download_and_verify_to, planned_pack_download_count, prune_local_pack_cache, DownloadJournal,
    DownloadOptions, DownloadReport, PackCacheGcOptions, PackCacheGcReport,
    DEFAULT_MAX_PARALLEL_PACK_DOWNLOADS,
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

    /// Removes only this restore's staged pack hardlinks. Cached pack objects are left intact
    /// until the caller explicitly prunes them.
    pub fn release_staged_pack_links(&self) -> Result<usize> {
        let packs = self.staging.join("packs");
        let metadata = match fs::symlink_metadata(&packs) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StateError::UnsafePath(format!(
                "restore pack staging is not a directory: {}",
                packs.display()
            )));
        }

        let mut released = 0;
        for entry in fs::read_dir(&packs)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                return Err(StateError::UnsafePath(format!(
                    "unexpected directory in restore pack staging: {}",
                    path.display()
                )));
            }
            fs::remove_file(path)?;
            released += 1;
        }
        fs::remove_dir(packs)?;
        Ok(released)
    }

    /// Removes every unlinked verified pack from the shared restore cache.
    pub fn prune_restore_pack_cache(&self) -> Result<PackCacheGcReport> {
        let report = prune_local_pack_cache(&self.pack_cache, &PackCacheGcOptions::new(0, 0))?;
        if report.packs_after != 0 {
            return Err(StateError::Invalid(format!(
                "{} restore packs remain linked or protected",
                report.packs_after
            )));
        }
        Ok(report)
    }
}

struct DirectoryCleanupGuard {
    path: PathBuf,
    armed: bool,
}

impl DirectoryCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DirectoryCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
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
    let mut staging_cleanup = DirectoryCleanupGuard::new(staging.clone());
    for child in [
        "manifest",
        "packs",
        "materialized/.chunks",
        "pre_restore",
        "logs",
    ] {
        fs::create_dir_all(staging.join(child))?;
    }
    let manifest = read_committed_manifest(provider, master, app_id, bundle_id).await?;
    fs::write(
        staging.join("manifest/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    staging_cleanup.disarm();
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

#[derive(Debug, Default, Clone)]
pub struct RestoreReport {
    pub destinations: Vec<PathBuf>,
    pub published_entries: usize,
    pub reused_entries: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    Active,
    Committed,
    Aborted,
}

#[derive(Debug)]
struct PlannedDestination {
    manifest_index: usize,
    root: PathBuf,
    destination: PathBuf,
    identical: bool,
    existed: bool,
}

#[derive(Debug)]
struct PlannedTombstone {
    root: PathBuf,
    destination: PathBuf,
}

#[derive(Debug, Clone)]
struct SymlinkRollback {
    destination: PathBuf,
    target: PathBuf,
}

const TRANSACTION_JOURNAL_FILE: &str = "restore-transaction.json";
const TRANSACTION_JOURNAL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Active,
    Committed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JournalPathMapping {
    source: PathBuf,
    staged: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JournalSymlinkRollback {
    destination: PathBuf,
    target: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RestoreTransactionJournal {
    version: u32,
    restore_id: Uuid,
    state: JournalState,
    allowed_roots: Vec<PathBuf>,
    rollback_mappings: Vec<JournalPathMapping>,
    prior_symlinks: Vec<JournalSymlinkRollback>,
    created_paths: Vec<PathBuf>,
    created_directories: Vec<PathBuf>,
    partial_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct RestoreRecoveryReport {
    pub workspaces_scanned: usize,
    pub workspaces_rolled_back: usize,
    pub committed_workspaces_cleaned: usize,
    pub unjournaled_workspaces_cleaned: usize,
    pub failures: Vec<RestoreRecoveryFailure>,
}

#[derive(Debug, Clone)]
pub struct RestoreRecoveryFailure {
    pub workspace: PathBuf,
    pub error: String,
}

/// Owns all filesystem mutations for one restore across every milestone.
///
/// Dropping an active transaction rolls back published paths and removes restore staging. This is
/// required because operation cancellation drops the restore future at an async cancellation point.
pub struct RestoreTransaction<'a> {
    plan: &'a RestorePlan,
    roots: &'a LogicalRootMap,
    db: Option<&'a StateDb>,
    rollback_views: Vec<SnapshotView>,
    symlink_rollbacks: Vec<SymlinkRollback>,
    created_paths: BTreeSet<PathBuf>,
    created_directories: BTreeSet<PathBuf>,
    active_partials: BTreeSet<PathBuf>,
    published_entries: BTreeSet<usize>,
    pending_associations: BTreeSet<usize>,
    allowed_roots: BTreeSet<PathBuf>,
    remaining_chunk_refs: BTreeMap<String, usize>,
    chunk_eviction_enabled: bool,
    tombstones_applied: bool,
    state: TransactionState,
}

impl<'a> RestoreTransaction<'a> {
    pub fn new(plan: &'a RestorePlan, roots: &'a LogicalRootMap, db: Option<&'a StateDb>) -> Self {
        let mut remaining_chunk_refs = BTreeMap::new();
        for entry in plan.priority_plan().entries {
            let unique_chunks = plan.manifest.files[entry.manifest_index]
                .chunks
                .iter()
                .map(|chunk| chunk.hash.clone())
                .collect::<BTreeSet<_>>();
            for hash in unique_chunks {
                *remaining_chunk_refs.entry(hash).or_insert(0) += 1;
            }
        }
        Self {
            plan,
            roots,
            db,
            rollback_views: Vec::new(),
            symlink_rollbacks: Vec::new(),
            created_paths: BTreeSet::new(),
            created_directories: BTreeSet::new(),
            active_partials: BTreeSet::new(),
            published_entries: BTreeSet::new(),
            pending_associations: BTreeSet::new(),
            allowed_roots: BTreeSet::new(),
            remaining_chunk_refs,
            chunk_eviction_enabled: false,
            tombstones_applied: false,
            state: TransactionState::Active,
        }
    }

    /// Reconstructs and atomically publishes entries through `target` directly at their final
    /// destinations. The transaction retains rollback state until [`Self::commit`].
    pub fn publish_to(&mut self, target: RestoreTarget) -> Result<RestoreReport> {
        if self.state != TransactionState::Active {
            return Err(StateError::Invalid(
                "restore transaction is not active".into(),
            ));
        }

        let (planned, tombstones) = self.preflight(target)?;
        self.prepare_rollback(&planned, &tombstones)?;

        let mut report = RestoreReport::default();
        for planned in planned {
            report.destinations.push(planned.destination.clone());
            if planned.identical {
                self.release_entry_chunks(planned.manifest_index)?;
                self.published_entries.insert(planned.manifest_index);
                // A retry after a crash may find the destination already published while the
                // prior process died before committing its database association.
                self.pending_associations.insert(planned.manifest_index);
                report.reused_entries += 1;
                continue;
            }

            let file = &self.plan.manifest.files[planned.manifest_index];
            match file.file_type.as_str() {
                "directory" => {
                    self.create_directory_chain_journaled(&planned.root, &planned.destination)?;
                }
                "symlink" => self.publish_symlink(file, &planned)?,
                "file" => self.publish_file(file, &planned)?,
                other => {
                    return Err(StateError::UnsafePath(format!(
                        "unsupported file type '{other}'"
                    )));
                }
            }
            self.release_entry_chunks(planned.manifest_index)?;
            self.published_entries.insert(planned.manifest_index);
            self.pending_associations.insert(planned.manifest_index);
            report.published_entries += 1;
        }

        if target == RestoreTarget::Complete && !self.tombstones_applied {
            for tombstone in tombstones {
                if remove_path_if_present(&tombstone.destination)? {
                    sync_parent_directory(&tombstone.destination)?;
                }
            }
            self.tombstones_applied = true;
        }

        Ok(report)
    }

    /// Enables chunk eviction after the complete download has populated every chunk needed by the
    /// selected restore mode. Chunks whose only files were already published are removed now;
    /// remaining chunks are removed as their final referencing file is published.
    pub fn enable_chunk_eviction(&mut self) -> Result<usize> {
        if self.state != TransactionState::Active {
            return Err(StateError::Invalid(
                "restore transaction is not active".into(),
            ));
        }
        self.chunk_eviction_enabled = true;
        let zero_reference = self
            .remaining_chunk_refs
            .iter()
            .filter(|(_, references)| **references == 0)
            .map(|(hash, _)| hash.clone())
            .collect::<Vec<_>>();
        let mut evicted = 0;
        for hash in zero_reference {
            if self.evict_chunk(&hash)? {
                evicted += 1;
            }
        }
        Ok(evicted)
    }

    pub fn remaining_chunk_references(&self, hash: &str) -> usize {
        self.remaining_chunk_refs.get(hash).copied().unwrap_or(0)
    }

    pub fn release_staged_pack_links(&self) -> Result<usize> {
        self.plan.release_staged_pack_links()
    }

    pub fn prune_restore_pack_cache(&self) -> Result<PackCacheGcReport> {
        self.plan.prune_restore_pack_cache()
    }

    /// Persists deferred associations and removes all restore-local staging and rollback data.
    pub fn commit(mut self) -> Result<()> {
        if self.state != TransactionState::Active {
            return Err(StateError::Invalid(
                "restore transaction is not active".into(),
            ));
        }

        if self
            .remaining_chunk_refs
            .values()
            .any(|references| *references != 0)
        {
            return Err(StateError::Invalid(
                "cannot commit restore before every selected file is published".into(),
            ));
        }
        let associations = if self.db.is_some() {
            self.pending_associations
                .iter()
                .map(|manifest_index| {
                    let file = &self.plan.manifest.files[*manifest_index];
                    let (_, destination) = resolve_destination(self.roots, file)?;
                    Ok(RestoredPathAssociationInput {
                        canonical_path: destination.to_string_lossy().into_owned(),
                        persistence_class: file.persistence_class,
                        semantic_role: file.semantic_role,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        self.release_staged_pack_links()?;
        self.prune_restore_pack_cache()?;

        // Persist the filesystem commit decision before the SQLite commit. A crash after SQLite
        // commits can never leave an active journal that startup recovery would roll back.
        self.persist_journal(JournalState::Committed)?;
        self.state = TransactionState::Committed;

        if let Some(db) = self.db {
            if let Err(database_error) =
                db.upsert_restored_path_associations(&self.plan.manifest.app.app_id, &associations)
            {
                // A returned database error is rollback-safe only after the durable journal has
                // been re-armed. If that write fails, retain the committed decision and destinations.
                if let Err(journal_error) = self.persist_journal(JournalState::Active) {
                    return Err(StateError::Invalid(format!(
                        "database publication failed: {database_error}; restore remains committed because the rollback journal could not be re-armed: {journal_error}"
                    )));
                }
                self.state = TransactionState::Active;
                return Err(database_error);
            }
        }

        if self.plan.staging.exists() {
            remove_directory_tree_durable(&self.plan.staging)?;
        }
        Ok(())
    }

    /// Reverts every path changed by this restore and removes staging after rollback succeeds.
    pub fn abort(&mut self) -> Result<()> {
        if self.state != TransactionState::Active {
            return Ok(());
        }

        let journal = self.journal_snapshot(JournalState::Active);
        let mut first_error = None;
        record_cleanup_error(self.persist_journal(JournalState::Active), &mut first_error);
        record_cleanup_error(
            rollback_from_journal(&self.plan.staging, &journal),
            &mut first_error,
        );
        record_cleanup_error(
            self.release_staged_pack_links().map(|_| ()),
            &mut first_error,
        );
        record_cleanup_error(
            self.prune_restore_pack_cache().map(|_| ()),
            &mut first_error,
        );

        if let Some(error) = first_error {
            return Err(error);
        }
        self.state = TransactionState::Aborted;
        if self.plan.staging.exists() {
            remove_directory_tree_durable(&self.plan.staging)?;
        }
        Ok(())
    }

    fn preflight(
        &self,
        target: RestoreTarget,
    ) -> Result<(Vec<PlannedDestination>, Vec<PlannedTombstone>)> {
        let priority_plan = self.plan.priority_plan();
        let mut destinations = BTreeSet::new();
        let mut planned = Vec::new();
        for entry in priority_plan.entries_for(target) {
            if self.published_entries.contains(&entry.manifest_index) {
                continue;
            }
            let file = &self.plan.manifest.files[entry.manifest_index];
            let (root, destination) = resolve_destination(self.roots, file)?;
            if !destinations.insert(destination.clone()) {
                return Err(StateError::Invalid(format!(
                    "multiple manifest entries resolve to {}",
                    destination.display()
                )));
            }
            validate_parent_chain(&root, &destination)?;
            let metadata = symlink_metadata_if_present(&destination)?;
            let identical = match file.file_type.as_str() {
                "file" => target_file_is_identical(&destination, file),
                "directory" => metadata.as_ref().is_some_and(|value| value.is_dir()),
                "symlink" => {
                    let target = file
                        .symlink_target
                        .as_deref()
                        .ok_or_else(|| StateError::Invalid("symlink missing target".into()))?;
                    let logical = file
                        .logical_root_parsed()
                        .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
                    validate_symlink_target(target, self.roots, &logical)?;
                    fs::read_link(&destination)
                        .map(|existing| existing == Path::new(target))
                        .unwrap_or(false)
                }
                other => {
                    return Err(StateError::UnsafePath(format!(
                        "unsupported file type '{other}'"
                    )));
                }
            };
            if let Some(metadata) = &metadata {
                let incompatible = match file.file_type.as_str() {
                    "directory" => !metadata.is_dir(),
                    "file" | "symlink" => metadata.is_dir(),
                    _ => false,
                };
                if incompatible {
                    return Err(StateError::Invalid(format!(
                        "cannot atomically replace incompatible destination {}",
                        destination.display()
                    )));
                }
            }
            planned.push(PlannedDestination {
                manifest_index: entry.manifest_index,
                root,
                destination,
                identical,
                existed: metadata.is_some(),
            });
        }

        let mut tombstones = Vec::new();
        if target == RestoreTarget::Complete && !self.tombstones_applied {
            for tombstone in &self.plan.manifest.tombstones {
                validate_relative_path(&tombstone.relative_path)?;
                let Some(logical) = LogicalRoot::parse(&tombstone.logical_root) else {
                    return Err(StateError::UnsafePath(tombstone.logical_root.clone()));
                };
                let Some(root) = self.roots.resolve(&logical) else {
                    continue;
                };
                let root = canonicalize_root_if_present(root)?;
                let destination = join_validated(&root, &tombstone.relative_path)?;
                validate_parent_chain(&root, &destination)?;
                if !destinations.insert(destination.clone()) {
                    return Err(StateError::Invalid(format!(
                        "manifest entry and tombstone both resolve to {}",
                        destination.display()
                    )));
                }
                if symlink_metadata_if_present(&destination)?
                    .as_ref()
                    .is_some_and(|metadata| metadata.is_dir())
                {
                    continue;
                }
                tombstones.push(PlannedTombstone { root, destination });
            }
        }
        Ok((planned, tombstones))
    }

    fn prepare_rollback(
        &mut self,
        planned: &[PlannedDestination],
        tombstones: &[PlannedTombstone],
    ) -> Result<()> {
        self.allowed_roots
            .extend(planned.iter().map(|planned| planned.root.clone()));
        self.allowed_roots
            .extend(tombstones.iter().map(|tombstone| tombstone.root.clone()));
        let mut regular_files = Vec::new();
        let mut symlinks = Vec::new();
        for destination in planned
            .iter()
            .filter(|planned| !planned.identical)
            .map(|planned| &planned.destination)
            .chain(tombstones.iter().map(|tombstone| &tombstone.destination))
        {
            let Some(metadata) = symlink_metadata_if_present(destination)? else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                symlinks.push(SymlinkRollback {
                    destination: destination.clone(),
                    target: fs::read_link(destination)?,
                });
            } else if metadata.is_file() {
                regular_files.push(destination.clone());
            }
        }
        if !regular_files.is_empty() {
            let rollback_root = self.plan.staging.join("pre_restore");
            let view = create_view(&rollback_root, &regular_files, false)?;
            if view.mappings.len() != regular_files.len() {
                return Err(StateError::Invalid(
                    "failed to capture every regular restore rollback path".into(),
                ));
            }
            sync_snapshot_view(&rollback_root, &view)?;
            self.rollback_views.push(view);
        }
        self.symlink_rollbacks.extend(symlinks);
        self.persist_journal(JournalState::Active)
    }

    fn journal_snapshot(&self, state: JournalState) -> RestoreTransactionJournal {
        RestoreTransactionJournal {
            version: TRANSACTION_JOURNAL_VERSION,
            restore_id: self.plan.restore_id,
            state,
            allowed_roots: self.allowed_roots.iter().cloned().collect(),
            rollback_mappings: self
                .rollback_views
                .iter()
                .flat_map(|view| &view.mappings)
                .map(|mapping| JournalPathMapping {
                    source: mapping.source.clone(),
                    staged: mapping.staged.clone(),
                })
                .collect(),
            prior_symlinks: self
                .symlink_rollbacks
                .iter()
                .map(|rollback| JournalSymlinkRollback {
                    destination: rollback.destination.clone(),
                    target: rollback.target.clone(),
                })
                .collect(),
            created_paths: self.created_paths.iter().cloned().collect(),
            created_directories: self.created_directories.iter().cloned().collect(),
            partial_paths: self.active_partials.iter().cloned().collect(),
        }
    }

    fn persist_journal(&self, state: JournalState) -> Result<()> {
        persist_transaction_journal(&self.plan.staging, &self.journal_snapshot(state))
    }

    fn create_directory_chain_journaled(&mut self, root: &Path, destination: &Path) -> Result<()> {
        for directory in missing_directory_chain(root, destination)? {
            // A logical root may not exist yet, so creating it can also require ancestors outside
            // the root itself. Journal each such directory as an allowed rollback boundary before
            // creating it.
            self.allowed_roots.insert(directory.clone());
            self.created_directories.insert(directory.clone());
            self.persist_journal(JournalState::Active)?;
            fs::create_dir(&directory)?;
            sync_parent_directory(&directory)?;
        }
        Ok(())
    }

    fn track_partial(&mut self, partial: &Path) -> Result<()> {
        self.active_partials.insert(partial.to_path_buf());
        self.persist_journal(JournalState::Active)
    }

    fn clear_partial(&mut self, partial: &Path) -> Result<()> {
        self.active_partials.remove(partial);
        self.persist_journal(JournalState::Active)
    }

    fn track_created_path(&mut self, path: &Path) -> Result<()> {
        self.created_paths.insert(path.to_path_buf());
        self.persist_journal(JournalState::Active)
    }

    fn release_entry_chunks(&mut self, manifest_index: usize) -> Result<()> {
        let unique_chunks = self.plan.manifest.files[manifest_index]
            .chunks
            .iter()
            .map(|chunk| chunk.hash.clone())
            .collect::<BTreeSet<_>>();
        for hash in unique_chunks {
            let references = self.remaining_chunk_refs.get_mut(&hash).ok_or_else(|| {
                StateError::Invalid(format!("missing restore chunk reference count for {hash}"))
            })?;
            if *references == 0 {
                return Err(StateError::Invalid(format!(
                    "restore chunk reference count underflow for {hash}"
                )));
            }
            *references -= 1;
            if self.chunk_eviction_enabled && *references == 0 {
                self.evict_chunk(&hash)?;
            }
        }
        Ok(())
    }

    fn evict_chunk(&self, hash: &str) -> Result<bool> {
        let path = restore_chunk_path(self.plan, hash)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn publish_file(&mut self, file: &ManifestFile, planned: &PlannedDestination) -> Result<()> {
        let parent = planned.destination.parent().ok_or_else(|| {
            StateError::UnsafePath(format!(
                "destination has no parent: {}",
                planned.destination.display()
            ))
        })?;
        self.create_directory_chain_journaled(&planned.root, parent)?;
        validate_parent_chain(&planned.root, &planned.destination)?;

        let partial = destination_partial_path(&planned.destination, self.plan.restore_id)?;
        self.track_partial(&partial)?;
        remove_recovery_leaf(&partial)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        for chunk in &file.chunks {
            let chunk_path = restore_chunk_path(self.plan, &chunk.hash)?;
            let mut input = fs::File::open(chunk_path)?;
            std::io::copy(&mut input, &mut output)?;
        }
        output.flush()?;
        output.sync_all()?;
        drop(output);

        let size = fs::metadata(&partial)?.len();
        if size != file.size {
            return Err(StateError::Integrity(format!(
                "file size mismatch for {}: expected {}, got {size}",
                file.relative_path, file.size
            )));
        }
        if blake3_file(&partial)? != file.file_hash {
            return Err(StateError::Integrity(format!(
                "file hash mismatch for {}",
                file.relative_path
            )));
        }
        if let Some(mode) = file.mode {
            fs::set_permissions(&partial, fs::Permissions::from_mode(mode))?;
        }
        validate_parent_chain(&planned.root, &planned.destination)?;
        if !planned.existed {
            self.track_created_path(&planned.destination)?;
        }
        fs::rename(&partial, &planned.destination)?;
        sync_parent_directory(&planned.destination)?;
        self.clear_partial(&partial)?;
        Ok(())
    }

    fn publish_symlink(&mut self, file: &ManifestFile, planned: &PlannedDestination) -> Result<()> {
        let target = file
            .symlink_target
            .as_deref()
            .ok_or_else(|| StateError::Invalid("symlink missing target".into()))?;
        let logical = file
            .logical_root_parsed()
            .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
        validate_symlink_target(target, self.roots, &logical)?;
        let parent = planned.destination.parent().ok_or_else(|| {
            StateError::UnsafePath(format!(
                "destination has no parent: {}",
                planned.destination.display()
            ))
        })?;
        self.create_directory_chain_journaled(&planned.root, parent)?;
        validate_parent_chain(&planned.root, &planned.destination)?;
        let partial = destination_partial_path(&planned.destination, self.plan.restore_id)?;
        self.track_partial(&partial)?;
        remove_recovery_leaf(&partial)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &partial)?;
        if !planned.existed {
            self.track_created_path(&planned.destination)?;
        }
        fs::rename(&partial, &planned.destination)?;
        sync_parent_directory(&planned.destination)?;
        self.clear_partial(&partial)?;
        Ok(())
    }
}

impl Drop for RestoreTransaction<'_> {
    fn drop(&mut self) {
        if self.state == TransactionState::Active {
            let _ = self.abort();
        }
    }
}

/// Recovers restore workspaces left by a process crash. Active journals are rolled back;
/// committed journals and workspaces created before the first destination mutation are cleaned.
/// A workspace is retained when its journal is invalid or any rollback step fails.
pub fn recover_interrupted_restores(paths: &AgentPaths) -> Result<RestoreRecoveryReport> {
    let mut report = RestoreRecoveryReport::default();
    let metadata = match fs::symlink_metadata(&paths.restore) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StateError::UnsafePath(format!(
            "restore workspace root is not a directory: {}",
            paths.restore.display()
        )));
    }
    let entries = fs::read_dir(&paths.restore)?;
    let mut workspaces = entries
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    workspaces.sort();

    for workspace in workspaces {
        report.workspaces_scanned += 1;
        let result = recover_restore_workspace(&workspace);
        match result {
            Ok(WorkspaceRecovery::RolledBack) => report.workspaces_rolled_back += 1,
            Ok(WorkspaceRecovery::CommittedCleaned) => report.committed_workspaces_cleaned += 1,
            Ok(WorkspaceRecovery::UnjournaledCleaned) => report.unjournaled_workspaces_cleaned += 1,
            Err(error) => report.failures.push(RestoreRecoveryFailure {
                workspace,
                error: error.to_string(),
            }),
        }
    }

    // Removing successful workspaces releases their staged hardlinks. Zero-budget pruning then
    // removes all packs not still protected by a failed recovery workspace.
    let _ = prune_local_pack_cache(
        &paths.cache.join("restore-packs"),
        &PackCacheGcOptions::new(0, 0),
    );
    Ok(report)
}

#[derive(Debug, Clone, Copy)]
enum WorkspaceRecovery {
    RolledBack,
    CommittedCleaned,
    UnjournaledCleaned,
}

fn recover_restore_workspace(workspace: &Path) -> Result<WorkspaceRecovery> {
    let metadata = fs::symlink_metadata(workspace)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StateError::UnsafePath(format!(
            "restore workspace is not a directory: {}",
            workspace.display()
        )));
    }
    let journal_path = workspace.join(TRANSACTION_JOURNAL_FILE);
    let journal_metadata = match fs::symlink_metadata(&journal_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_directory_tree_durable(workspace)?;
            return Ok(WorkspaceRecovery::UnjournaledCleaned);
        }
        Err(error) => return Err(error.into()),
    };
    if !journal_metadata.is_file() || journal_metadata.file_type().is_symlink() {
        return Err(StateError::UnsafePath(format!(
            "restore transaction journal is not a regular file: {}",
            journal_path.display()
        )));
    }
    let bytes = fs::read(&journal_path)?;
    let journal: RestoreTransactionJournal = serde_json::from_slice(&bytes)?;
    if workspace
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| Uuid::parse_str(name).ok())
        != Some(journal.restore_id)
    {
        return Err(StateError::Invalid(format!(
            "restore transaction journal ID does not match workspace {}",
            workspace.display()
        )));
    }
    validate_recovery_journal(workspace, &journal)?;
    match journal.state {
        JournalState::Committed => {
            remove_directory_tree_durable(workspace)?;
            Ok(WorkspaceRecovery::CommittedCleaned)
        }
        JournalState::Active => {
            rollback_from_journal(workspace, &journal)?;
            remove_directory_tree_durable(workspace)?;
            Ok(WorkspaceRecovery::RolledBack)
        }
    }
}

fn persist_transaction_journal(
    workspace: &Path,
    journal: &RestoreTransactionJournal,
) -> Result<()> {
    fs::create_dir_all(workspace)?;
    let workspace_metadata = fs::symlink_metadata(workspace)?;
    if workspace_metadata.file_type().is_symlink() || !workspace_metadata.is_dir() {
        return Err(StateError::UnsafePath(format!(
            "restore workspace is not a directory: {}",
            workspace.display()
        )));
    }
    let path = workspace.join(TRANSACTION_JOURNAL_FILE);
    let temporary = workspace.join(format!("{TRANSACTION_JOURNAL_FILE}.partial"));
    if let Some(metadata) = symlink_metadata_if_present(&temporary)? {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            return Err(StateError::UnsafePath(format!(
                "restore transaction journal temporary path is a directory: {}",
                temporary.display()
            )));
        }
        fs::remove_file(&temporary)?;
    }
    let bytes = serde_json::to_vec_pretty(journal)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &path)?;
    fs::File::open(workspace)?.sync_all()?;
    Ok(())
}

fn sync_snapshot_view(rollback_root: &Path, view: &SnapshotView) -> Result<()> {
    let mut directories = BTreeSet::new();
    directories.insert(rollback_root.to_path_buf());
    for mapping in &view.mappings {
        let relative = mapping.staged.strip_prefix(rollback_root).map_err(|_| {
            StateError::UnsafePath(format!(
                "rollback artifact escapes restore staging: {}",
                mapping.staged.display()
            ))
        })?;
        if relative.as_os_str().is_empty() {
            return Err(StateError::UnsafePath(format!(
                "invalid rollback artifact path: {}",
                mapping.staged.display()
            )));
        }
        let metadata = fs::symlink_metadata(&mapping.staged)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(StateError::UnsafePath(format!(
                "rollback artifact is not a regular file: {}",
                mapping.staged.display()
            )));
        }
        fs::File::open(&mapping.staged)?.sync_all()?;

        let mut directory = mapping.staged.parent();
        while let Some(path) = directory {
            directories.insert(path.to_path_buf());
            if path == rollback_root {
                break;
            }
            directory = path.parent();
        }
    }
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

fn validate_recovery_journal(workspace: &Path, journal: &RestoreTransactionJournal) -> Result<()> {
    if journal.version != TRANSACTION_JOURNAL_VERSION {
        return Err(StateError::Invalid(format!(
            "unsupported restore transaction journal version {}",
            journal.version
        )));
    }
    validate_absolute_journal_path(workspace, "restore workspace")?;
    for root in &journal.allowed_roots {
        validate_absolute_journal_path(root, "journaled logical root")?;
    }
    let rollback_root = workspace.join("pre_restore");
    for mapping in &journal.rollback_mappings {
        validate_recovery_destination(journal, &mapping.source)?;
        validate_absolute_journal_path(&mapping.staged, "rollback artifact")?;
        let relative = mapping.staged.strip_prefix(&rollback_root).map_err(|_| {
            StateError::UnsafePath(format!(
                "rollback artifact escapes restore workspace: {}",
                mapping.staged.display()
            ))
        })?;
        if relative.as_os_str().is_empty()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(StateError::UnsafePath(format!(
                "invalid rollback artifact path: {}",
                mapping.staged.display()
            )));
        }
        validate_parent_chain(workspace, &mapping.staged)?;
    }
    for rollback in &journal.prior_symlinks {
        validate_recovery_destination(journal, &rollback.destination)?;
    }
    for path in journal
        .created_paths
        .iter()
        .chain(&journal.created_directories)
        .chain(&journal.partial_paths)
    {
        validate_recovery_destination(journal, path)?;
    }
    Ok(())
}

fn validate_absolute_journal_path(path: &Path, description: &str) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(StateError::UnsafePath(format!(
            "{description} is not a normalized absolute path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_recovery_destination(
    journal: &RestoreTransactionJournal,
    destination: &Path,
) -> Result<()> {
    validate_absolute_journal_path(destination, "recovery destination")?;
    let root = journal
        .allowed_roots
        .iter()
        .filter(|root| destination.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| {
            StateError::UnsafePath(format!(
                "recovery destination is outside journaled logical roots: {}",
                destination.display()
            ))
        })?;
    validate_parent_chain(root, destination)
}

fn rollback_from_journal(workspace: &Path, journal: &RestoreTransactionJournal) -> Result<()> {
    validate_recovery_journal(workspace, journal)?;
    let mut first_error = None;
    for partial in &journal.partial_paths {
        record_cleanup_error(remove_recovery_leaf_durable(partial), &mut first_error);
    }
    for path in journal.created_paths.iter().rev() {
        record_cleanup_error(remove_recovery_leaf_durable(path), &mut first_error);
    }
    for mapping in journal.rollback_mappings.iter().rev() {
        record_cleanup_error(restore_regular_mapping(journal, mapping), &mut first_error);
    }
    for rollback in journal.prior_symlinks.iter().rev() {
        record_cleanup_error(restore_prior_symlink(journal, rollback), &mut first_error);
    }
    let mut directories = journal.created_directories.clone();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        match fs::remove_dir(&directory) {
            Ok(()) => record_cleanup_error(sync_parent_directory(&directory), &mut first_error),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                record_cleanup_error(sync_parent_directory(&directory), &mut first_error);
            }
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(error) => record_cleanup_error(Err(error.into()), &mut first_error),
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn restore_regular_mapping(
    journal: &RestoreTransactionJournal,
    mapping: &JournalPathMapping,
) -> Result<()> {
    let metadata = fs::symlink_metadata(&mapping.staged).map_err(|error| {
        StateError::Invalid(format!(
            "rollback artifact {} is unavailable: {error}",
            mapping.staged.display()
        ))
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StateError::UnsafePath(format!(
            "unsupported rollback artifact type: {}",
            mapping.staged.display()
        )));
    }
    validate_recovery_destination(journal, &mapping.source)?;
    require_destination_parent(&mapping.source)?;
    ensure_recovery_leaf_replaceable(&mapping.source)?;

    let partial = rollback_partial_path(&mapping.source, journal.restore_id)?;
    validate_recovery_destination(journal, &partial)?;
    remove_recovery_leaf(&partial)?;
    fs::copy(&mapping.staged, &partial)?;
    fs::File::open(&partial)?.sync_all()?;
    fs::rename(&partial, &mapping.source)?;
    sync_parent_directory(&mapping.source)
}

fn restore_prior_symlink(
    journal: &RestoreTransactionJournal,
    rollback: &JournalSymlinkRollback,
) -> Result<()> {
    validate_recovery_destination(journal, &rollback.destination)?;
    require_destination_parent(&rollback.destination)?;
    ensure_recovery_leaf_replaceable(&rollback.destination)?;

    let partial = rollback_partial_path(&rollback.destination, journal.restore_id)?;
    validate_recovery_destination(journal, &partial)?;
    remove_recovery_leaf(&partial)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&rollback.target, &partial)?;
    fs::rename(&partial, &rollback.destination)?;
    sync_parent_directory(&rollback.destination)
}

fn resolve_destination(roots: &LogicalRootMap, file: &ManifestFile) -> Result<(PathBuf, PathBuf)> {
    validate_relative_path(&file.relative_path)?;
    let logical = file
        .logical_root_parsed()
        .ok_or_else(|| StateError::UnsafePath(file.logical_root.clone()))?;
    let root = roots
        .resolve(&logical)
        .ok_or_else(|| StateError::NotFound(format!("logical root {}", file.logical_root)))?;
    let root = canonicalize_root_if_present(root)?;
    let destination = join_validated(&root, &file.relative_path)?;
    Ok((root, destination))
}

fn canonicalize_root_if_present(root: PathBuf) -> Result<PathBuf> {
    let mut ancestor = root.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(ancestor)?;
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    StateError::UnsafePath(format!(
                        "logical root has no existing ancestor: {}",
                        root.display()
                    ))
                })?;
                missing.push(name.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    StateError::UnsafePath(format!(
                        "logical root has no existing ancestor: {}",
                        root.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn validate_parent_chain(root: &Path, destination: &Path) -> Result<()> {
    validate_directory_component(root)?;
    if destination == root {
        return Ok(());
    }
    let parent = destination.parent().ok_or_else(|| {
        StateError::UnsafePath(format!(
            "destination has no parent: {}",
            destination.display()
        ))
    })?;
    let relative = parent.strip_prefix(root).map_err(|_| {
        StateError::UnsafePath(format!(
            "destination {} escapes {}",
            destination.display(),
            root.display()
        ))
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        validate_directory_component(&current)?;
        if !current.exists() {
            break;
        }
    }
    Ok(())
}

fn validate_directory_component(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StateError::UnsafePath(format!(
            "destination parent is a symlink: {}",
            path.display()
        ))),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(StateError::Invalid(format!(
            "destination parent is not a directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn missing_directory_chain(root: &Path, destination: &Path) -> Result<Vec<PathBuf>> {
    destination.strip_prefix(root).map_err(|_| {
        StateError::UnsafePath(format!(
            "destination {} escapes {}",
            destination.display(),
            root.display()
        ))
    })?;

    let mut missing = Vec::new();
    let mut current = destination.to_path_buf();
    loop {
        match symlink_metadata_if_present(&current)? {
            Some(metadata) if metadata.file_type().is_symlink() => {
                return Err(StateError::UnsafePath(format!(
                    "destination directory is a symlink: {}",
                    current.display()
                )));
            }
            Some(metadata) if metadata.is_dir() => break,
            Some(_) => {
                return Err(StateError::Invalid(format!(
                    "destination parent is not a directory: {}",
                    current.display()
                )));
            }
            None => missing.push(current.clone()),
        }
        current = current
            .parent()
            .ok_or_else(|| {
                StateError::UnsafePath(format!(
                    "destination has no existing ancestor: {}",
                    destination.display()
                ))
            })?
            .to_path_buf();
    }

    missing.reverse();
    Ok(missing)
}

fn restore_chunk_path(plan: &RestorePlan, hash: &str) -> Result<PathBuf> {
    let token = hash.trim_start_matches("blake3:");
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StateError::Integrity(format!(
            "invalid restore chunk hash {hash}"
        )));
    }
    Ok(plan.staging.join("materialized/.chunks").join(token))
}

fn destination_partial_path(destination: &Path, restore_id: Uuid) -> Result<PathBuf> {
    destination_auxiliary_path(destination, format!(".noland-{restore_id}.partial"))
}

fn rollback_partial_path(destination: &Path, restore_id: Uuid) -> Result<PathBuf> {
    destination_auxiliary_path(
        destination,
        format!(".noland-{restore_id}.rollback.partial"),
    )
}

fn destination_auxiliary_path(destination: &Path, suffix: String) -> Result<PathBuf> {
    let file_name = destination.file_name().ok_or_else(|| {
        StateError::UnsafePath(format!(
            "destination has no filename: {}",
            destination.display()
        ))
    })?;
    let mut auxiliary_name = OsString::from(file_name);
    auxiliary_name.push(suffix);
    Ok(destination.with_file_name(auxiliary_name))
}

fn symlink_metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn require_destination_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        StateError::UnsafePath(format!("destination has no parent: {}", path.display()))
    })?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StateError::UnsafePath(format!(
            "destination parent is a symlink: {}",
            parent.display()
        ))),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(StateError::Invalid(format!(
            "destination parent is not a directory: {}",
            parent.display()
        ))),
        Err(error) => Err(error.into()),
    }
}

fn ensure_recovery_leaf_replaceable(path: &Path) -> Result<()> {
    if symlink_metadata_if_present(path)?
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        return Err(StateError::UnsafePath(format!(
            "recovery expected a file or symlink but found a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn remove_recovery_leaf(path: &Path) -> Result<()> {
    let Some(metadata) = symlink_metadata_if_present(path)? else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Err(StateError::UnsafePath(format!(
            "recovery expected a file or symlink but found a directory: {}",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    Ok(())
}

fn remove_recovery_leaf_durable(path: &Path) -> Result<()> {
    remove_recovery_leaf(path)?;
    sync_parent_directory(path)
}

fn remove_path_if_present(path: &Path) -> Result<bool> {
    let Some(metadata) = symlink_metadata_if_present(path)? else {
        return Ok(false);
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| StateError::UnsafePath(format!("path has no parent: {}", path.display())))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn remove_directory_tree_durable(path: &Path) -> Result<()> {
    fs::remove_dir_all(path)?;
    sync_parent_directory(path)
}

fn record_cleanup_error(result: Result<()>, first_error: &mut Option<StateError>) {
    if let Err(error) = result {
        if first_error.is_none() {
            *first_error = Some(error);
        }
    }
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    use chrono::Utc;
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
    fn ready_to_launch_publishes_only_critical_tiers() {
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
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);

        let ready = transaction.publish_to(READY_TO_LAUNCH).unwrap();
        assert_eq!(ready.published_entries, 1);
        assert_eq!(
            fs::read(home.join(".local/share/game/save.dat")).unwrap(),
            b"save"
        );
        assert!(!home.join(".local/share/game/cache.bin").exists());
        assert!(!plan.staging.join("materialized/tree").exists());

        let complete = transaction.publish_to(RestoreTarget::Complete).unwrap();
        assert_eq!(complete.published_entries, 1);
        assert_eq!(
            fs::read(home.join(".local/share/game/cache.bin")).unwrap(),
            b"cache"
        );
        transaction.commit().unwrap();
        assert!(!plan.staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn crash_recovery_uses_durable_journal_for_files_symlinks_directories_and_partials() {
        let root = test_dir("crash-recovery");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let replaced = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let created = test_file(
            "new/deep/cache.bin",
            PersistenceClass::Ephemeral,
            SemanticRole::Cache,
            b"cache",
        );
        let mut symlink = test_file(
            "game/current-link",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"",
        );
        symlink.file_type = "symlink".into();
        symlink.chunks.clear();
        symlink.symlink_target = Some("new-target".into());
        let plan = crash_test_plan(
            &paths,
            test_manifest(vec![replaced.clone(), created.clone(), symlink]),
        );
        for (file, bytes) in [
            (&replaced, b"save".as_slice()),
            (&created, b"cache".as_slice()),
        ] {
            fs::write(
                restore_chunk_path(&plan, &file.chunks[0].hash).unwrap(),
                bytes,
            )
            .unwrap();
        }
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let replaced_path = home.join(".local/share/game/save.dat");
        let created_path = home.join(".local/share/new/deep/cache.bin");
        let symlink_path = home.join(".local/share/game/current-link");
        fs::create_dir_all(replaced_path.parent().unwrap()).unwrap();
        fs::write(&replaced_path, b"old-save").unwrap();
        std::os::unix::fs::symlink("old-target", &symlink_path).unwrap();

        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        let partial = destination_partial_path(
            &home.join(".local/share/new/deep/interrupted.bin"),
            plan.restore_id,
        )
        .unwrap();
        transaction.track_partial(&partial).unwrap();
        fs::write(&partial, b"incomplete").unwrap();
        let journal: RestoreTransactionJournal =
            serde_json::from_slice(&fs::read(plan.staging.join(TRANSACTION_JOURNAL_FILE)).unwrap())
                .unwrap();
        assert!(!journal.rollback_mappings.is_empty());
        assert!(!journal.prior_symlinks.is_empty());
        assert!(journal.created_paths.contains(&created_path));
        assert!(journal
            .created_directories
            .iter()
            .any(|path| path.ends_with("new/deep")));
        assert!(journal.partial_paths.contains(&partial));
        assert!(!plan
            .staging
            .join(format!("{TRANSACTION_JOURNAL_FILE}.partial"))
            .exists());
        std::mem::forget(transaction);

        let report = recover_interrupted_restores(&paths).unwrap();

        assert_eq!(report.workspaces_rolled_back, 1);
        assert!(report.failures.is_empty());
        assert_eq!(fs::read(&replaced_path).unwrap(), b"old-save");
        assert!(!created_path.exists());
        assert_eq!(
            fs::read_link(&symlink_path).unwrap(),
            PathBuf::from("old-target")
        );
        assert!(!partial.exists());
        assert!(!plan.staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn crash_recovery_removes_new_logical_root_ancestor_directories() {
        let root = test_dir("crash-new-logical-root");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let file = test_file(
            "new/deep/cache.bin",
            PersistenceClass::Ephemeral,
            SemanticRole::Cache,
            b"cache",
        );
        let plan = crash_test_plan(&paths, test_manifest(vec![file.clone()]));
        fs::write(
            restore_chunk_path(&plan, &file.chunks[0].hash).unwrap(),
            b"cache",
        )
        .unwrap();
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/new/deep/cache.bin");

        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"cache");
        assert!(plan.staging.join(TRANSACTION_JOURNAL_FILE).exists());
        std::mem::forget(transaction);

        let report = recover_interrupted_restores(&paths).unwrap();

        assert_eq!(report.workspaces_rolled_back, 1);
        assert!(report.failures.is_empty());
        assert!(!destination.exists());
        assert!(!home.exists());
        assert!(!plan.staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn crash_recovery_cleans_committed_workspace_without_reverting_destination() {
        let root = test_dir("crash-after-commit-marker");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = crash_test_plan(&paths, test_manifest(vec![file.clone()]));
        fs::write(
            restore_chunk_path(&plan, &file.chunks[0].hash).unwrap(),
            b"save",
        )
        .unwrap();
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"old-save").unwrap();

        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        transaction
            .persist_journal(JournalState::Committed)
            .unwrap();
        transaction.state = TransactionState::Committed;
        std::mem::forget(transaction);

        let report = recover_interrupted_restores(&paths).unwrap();

        assert_eq!(report.committed_workspaces_cleaned, 1);
        assert!(report.failures.is_empty());
        assert_eq!(fs::read(destination).unwrap(), b"save");
        assert!(!plan.staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn database_failure_rearms_journal_and_rolls_back_published_destination() {
        let root = test_dir("database-failure-rollback");
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = test_plan(&root, test_manifest(vec![file.clone()]));
        seed_chunks(&plan, &[&file]);
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"old-save").unwrap();
        let db = StateDb::open_in_memory().unwrap();

        let mut transaction = RestoreTransaction::new(&plan, &roots, Some(&db));
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"save");

        let error = transaction.commit().unwrap_err();

        assert!(matches!(error, StateError::Database(_)));
        assert_eq!(fs::read(&destination).unwrap(), b"old-save");
        assert!(!plan.staging.exists());
        assert!(db
            .get_path_by_canonical(&destination.to_string_lossy())
            .unwrap()
            .is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn crash_recovery_retains_workspace_when_rollback_artifact_is_missing() {
        let root = test_dir("crash-recovery-failure");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = crash_test_plan(&paths, test_manifest(vec![file.clone()]));
        fs::write(
            restore_chunk_path(&plan, &file.chunks[0].hash).unwrap(),
            b"save",
        )
        .unwrap();
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"old-save").unwrap();

        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        let staged = transaction.rollback_views[0].mappings[0].staged.clone();
        fs::remove_file(staged).unwrap();
        std::mem::forget(transaction);

        let report = recover_interrupted_restores(&paths).unwrap();

        assert_eq!(report.failures.len(), 1);
        assert!(plan.staging.exists());
        assert!(plan.staging.join(TRANSACTION_JOURNAL_FILE).exists());
        assert_eq!(fs::read(destination).unwrap(), b"save");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn chunk_eviction_waits_for_enable_and_final_file_reference() {
        let root = test_dir("chunk-refs");
        let exclusive_ready = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let shared_ready = test_file(
            "game/shared-ready.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"shared",
        );
        let shared_background = test_file(
            "game/shared-background.dat",
            PersistenceClass::Ephemeral,
            SemanticRole::Cache,
            b"shared",
        );
        let exclusive_hash = exclusive_ready.chunks[0].hash.clone();
        let shared_hash = shared_ready.chunks[0].hash.clone();
        assert_eq!(shared_background.chunks[0].hash, shared_hash);
        let plan = test_plan(
            &root,
            test_manifest(vec![shared_background, exclusive_ready, shared_ready]),
        );
        fs::write(restore_chunk_path(&plan, &exclusive_hash).unwrap(), b"save").unwrap();
        fs::write(restore_chunk_path(&plan, &shared_hash).unwrap(), b"shared").unwrap();
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);

        assert_eq!(transaction.remaining_chunk_references(&exclusive_hash), 1);
        assert_eq!(transaction.remaining_chunk_references(&shared_hash), 2);
        transaction
            .publish_to(RestoreTarget::ReadyToLaunch)
            .unwrap();
        assert_eq!(transaction.remaining_chunk_references(&exclusive_hash), 0);
        assert_eq!(transaction.remaining_chunk_references(&shared_hash), 1);
        assert!(restore_chunk_path(&plan, &exclusive_hash).unwrap().exists());
        assert!(restore_chunk_path(&plan, &shared_hash).unwrap().exists());

        assert_eq!(transaction.enable_chunk_eviction().unwrap(), 1);
        assert!(!restore_chunk_path(&plan, &exclusive_hash).unwrap().exists());
        assert!(restore_chunk_path(&plan, &shared_hash).unwrap().exists());

        transaction.publish_to(RestoreTarget::Complete).unwrap();
        assert_eq!(transaction.remaining_chunk_references(&shared_hash), 0);
        assert!(!restore_chunk_path(&plan, &shared_hash).unwrap().exists());
        assert_eq!(
            fs::read(root.join("home/.local/share/game/shared-background.dat")).unwrap(),
            b"shared"
        );
        transaction.commit().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn chunk_reference_counts_include_only_entries_selected_by_restore_mode() {
        let root = test_dir("chunk-mode");
        let personal = test_file(
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
        let personal_hash = personal.chunks[0].hash.clone();
        let background_hash = background.chunks[0].hash.clone();
        let mut plan = test_plan(&root, test_manifest(vec![personal, background]));
        plan.mode = RestoreMode::PersonalState;
        let roots = LogicalRootMap::from_home(root.join("home"));
        let transaction = RestoreTransaction::new(&plan, &roots, None);

        assert_eq!(transaction.remaining_chunk_references(&personal_hash), 1);
        assert_eq!(transaction.remaining_chunk_references(&background_hash), 0);
        drop(transaction);
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

        let roots = LogicalRootMap::from_home(&home);
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let report = transaction.publish_to(RestoreTarget::Complete).unwrap();

        assert_eq!(report.reused_entries, 1);
        assert_eq!(report.published_entries, 0);
        assert_eq!(fs::read(&target).unwrap(), b"save");
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), before);
        assert!(fs::read_dir(plan.staging.join("pre_restore"))
            .unwrap()
            .next()
            .is_none());
        transaction.commit().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn published_target_is_reused_when_chunks_are_gone() {
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
        let manifest = test_manifest(vec![file.clone()]);
        let plan = test_plan(&root, manifest.clone());
        seed_chunks(&plan, &[&file]);
        let roots = LogicalRootMap::from_home(root.join("home"));

        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        transaction.commit().unwrap();

        let retry_plan = test_plan(&root.join("retry"), manifest);
        fs::remove_dir_all(retry_plan.staging.join("materialized/.chunks")).unwrap();
        let mut retry = RestoreTransaction::new(&retry_plan, &roots, None);
        let report = retry.publish_to(RestoreTarget::Complete).unwrap();

        let destination = root.join("home/.local/share/game/large.dat");
        assert_eq!(report.published_entries, 0);
        assert_eq!(report.reused_entries, 1);
        assert_eq!(
            fs::metadata(&destination).unwrap().len(),
            payload.len() as u64
        );
        assert_eq!(
            blake3_file(&destination).unwrap(),
            noland_cas::blake3_hex(&payload)
        );
        assert!(!retry_plan.staging.join("materialized/tree").exists());
        retry.commit().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dropping_active_transaction_rolls_back_replaced_and_created_files() {
        let root = test_dir("drop-rollback");
        let replaced = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let created = test_file(
            "game/cache.bin",
            PersistenceClass::Ephemeral,
            SemanticRole::Cache,
            b"cache",
        );
        let plan = test_plan(
            &root,
            test_manifest(vec![replaced.clone(), created.clone()]),
        );
        seed_chunks(&plan, &[&replaced, &created]);
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let replaced_path = home.join(".local/share/game/save.dat");
        let created_path = home.join(".local/share/game/cache.bin");
        fs::create_dir_all(replaced_path.parent().unwrap()).unwrap();
        fs::write(&replaced_path, b"old-save").unwrap();
        let cached_pack = plan.pack_cache.join("aa/aa-cancelled.pack");
        fs::create_dir_all(cached_pack.parent().unwrap()).unwrap();
        fs::write(&cached_pack, b"cached pack").unwrap();
        let staged_pack = plan.staging.join("packs/aa-cancelled.pack");
        fs::hard_link(&cached_pack, &staged_pack).unwrap();

        {
            let mut transaction = RestoreTransaction::new(&plan, &roots, None);
            transaction.publish_to(RestoreTarget::Complete).unwrap();
            assert_eq!(fs::read(&replaced_path).unwrap(), b"save");
            assert_eq!(fs::read(&created_path).unwrap(), b"cache");
        }

        assert_eq!(fs::read(&replaced_path).unwrap(), b"old-save");
        assert!(!created_path.exists());
        assert!(!plan.staging.exists());
        assert!(!cached_pack.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_reconstruction_keeps_old_destination_and_removes_partial() {
        let root = test_dir("corrupt-atomic");
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = test_plan(&root, test_manifest(vec![file.clone()]));
        seed_chunks(&plan, &[&file]);
        let chunk = &file.chunks[0];
        fs::write(
            plan.staging
                .join("materialized/.chunks")
                .join(chunk.hash.trim_start_matches("blake3:")),
            b"evil",
        )
        .unwrap();
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"old-save").unwrap();
        let partial = destination_partial_path(&destination, plan.restore_id).unwrap();

        {
            let mut transaction = RestoreTransaction::new(&plan, &roots, None);
            assert!(transaction.publish_to(RestoreTarget::Complete).is_err());
        }

        assert_eq!(fs::read(&destination).unwrap(), b"old-save");
        assert!(!partial.exists());
        assert!(!plan.staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn abort_restores_complete_phase_tombstone() {
        let root = test_dir("tombstone-rollback");
        let mut manifest = test_manifest(Vec::new());
        manifest.tombstones.push(ManifestTombstone {
            logical_root: "$XDG_DATA_HOME".into(),
            relative_path: "game/deleted.dat".into(),
            reason: "deleted at source".into(),
        });
        let plan = test_plan(&root, manifest);
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/deleted.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"keep-on-abort").unwrap();

        {
            let mut transaction = RestoreTransaction::new(&plan, &roots, None);
            transaction.publish_to(RestoreTarget::Complete).unwrap();
            assert!(!destination.exists());
        }

        assert_eq!(fs::read(&destination).unwrap(), b"keep-on-abort");
        assert!(!plan.staging.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staged_pack_release_unlinks_staging_before_full_cache_prune() {
        let root = test_dir("release-staged-pack");
        let plan = test_plan(&root, test_manifest(Vec::new()));
        let pack_id = "aa-release";
        let cached_pack = plan.pack_cache.join("aa").join(format!("{pack_id}.pack"));
        fs::create_dir_all(cached_pack.parent().unwrap()).unwrap();
        fs::write(&cached_pack, b"verified pack").unwrap();
        let staged_pack = plan.staging.join("packs").join(format!("{pack_id}.pack"));
        fs::hard_link(&cached_pack, &staged_pack).unwrap();
        let roots = LogicalRootMap::from_home(root.join("home"));
        let transaction = RestoreTransaction::new(&plan, &roots, None);

        assert_eq!(transaction.release_staged_pack_links().unwrap(), 1);
        assert!(!staged_pack.exists());
        assert!(cached_pack.exists());
        let report = transaction.prune_restore_pack_cache().unwrap();
        assert_eq!(report.packs_pruned, 1);
        assert_eq!(report.packs_after, 0);
        assert!(!cached_pack.exists());
        transaction.commit().unwrap();
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

    #[test]
    fn planned_pack_count_excludes_irrelevant_and_locally_satisfied_packs() {
        let root = test_dir("planned-pack-count");
        let master = MasterKey::generate();
        let needed_payload = b"needed-state".to_vec();
        let irrelevant_payload = b"old-parent-state".to_vec();
        let needed_hash = noland_cas::blake3_hex(&needed_payload);
        let irrelevant_hash = noland_cas::blake3_hex(&irrelevant_payload);
        let needed_pack = pack_chunks(
            &root.join("needed-pack"),
            &master,
            vec![(needed_hash.clone(), needed_payload.clone())],
            |_| false,
        )
        .unwrap()
        .remove(0);
        let irrelevant_pack = pack_chunks(
            &root.join("irrelevant-pack"),
            &master,
            vec![(irrelevant_hash, irrelevant_payload)],
            |_| false,
        )
        .unwrap()
        .remove(0);
        let mut index = needed_pack.entries;
        index.extend(irrelevant_pack.entries);
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            &needed_payload,
        );
        let plan = test_plan(&root, test_manifest(vec![file]));

        assert_eq!(
            planned_pack_download_count(&plan, &index, RestoreTarget::Complete).unwrap(),
            1
        );
        let chunk = plan
            .staging
            .join("materialized/.chunks")
            .join(needed_hash.trim_start_matches("blake3:"));
        fs::write(chunk, needed_payload).unwrap();
        assert_eq!(
            planned_pack_download_count(&plan, &index, RestoreTarget::Complete).unwrap(),
            0
        );
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

    fn crash_test_plan(paths: &AgentPaths, manifest: BundleManifest) -> RestorePlan {
        let restore_id = Uuid::new_v4();
        let staging = paths.restore_dir(&restore_id.to_string());
        for child in ["packs", "materialized/.chunks", "pre_restore"] {
            fs::create_dir_all(staging.join(child)).unwrap();
        }
        RestorePlan {
            restore_id,
            staging,
            pack_cache: paths.cache.join("restore-packs"),
            manifest,
            mode: RestoreMode::CompleteApplication,
        }
    }

    fn test_plan(root: &Path, manifest: BundleManifest) -> RestorePlan {
        let staging = root.join("restore");
        for child in ["packs", "materialized/.chunks", "pre_restore"] {
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
