//! Preview-frozen full deletion, including the transitive pack-sharing closure.
//! Destructive plans are durable before the first tombstone/unlink. Pending plans
//! block new managed transfers; retries never widen the confirmed selection.
use crate::{
    commit::download_metadata, open_metadata, seal_metadata, BundleDeletion, RemoteKey,
    SharedStorageProvider,
};
use bytes::Bytes;
use noland_crypto::{derive_keys, MasterKey};
use noland_state_core::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

const LIMIT_BUNDLES: usize = 10_000;
const LIMIT_PACKS: usize = 200_000;

mod execution;
pub use execution::{
    execute_full_deletion_observed, DeletionObserver, DeletionProgress, PrevalidatedDeletion,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeletionTarget {
    pub deletion: BundleDeletion,
    pub name: String,
    pub logical_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FullDeletionPlan {
    pub plan_id: Uuid,
    pub operation_id: Uuid,
    pub owner: String,
    pub automatic: bool,
    pub targets: Vec<DeletionTarget>,
    pub requested_count: usize,
    pub packs: BTreeMap<String, u64>,
    pub metadata: BTreeMap<String, u64>,
    target_hashes: BTreeMap<Uuid, (String, String)>,
    inventory_hash: String,
}

impl FullDeletionPlan {
    pub fn physical_bytes(&self) -> u64 {
        self.packs
            .values()
            .chain(self.metadata.values())
            .fold(0u64, |sum, n| sum.saturating_add(*n))
    }
    pub fn logical_bytes(&self) -> u64 {
        self.targets
            .iter()
            .fold(0u64, |sum, t| sum.saturating_add(t.logical_bytes))
    }
    fn validate(&self) -> Result<()> {
        if self.targets.is_empty()
            || self.targets.len() > LIMIT_BUNDLES
            || self.packs.len() > LIMIT_PACKS
            || self.requested_count > self.targets.len()
        {
            return Err(StateError::Integrity(
                "invalid full deletion plan bounds".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        for target in &self.targets {
            if !ids.insert(target.deletion.bundle_id) {
                return Err(StateError::Integrity("duplicate deletion target".into()));
            }
        }
        if self.target_hashes.keys().copied().collect::<BTreeSet<_>>() != ids {
            return Err(StateError::Integrity(
                "deletion target fingerprints are incomplete".into(),
            ));
        }
        for id in self.packs.keys() {
            canonical_pack(id)?;
        }
        for key in self.metadata.keys() {
            crate::deletion::validate_deletable_key(&RemoteKey::new(key))?;
            if !self.targets.iter().any(|t| {
                key.starts_with(&format!(
                    "{}/",
                    bundle_dir(&t.deletion.app_id, t.deletion.bundle_id)
                ))
            }) {
                return Err(StateError::UnsafePath(
                    "deletion metadata is outside confirmed bundles".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct InventoryBundle {
    target: DeletionTarget,
    packs: BTreeSet<String>,
    metadata: BTreeMap<String, u64>,
    manifest_hash: String,
    index_hash: String,
    parent: Option<Uuid>,
}

fn plan_key(id: Uuid) -> RemoteKey {
    RemoteKey::new(format!("deletions/plans/{id}.enc"))
}
fn done_key(id: Uuid) -> RemoteKey {
    RemoteKey::new(format!("deletions/plans/{id}.DONE"))
}
fn pending_key(id: Uuid) -> RemoteKey {
    RemoteKey::new(format!("maintenance/pending/{id}.enc"))
}
fn retired_key(id: &str) -> RemoteKey {
    RemoteKey::new(format!("deletions/packs/{id}.enc"))
}

#[derive(Serialize, Deserialize)]
struct RetiredPacks {
    plan_id: Uuid,
    packs: BTreeSet<String>,
}

pub(crate) async fn completed(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    id: Uuid,
) -> Result<bool> {
    let key = done_key(id);
    if provider.stat(&key).await?.is_none() {
        return Ok(false);
    }
    let observed: Uuid = open_metadata(
        &derive_keys(master).catalog,
        b"full-deletion-done-v1",
        download_metadata(provider, &key, 16 * 1024).await?,
    )?;
    if observed != id {
        return Err(StateError::Integrity(
            "deletion completion identity mismatch".into(),
        ));
    }
    Ok(true)
}

fn canonical_pack(id: &str) -> Result<()> {
    if Uuid::parse_str(id)
        .ok()
        .is_none_or(|uuid| uuid.to_string() != id)
    {
        return Err(StateError::Integrity(
            "noncanonical pack identity in index".into(),
        ));
    }
    Ok(())
}

async fn inventory(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<Vec<InventoryBundle>> {
    let entries = provider.list_prefix(&RemoteKey::new("bundles")).await?;
    if entries.len() > LIMIT_BUNDLES * 16 {
        return Err(StateError::Storage(
            "bundle inventory exceeds maintenance bounds".into(),
        ));
    }
    let sizes: BTreeMap<_, _> = entries
        .iter()
        .filter(|e| !e.is_prefix)
        .map(|e| (e.key.as_str().to_owned(), e.size))
        .collect();
    if sizes.len() != entries.iter().filter(|entry| !entry.is_prefix).count() {
        return Err(StateError::Integrity("ambiguous bundle inventory".into()));
    }
    // The catalog is a completeness cross-check, never the reference inventory:
    // it may omit branches, but an omitted known live commit is proof that a
    // listing is incomplete. Never interpret that as permission to free packs.
    let catalog = crate::load_catalog(provider, master).await?;
    for app in &catalog.apps {
        for bundle in &app.bundles {
            if !sizes.contains_key(&format!(
                "{}/COMMITTED",
                bundle_dir(&app.app_id, bundle.bundle_id)
            )) {
                return Err(StateError::Integrity("cloud listing omitted a catalogued live bundle; full deletion requires a complete inventory".into()));
            }
        }
    }
    let mut output = Vec::new();
    let mut total_references = 0usize;
    for entry in entries
        .iter()
        .filter(|e| !e.is_prefix && e.key.as_str().ends_with("/COMMITTED"))
    {
        if output.len() == LIMIT_BUNDLES {
            return Err(StateError::Storage("too many committed bundles".into()));
        }
        let parts: Vec<_> = entry.key.as_str().split('/').collect();
        if parts.len() != 4 || parts[0] != "bundles" {
            return Err(StateError::Integrity("invalid committed bundle key".into()));
        }
        let id = Uuid::parse_str(parts[2])
            .map_err(|_| StateError::Integrity("invalid committed bundle ID".into()))?;
        if crate::bundle_is_deleted(provider, master, id).await? {
            continue;
        }
        let prefix = format!("bundles/{}/{}", parts[1], id);
        let raw = download_metadata(
            provider,
            &RemoteKey::new(format!("{prefix}/manifest.enc")),
            crate::MAX_METADATA_BYTES as u64 + 1024 * 1024,
        )
        .await?;
        let manifest: BundleManifest =
            open_metadata(&derive_keys(master).manifest, b"manifest", raw)?;
        if manifest.bundle_id != id || bundle_dir(&manifest.app.app_id, id) != prefix {
            return Err(StateError::Integrity(
                "committed manifest identity differs from storage path".into(),
            ));
        }
        if catalog
            .apps
            .iter()
            .filter(|app| app.app_id == manifest.app.app_id)
            .flat_map(|app| &app.bundles)
            .any(|bundle| bundle.bundle_id == id && bundle.commit_id != manifest.commit_id)
        {
            return Err(StateError::Integrity(
                "catalog/manifest identity mismatch".into(),
            ));
        }
        let marker = download_metadata(provider, &entry.key, 64).await?;
        if marker != manifest.commit_id.to_string().as_bytes() {
            return Err(StateError::Integrity(
                "commit marker differs from authenticated manifest".into(),
            ));
        }
        let index_key = format!("{prefix}/index.enc");
        let index: Vec<noland_pack::PackIndexEntry> = if sizes.contains_key(&index_key) {
            crate::read_pack_index(provider, master, &manifest.app.app_id, id).await?
        } else if manifest.files.iter().any(|f| !f.chunks.is_empty()) {
            return Err(StateError::Integrity(
                "committed data bundle has no pack index".into(),
            ));
        } else {
            Vec::new()
        };
        let by_hash: HashMap<_, _> = index
            .iter()
            .map(|e| (e.chunk_hash.as_str(), u64::from(e.plaintext_len)))
            .collect();
        for chunk in manifest.files.iter().flat_map(|f| &f.chunks) {
            if by_hash.get(chunk.hash.as_str()) != Some(&chunk.size) {
                return Err(StateError::Integrity(
                    "manifest chunk missing from authenticated pack index".into(),
                ));
            }
        }
        let mut packs = BTreeSet::new();
        for item in &index {
            canonical_pack(&item.pack_id)?;
            packs.insert(item.pack_id.clone());
        }
        if packs.len() > LIMIT_PACKS {
            return Err(StateError::Storage(
                "bundle pack inventory exceeds bounds".into(),
            ));
        }
        total_references = total_references.saturating_add(packs.len());
        if total_references > LIMIT_PACKS {
            return Err(StateError::Storage(
                "repository reference inventory exceeds maintenance bounds".into(),
            ));
        }
        let metadata = sizes
            .iter()
            .filter(|(k, _)| k.starts_with(&format!("{prefix}/")))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        output.push(InventoryBundle {
            target: DeletionTarget {
                deletion: BundleDeletion {
                    app_id: manifest.app.app_id.clone(),
                    bundle_id: id,
                    commit_id: manifest.commit_id,
                },
                name: format!("{} · {}", manifest.app.display_name, id),
                logical_bytes: manifest.logical_size(),
            },
            packs,
            metadata,
            manifest_hash: crate::compact_json_hash(&manifest)?,
            index_hash: crate::compact_json_hash(&index)?,
            parent: manifest.parent_bundle_id,
        });
    }
    output.sort_by_key(|b| b.target.deletion.bundle_id);
    Ok(output)
}

async fn build_plan(
    provider: &dyn SharedStorageProvider,
    items: &[InventoryBundle],
    selected: &BTreeSet<Uuid>,
    plan_id: Uuid,
    operation_id: Uuid,
    owner: &str,
    requested_count: usize,
    automatic: bool,
) -> Result<FullDeletionPlan> {
    let mut packs = BTreeSet::new();
    let mut metadata = BTreeMap::new();
    let mut targets = Vec::new();
    let mut target_hashes = BTreeMap::new();
    for bundle in items
        .iter()
        .filter(|b| selected.contains(&b.target.deletion.bundle_id))
    {
        packs.extend(bundle.packs.iter().cloned());
        metadata.extend(bundle.metadata.clone());
        targets.push(bundle.target.clone());
        target_hashes.insert(
            bundle.target.deletion.bundle_id,
            (bundle.manifest_hash.clone(), bundle.index_hash.clone()),
        );
    }
    if automatic {
        for bundle in items
            .iter()
            .filter(|b| !selected.contains(&b.target.deletion.bundle_id))
        {
            packs.retain(|id| !bundle.packs.contains(id));
        }
    }
    if packs.len() > LIMIT_PACKS {
        return Err(StateError::Storage(
            "deletion pack inventory exceeds bounds".into(),
        ));
    }
    let remote = provider.list_prefix(&RemoteKey::new("packs")).await?;
    if remote.len() > LIMIT_PACKS * 4 {
        return Err(StateError::Storage(
            "remote pack inventory exceeds bounds".into(),
        ));
    }
    let sizes: HashMap<_, _> = remote
        .into_iter()
        .filter(|e| !e.is_prefix)
        .map(|e| (e.key.as_str().to_owned(), e.size))
        .collect();
    let mut pack_sizes = BTreeMap::new();
    for id in packs {
        let size = sizes.get(&pack_key(&id)).ok_or_else(|| {
            StateError::Integrity("referenced pack is missing before deletion preview".into())
        })?;
        pack_sizes.insert(id, *size);
    }
    let plan = FullDeletionPlan {
        plan_id,
        operation_id,
        owner: owner.into(),
        automatic,
        targets,
        requested_count,
        packs: pack_sizes,
        metadata,
        target_hashes,
        inventory_hash: crate::compact_json_hash(items)?,
    };
    plan.validate()?;
    Ok(plan)
}

pub async fn prepare_full_deletion(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    requested: &[BundleDeletion],
    plan_id: Uuid,
    owner: &str,
) -> Result<FullDeletionPlan> {
    if requested.is_empty() {
        return Err(StateError::Invalid("select at least one backup".into()));
    }
    provider.check_full_deletion_support().await?;
    let items = inventory(provider, master).await?;
    let mut selected = BTreeSet::new();
    for deletion in requested {
        if !items.iter().any(|b| b.target.deletion == *deletion) {
            return Err(StateError::Conflict(
                "selected backup changed; preview again".into(),
            ));
        }
        selected.insert(deletion.bundle_id);
    }
    expand_plan(provider, &items, selected, plan_id, owner).await
}

/// Resolve UI selections from this preview's authenticated inventory. Do not
/// fetch the catalog/manifests once for selection and again for dependency work.
/// Confirmation still obtains a new inventory and checks the frozen hash.
pub async fn prepare_full_deletion_selected(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    requested: &[(AppId, Uuid)],
    plan_id: Uuid,
    owner: &str,
) -> Result<FullDeletionPlan> {
    if requested.is_empty() || requested.len() > LIMIT_BUNDLES {
        return Err(StateError::Invalid(
            "select 1–10000 explicit backups".into(),
        ));
    }
    provider.check_full_deletion_support().await?;
    let items = inventory(provider, master).await?;
    let mut selected = BTreeSet::new();
    for (app, id) in requested {
        if !items.iter().any(|bundle| {
            &bundle.target.deletion.app_id == app && bundle.target.deletion.bundle_id == *id
        }) {
            return Err(StateError::NotFound(
                "selected backup is not in the authenticated inventory".into(),
            ));
        }
        selected.insert(*id);
    }
    expand_plan(provider, &items, selected, plan_id, owner).await
}

async fn expand_plan(
    provider: &dyn SharedStorageProvider,
    items: &[InventoryBundle],
    mut selected: BTreeSet<Uuid>,
    plan_id: Uuid,
    owner: &str,
) -> Result<FullDeletionPlan> {
    let requested_count = selected.len();
    // Linear worklist closure: A shares P with B, B shares Q with C.
    let by_id: HashMap<_, _> = items
        .iter()
        .enumerate()
        .map(|(i, b)| (b.target.deletion.bundle_id, i))
        .collect();
    let mut owners = HashMap::<&str, Vec<Uuid>>::new();
    for bundle in items {
        for pack in &bundle.packs {
            owners
                .entry(pack)
                .or_default()
                .push(bundle.target.deletion.bundle_id);
        }
    }
    let mut queue = std::collections::VecDeque::from_iter(selected.iter().copied());
    let mut visited_packs = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        for pack in &items[by_id[&id]].packs {
            if !visited_packs.insert(pack.as_str()) {
                continue;
            }
            for other in &owners[pack.as_str()] {
                if selected.insert(*other) {
                    queue.push_back(*other);
                }
            }
        }
    }
    build_plan(
        provider,
        items,
        &selected,
        plan_id,
        plan_id,
        owner,
        requested_count,
        false,
    )
    .await
}

pub async fn load_full_deletion(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    id: Uuid,
) -> Result<Option<FullDeletionPlan>> {
    let key = plan_key(id);
    if provider.stat(&key).await?.is_none() {
        return Ok(None);
    }
    let plan: FullDeletionPlan = open_metadata(
        &derive_keys(master).catalog,
        b"full-deletion-plan-v1",
        download_metadata(
            provider,
            &key,
            crate::MAX_METADATA_BYTES as u64 + 1024 * 1024,
        )
        .await?,
    )?;
    if plan.plan_id != id {
        return Err(StateError::Integrity(
            "deletion plan identity mismatch".into(),
        ));
    }
    plan.validate()?;
    Ok(Some(plan))
}

pub async fn pending_deletions(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<Vec<FullDeletionPlan>> {
    let entries = provider
        .list_prefix(&RemoteKey::new("maintenance/pending"))
        .await?;
    if entries.len() > LIMIT_BUNDLES * 2 {
        return Err(StateError::Storage(
            "maintenance history exceeds bounds".into(),
        ));
    }
    let mut pending = Vec::new();
    for entry in entries
        .iter()
        .filter(|e| !e.is_prefix && e.key.as_str().ends_with(".enc"))
    {
        let stem = entry
            .key
            .as_str()
            .strip_prefix("maintenance/pending/")
            .and_then(|p| p.strip_suffix(".enc"))
            .ok_or_else(|| StateError::Integrity("invalid deletion plan key".into()))?;
        let id = Uuid::parse_str(stem)
            .map_err(|_| StateError::Integrity("invalid deletion plan ID".into()))?;
        let observed: Uuid = open_metadata(
            &derive_keys(master).catalog,
            b"pending-deletion-v1",
            download_metadata(provider, &entry.key, 16 * 1024).await?,
        )?;
        if observed != id {
            return Err(StateError::Integrity(
                "pending deletion identity mismatch".into(),
            ));
        }
        if completed(provider, master, id).await? {
            continue;
        }
        pending.push(
            load_full_deletion(provider, master, id)
                .await?
                .ok_or_else(|| StateError::Integrity("deletion plan disappeared".into()))?,
        );
    }
    Ok(pending)
}

pub async fn retired_pack_ids(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<BTreeSet<String>> {
    let key = retired_key("INDEX");
    if provider.stat(&key).await?.is_none() {
        return Ok(BTreeSet::new());
    }
    let record: RetiredPacks = open_metadata(
        &derive_keys(master).catalog,
        b"retired-packs-v1",
        download_metadata(
            provider,
            &key,
            crate::MAX_METADATA_BYTES as u64 + 1024 * 1024,
        )
        .await?,
    )?;
    if record.packs.len() > LIMIT_PACKS {
        return Err(StateError::Integrity(
            "pack retirement index exceeds bounds".into(),
        ));
    }
    for pack in &record.packs {
        canonical_pack(pack)?;
    }
    Ok(record.packs)
}

pub async fn execute_full_deletion(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &FullDeletionPlan,
) -> Result<()> {
    execute_full_deletion_observed(provider, master, plan, None).await
}

/// Retire old generations only after the replacement manifest AND catalog are
/// durable. Packs still used by the current bundle/another app belong to those
/// bundles and are not garbage. Never automatically expand to another app.
pub async fn converge_application(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    operation_id: Uuid,
    owner: &str,
) -> Result<()> {
    if let Some(plan) = load_full_deletion(provider, master, manifest.bundle_id).await? {
        if plan.automatic
            && plan.owner == owner
            && completed(provider, master, plan.plan_id).await?
        {
            return Ok(());
        }
        if !plan.automatic || plan.operation_id != operation_id || plan.owner != owner {
            return Err(StateError::Conflict(
                "replacement cleanup identity changed".into(),
            ));
        }
        return execute_full_deletion(provider, master, &plan).await;
    }
    let app_prefix = format!("bundles/{}", manifest.app.app_id.storage_safe());
    let current_marker = format!(
        "{}/COMMITTED",
        bundle_dir(&manifest.app.app_id, manifest.bundle_id)
    );
    let markers = provider.list_prefix(&RemoteKey::new(app_prefix)).await?;
    if markers
        .iter()
        .any(|entry| entry.key.as_str() == current_marker)
        && markers
            .iter()
            .filter(|entry| !entry.is_prefix && entry.key.as_str().ends_with("/COMMITTED"))
            .all(|entry| entry.key.as_str() == current_marker)
    {
        return Ok(()); // no historical generation to reclaim, no payload reread
    }
    let committed =
        crate::read_committed_manifest(provider, master, &manifest.app.app_id, manifest.bundle_id)
            .await?;
    if crate::compact_json_hash(&committed)? != crate::compact_json_hash(manifest)? {
        return Err(StateError::Integrity(
            "replacement is not the committed revision".into(),
        ));
    }
    let catalog = crate::load_catalog(provider, master).await?;
    if !catalog
        .apps
        .iter()
        .any(|a| a.app_id == manifest.app.app_id && a.latest_bundle_id == manifest.bundle_id)
    {
        return Err(StateError::Conflict(
            "replacement catalog pointer is not durable".into(),
        ));
    }
    let items = inventory(provider, master).await?;
    let selected: BTreeSet<_> = items
        .iter()
        .filter(|b| {
            b.target.deletion.app_id == manifest.app.app_id
                && b.target.deletion.bundle_id != manifest.bundle_id
        })
        .map(|b| b.target.deletion.bundle_id)
        .collect();
    if selected.is_empty() {
        return Ok(());
    }
    let mut ancestors = BTreeSet::new();
    let mut parent = manifest.parent_bundle_id;
    while let Some(id) = parent {
        if !ancestors.insert(id) {
            return Err(StateError::Integrity("bundle parent cycle".into()));
        }
        parent = items
            .iter()
            .find(|b| b.target.deletion.bundle_id == id)
            .and_then(|b| b.parent);
    }
    if !selected.is_subset(&ancestors) {
        return Err(StateError::Conflict("replacement committed, but divergent app backups require explicit resolution before convergence".into()));
    }
    let plan = build_plan(
        provider,
        &items,
        &selected,
        manifest.bundle_id,
        operation_id,
        owner,
        selected.len(),
        true,
    )
    .await?;
    execute_full_deletion(provider, master, &plan).await
}

#[cfg(test)]
#[path = "reclamation/optimization_tests.rs"]
mod optimization_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalStorage, RepositoryGuard};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) fn fixture() -> (PathBuf, LocalStorage, MasterKey) {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/full-delete-{}", Uuid::new_v4()));
        (
            root.clone(),
            LocalStorage::new(root.join("cloud")),
            MasterKey::generate(),
        )
    }
    pub(super) fn pack(root: &Path, key: &MasterKey, bytes: &[u8]) -> noland_pack::BuiltPack {
        noland_pack::pack_chunks(
            &root.join("prepared"),
            key,
            [(
                format!("blake3:{}", blake3::hash(bytes).to_hex()),
                bytes.to_vec(),
            )],
            |_| false,
        )
        .unwrap()
        .remove(0)
    }
    pub(super) async fn bundle(
        storage: &dyn SharedStorageProvider,
        key: &MasterKey,
        app: &str,
        packs: &[&noland_pack::BuiltPack],
        parent: Option<Uuid>,
    ) -> BundleManifest {
        let mut manifest = BundleManifest::new(
            ManifestApp::from(&AppIdentity::new(AppId::desktop(app), app)),
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "fixture".into(),
                captured_at: chrono::Utc::now(),
            },
            BackupMode::CompleteApplication,
        );
        manifest.parent_bundle_id = parent;
        let index = packs
            .iter()
            .flat_map(|p| p.entries.iter().cloned())
            .collect::<Vec<_>>();
        let paths = packs
            .iter()
            .map(|p| (p.pack_id.clone(), p.path.clone()))
            .collect::<Vec<_>>();
        crate::commit_bundle_with_index(
            storage,
            key,
            &manifest,
            &paths,
            Some(&serde_json::to_vec(&index).unwrap()),
            None,
        )
        .await
        .unwrap();
        manifest
    }
    pub(super) fn target(manifest: &BundleManifest) -> BundleDeletion {
        BundleDeletion {
            app_id: manifest.app.app_id.clone(),
            bundle_id: manifest.bundle_id,
            commit_id: manifest.commit_id,
        }
    }

    #[tokio::test]
    async fn full_delete_expands_transitive_dependencies_and_removes_real_payloads() {
        let (root, storage, key) = fixture();
        let p = pack(&root, &key, b"shared P");
        let q = pack(&root, &key, b"shared Q");
        let r = pack(&root, &key, b"untouched R");
        let a = bundle(&storage, &key, "A", &[&p], None).await;
        let b = bundle(&storage, &key, "B", &[&p, &q], None).await;
        let c = bundle(&storage, &key, "C", &[&q], None).await;
        let d = bundle(&storage, &key, "D", &[&r], None).await;
        // B/C are intentionally absent from the catalog. Authoritative committed
        // bundle discovery must still include them in the confirmation.
        crate::update_catalog_with_bundle(&storage, &key, &a, 0)
            .await
            .unwrap();
        crate::update_catalog_with_bundle(&storage, &key, &d, 0)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let guard = RepositoryGuard::acquire(&storage, &key, id, "fixture")
            .await
            .unwrap();
        let plan = prepare_full_deletion(&storage, &key, &[target(&a)], id, "fixture")
            .await
            .unwrap();
        assert_eq!(plan.requested_count, 1);
        assert_eq!(plan.targets.len(), 3);
        assert_eq!(plan.packs.len(), 2);
        assert!(plan.physical_bytes() >= p.bytes + q.bytes);
        guard.release().await.unwrap();
        assert!(
            crate::read_committed_manifest(&storage, &key, &a.app.app_id, a.bundle_id)
                .await
                .is_ok(),
            "preview never deletes"
        );
        let guard = RepositoryGuard::acquire(&storage, &key, id, "fixture")
            .await
            .unwrap();
        execute_full_deletion(&storage, &key, &plan).await.unwrap();
        execute_full_deletion(&storage, &key, &plan).await.unwrap();
        guard.release().await.unwrap();
        for gone in [&a, &b, &c] {
            assert!(crate::read_committed_manifest(
                &storage,
                &key,
                &gone.app.app_id,
                gone.bundle_id
            )
            .await
            .is_err());
            assert!(storage
                .list_prefix(&RemoteKey::new(bundle_dir(
                    &gone.app.app_id,
                    gone.bundle_id
                )))
                .await
                .unwrap()
                .is_empty());
        }
        for gone in [&p, &q] {
            assert!(storage
                .stat(&RemoteKey::new(pack_key(&gone.pack_id)))
                .await
                .unwrap()
                .is_none());
        }
        assert_eq!(
            noland_pack::extract_chunk(
                &storage.root.join(pack_key(&r.pack_id)),
                &r.entries[0],
                &key
            )
            .unwrap(),
            b"untouched R"
        );
        assert!(
            crate::read_committed_manifest(&storage, &key, &d.app.app_id, d.bundle_id)
                .await
                .is_ok()
        );
        assert!(pending_deletions(&storage, &key).await.unwrap().is_empty());
        let mut replay = a.clone();
        replay.bundle_id = Uuid::new_v4();
        replay.commit_id = Uuid::new_v4();
        assert!(crate::commit_bundle_with_index(
            &storage,
            &key,
            &replay,
            &[],
            Some(&serde_json::to_vec(&p.entries).unwrap()),
            None
        )
        .await
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn changed_or_corrupt_inventory_fails_before_any_deletion() {
        let (root, storage, key) = fixture();
        let p = pack(&root, &key, b"P");
        let a = bundle(&storage, &key, "A", &[&p], None).await;
        let plan = prepare_full_deletion(&storage, &key, &[target(&a)], Uuid::new_v4(), "fixture")
            .await
            .unwrap();
        let _other = bundle(&storage, &key, "new dependent", &[&p], None).await;
        let guard = RepositoryGuard::acquire(&storage, &key, plan.operation_id, "fixture")
            .await
            .unwrap();
        assert!(execute_full_deletion(&storage, &key, &plan).await.is_err());
        guard.release().await.unwrap();
        assert!(load_full_deletion(&storage, &key, plan.plan_id)
            .await
            .unwrap()
            .is_none());
        assert!(
            crate::read_committed_manifest(&storage, &key, &a.app.app_id, a.bundle_id)
                .await
                .is_ok()
        );
        std::fs::write(
            storage.root.join(format!(
                "{}/index.enc",
                bundle_dir(&a.app.app_id, a.bundle_id)
            )),
            b"corrupt",
        )
        .unwrap();
        assert!(
            prepare_full_deletion(&storage, &key, &[target(&a)], Uuid::new_v4(), "fixture")
                .await
                .is_err()
        );
        assert!(storage
            .stat(&RemoteKey::new(pack_key(&p.pack_id)))
            .await
            .unwrap()
            .is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct FailDelete {
        inner: LocalStorage,
        fail: AtomicBool,
        claim_barrier: Option<tokio::sync::Barrier>,
        hide_bundle: Option<Uuid>,
        hide_pending_once: AtomicBool,
    }
    #[async_trait::async_trait]
    impl SharedStorageProvider for FailDelete {
        async fn health_check(&self) -> Result<crate::Health> {
            self.inner.health_check().await
        }
        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }
        async fn stat(&self, key: &RemoteKey) -> Result<Option<crate::RemoteMeta>> {
            self.inner.stat(key).await
        }
        async fn upload_immutable(
            &self,
            local: &Path,
            key: &RemoteKey,
        ) -> Result<crate::RemoteMeta> {
            self.inner.upload_immutable(local, key).await
        }
        async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
            self.inner.download(key, path).await
        }
        async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<crate::RemoteEntry>> {
            if key.as_str() == "maintenance/pending"
                && self.hide_pending_once.swap(false, Ordering::SeqCst)
            {
                return Ok(Vec::new());
            }
            let mut entries = self.inner.list_prefix(key).await?;
            if key.as_str() == "bundles" {
                if let Some(id) = self.hide_bundle {
                    entries.retain(|entry| !entry.key.as_str().contains(&id.to_string()));
                }
            }
            Ok(entries)
        }
        async fn put_small_versioned(
            &self,
            bytes: Bytes,
            key: &RemoteKey,
        ) -> Result<crate::RemoteMeta> {
            let result = self.inner.put_small_versioned(bytes, key).await;
            if key.as_str().starts_with("maintenance/activity/") {
                if let Some(barrier) = &self.claim_barrier {
                    barrier.wait().await;
                }
            }
            result
        }
        async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
            if key.as_str().starts_with("packs/") && self.fail.swap(false, Ordering::SeqCst) {
                return Err(StateError::Storage("fixture interrupted delete".into()));
            }
            self.inner.delete_object(key).await
        }
    }

    #[tokio::test]
    async fn partial_delete_resumes_frozen_plan_and_blocks_other_transfers() {
        let (root, inner, key) = fixture();
        let storage = FailDelete {
            inner,
            fail: AtomicBool::new(true),
            claim_barrier: None,
            hide_bundle: None,
            hide_pending_once: AtomicBool::new(false),
        };
        let p = pack(&root, &key, b"P");
        let a = bundle(&storage, &key, "A", &[&p], None).await;
        let plan = prepare_full_deletion(&storage, &key, &[target(&a)], Uuid::new_v4(), "fixture")
            .await
            .unwrap();
        let guard = RepositoryGuard::acquire(&storage, &key, plan.operation_id, "fixture")
            .await
            .unwrap();
        assert!(execute_full_deletion(&storage, &key, &plan).await.is_err());
        guard.release().await.unwrap();
        // Simulate observing no pending plan before admission, followed by the
        // previous execution leaving pending work before our claim is written.
        storage.hide_pending_once.store(true, Ordering::SeqCst);
        assert!(
            RepositoryGuard::acquire(&storage, &key, Uuid::new_v4(), "another PC")
                .await
                .is_err()
        );
        let restored = load_full_deletion(&storage, &key, plan.plan_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored, plan);
        let guard = RepositoryGuard::acquire(&storage, &key, plan.operation_id, "fixture")
            .await
            .unwrap();
        execute_full_deletion(&storage, &key, &restored)
            .await
            .unwrap();
        guard.release().await.unwrap();
        assert!(storage
            .stat(&RemoteKey::new(pack_key(&p.pack_id)))
            .await
            .unwrap()
            .is_none());
        let guard = RepositoryGuard::acquire(&storage, &key, Uuid::new_v4(), "another PC")
            .await
            .unwrap();
        guard.release().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn convergence_keeps_last_good_until_commit_and_never_deletes_another_app() {
        let (root, storage, key) = fixture();
        let p = pack(&root, &key, b"old shared data");
        let q = pack(&root, &key, b"replacement");
        let a = bundle(&storage, &key, "A", &[&p], None).await;
        crate::update_catalog_with_bundle(&storage, &key, &a, 0)
            .await
            .unwrap();
        let other = bundle(&storage, &key, "other", &[&p], None).await;
        let b = bundle(&storage, &key, "A", &[&q], Some(a.bundle_id)).await;
        assert!(
            converge_application(&storage, &key, &b, Uuid::new_v4(), "fixture")
                .await
                .is_err(),
            "catalog publication is required"
        );
        assert!(
            crate::read_committed_manifest(&storage, &key, &a.app.app_id, a.bundle_id)
                .await
                .is_ok()
        );
        crate::update_catalog_with_bundle(&storage, &key, &b, 0)
            .await
            .unwrap();
        let op = Uuid::new_v4();
        let guard = RepositoryGuard::acquire(&storage, &key, op, "fixture")
            .await
            .unwrap();
        converge_application(&storage, &key, &b, op, "fixture")
            .await
            .unwrap();
        guard.release().await.unwrap();
        let catalog = crate::load_catalog(&storage, &key).await.unwrap();
        assert_eq!(catalog.apps[0].bundles.len(), 1);
        assert_eq!(catalog.apps[0].latest_bundle_id, b.bundle_id);
        assert!(
            crate::read_committed_manifest(&storage, &key, &a.app.app_id, a.bundle_id)
                .await
                .is_err()
        );
        assert!(
            crate::read_committed_manifest(&storage, &key, &other.app.app_id, other.bundle_id)
                .await
                .is_ok()
        );
        assert!(storage
            .stat(&RemoteKey::new(pack_key(&p.pack_id)))
            .await
            .unwrap()
            .is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn activity_claims_do_not_expire_or_get_cleared_by_another_owner() {
        let (root, storage, key) = fixture();
        storage.ensure_root().await.unwrap();
        let id = Uuid::new_v4();
        let guard = RepositoryGuard::acquire(&storage, &key, id, "PC A")
            .await
            .unwrap();
        assert!(
            RepositoryGuard::acquire(&storage, &key, Uuid::new_v4(), "PC B")
                .await
                .is_err()
        );
        drop(guard); // crash/cancellation, intentionally no implicit unlock
        assert!(
            RepositoryGuard::acquire(&storage, &key, Uuid::new_v4(), "PC B")
                .await
                .is_err()
        );
        let abandoned = crate::repository_activities(&storage, &key)
            .await
            .unwrap()
            .remove(0);
        crate::clear_repository_activity(&storage, &key, &abandoned)
            .await
            .unwrap();
        let guard = RepositoryGuard::acquire(&storage, &key, id, "PC A")
            .await
            .unwrap();
        guard.release().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_same_operation_never_reenters_or_overwrites_an_active_execution() {
        let (root, storage, key) = fixture();
        let id = Uuid::new_v4();
        let guard = RepositoryGuard::acquire(&storage, &key, id, "PC A")
            .await
            .unwrap();
        let original = crate::repository_activities(&storage, &key).await.unwrap();
        assert!(RepositoryGuard::acquire(&storage, &key, id, "PC A")
            .await
            .is_err());
        assert!(RepositoryGuard::acquire(&storage, &key, id, "PC B")
            .await
            .is_err());
        assert_eq!(
            crate::repository_activities(&storage, &key).await.unwrap(),
            original
        );
        guard.release().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn simultaneous_pc_claims_cannot_both_be_admitted() {
        let (root, inner, key) = fixture();
        let storage = FailDelete {
            inner,
            fail: AtomicBool::new(false),
            claim_barrier: Some(tokio::sync::Barrier::new(2)),
            hide_bundle: None,
            hide_pending_once: AtomicBool::new(false),
        };
        let id = Uuid::new_v4();
        let (a, b) = tokio::join!(
            RepositoryGuard::acquire(&storage, &key, id, "PC A"),
            RepositoryGuard::acquire(&storage, &key, id, "PC B")
        );
        assert!(!(a.is_ok() && b.is_ok()));
        if let Ok(guard) = a {
            guard.release().await.unwrap();
        }
        if let Ok(guard) = b {
            guard.release().await.unwrap();
        }
        assert!(crate::repository_activities(&storage, &key)
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn changed_pack_size_and_missing_index_cannot_be_approved_by_an_old_preview() {
        let (root, storage, key) = fixture();
        let p = pack(&root, &key, b"original");
        let a = bundle(&storage, &key, "A", &[&p], None).await;
        let plan = prepare_full_deletion(&storage, &key, &[target(&a)], Uuid::new_v4(), "fixture")
            .await
            .unwrap();
        std::fs::write(
            storage.root.join(pack_key(&p.pack_id)),
            b"unconfirmed replacement",
        )
        .unwrap();
        let guard = RepositoryGuard::acquire(&storage, &key, plan.operation_id, "fixture")
            .await
            .unwrap();
        assert!(execute_full_deletion(&storage, &key, &plan).await.is_err());
        guard.release().await.unwrap();
        assert!(!crate::bundle_is_deleted(&storage, &key, a.bundle_id)
            .await
            .unwrap());
        // A data-bearing manifest is required to reject a missing index; a
        // truly empty legacy manifest is allowed to have no pack index.
        let mut manifest = a.clone();
        manifest.files.push(ManifestFile {
            logical_root: "$HOME".into(),
            relative_path: "save".into(),
            source_path_hint: None,
            file_type: "file".into(),
            size: 1,
            file_hash: "fixture".into(),
            chunks: vec![ChunkRef {
                hash: p.entries[0].chunk_hash.clone(),
                size: u64::from(p.entries[0].plaintext_len),
            }],
            mode: None,
            mtime_ns: None,
            uid: None,
            gid: None,
            symlink_target: None,
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::Unknown,
            association_confidence: 1.0,
            shared_app_ids: Vec::new(),
        });
        let prefix = bundle_dir(&a.app.app_id, a.bundle_id);
        storage
            .put_small_versioned(
                Bytes::from(
                    seal_metadata(&derive_keys(&key).manifest, b"manifest", &manifest)
                        .unwrap()
                        .envelope,
                ),
                &RemoteKey::new(format!("{prefix}/manifest.enc")),
            )
            .await
            .unwrap();
        std::fs::remove_file(storage.root.join(format!("{prefix}/index.enc"))).unwrap();
        assert!(
            prepare_full_deletion(&storage, &key, &[target(&a)], Uuid::new_v4(), "fixture")
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn an_incomplete_listing_must_not_hide_a_known_dependent_backup() {
        let (root, inner, key) = fixture();
        let p = pack(&root, &key, b"shared");
        let a = bundle(&inner, &key, "A", &[&p], None).await;
        let b = bundle(&inner, &key, "B", &[&p], None).await;
        crate::update_catalog_with_bundle(&inner, &key, &a, 0)
            .await
            .unwrap();
        crate::update_catalog_with_bundle(&inner, &key, &b, 0)
            .await
            .unwrap();
        let storage = FailDelete {
            inner,
            fail: AtomicBool::new(false),
            claim_barrier: None,
            hide_bundle: Some(b.bundle_id),
            hide_pending_once: AtomicBool::new(false),
        };
        assert!(
            prepare_full_deletion(&storage, &key, &[target(&a)], Uuid::new_v4(), "fixture")
                .await
                .is_err()
        );
        assert!(storage.inner.root.join(pack_key(&p.pack_id)).is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires rclone on PATH; uses only a fresh local fixture, never cloud credentials"]
    async fn real_rclone_local_fixture_deletes_payloads_and_empty_bundle_directories() {
        let (root, local, master) = fixture();
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("fixture-rclone.conf");
        std::fs::write(&config, "[fixture]\ntype = local\n").unwrap();
        let session = noland_rclone_adapter::EphemeralRcloneSession {
            provider: "local".into(),
            backend_type: "local".into(),
            remote_name: "fixture".into(),
            root: local.root.to_string_lossy().into(),
            config_ini: "[fixture]\ntype = local\n".into(),
            operation_id: Uuid::new_v4().to_string(),
            expires_at_unix: chrono::Utc::now().timestamp() + 3600,
            upload_concurrency: None,
        };
        let storage = crate::RcloneStorage::try_from_session(&session, &config).unwrap();
        let p = pack(&root, &master, b"real cli selected payload");
        let q = pack(&root, &master, b"real cli preserved payload");
        let a = bundle(&storage, &master, "A", &[&p], None).await;
        let b = bundle(&storage, &master, "B", &[&q], None).await;
        crate::update_catalog_with_bundle(&storage, &master, &a, 0)
            .await
            .unwrap();
        crate::update_catalog_with_bundle(&storage, &master, &b, 0)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let guard = RepositoryGuard::acquire(&storage, &master, id, "fixture")
            .await
            .unwrap();
        let plan = prepare_full_deletion(&storage, &master, &[target(&a)], id, "fixture")
            .await
            .unwrap();
        execute_full_deletion(&storage, &master, &plan)
            .await
            .unwrap();
        guard.release().await.unwrap();
        assert!(!local
            .root
            .join(bundle_dir(&a.app.app_id, a.bundle_id))
            .exists());
        assert!(!local.root.join(pack_key(&p.pack_id)).exists());
        assert_eq!(
            noland_pack::extract_chunk(
                &local.root.join(pack_key(&q.pack_id)),
                &q.entries[0],
                &master
            )
            .unwrap(),
            b"real cli preserved payload"
        );
        assert!(
            crate::read_committed_manifest(&storage, &master, &b.app.app_id, b.bundle_id)
                .await
                .is_ok()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
