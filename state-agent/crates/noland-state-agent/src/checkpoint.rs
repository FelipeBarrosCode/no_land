use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

use noland_state_core::*;
use noland_state_db::LearnedCheckpointGeneration;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::StateAgent;

const PERIODIC_OWNER: &str = "noland-periodic-checkpoints-v1";
const MANIFEST_NAME: &str = "periodic-v1.json";
const RETAIN_PERIODIC: usize = 4;
const RETAIN_PERIODIC_BYTES: u64 = 64 * 1024 * 1024;
// Explicit and periodic publication share the lock. No StateAgent fields or
// background scheduler changes are needed; each tick coalesces changes since
// the last durable checkpoint, including across agent restarts.
static PUBLICATION: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
struct PeriodicManifest {
    owner: String,
    head: Option<CheckpointFile>,
    owned: Vec<CheckpointFile>,
    #[serde(default)]
    covered: Option<CheckpointSource>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CheckpointSource {
    database: LearnedCheckpointGeneration,
    instance_id: Uuid,
    image_id: String,
}

impl CheckpointSource {
    fn new(agent: &StateAgent, database: LearnedCheckpointGeneration) -> Self {
        Self {
            database,
            instance_id: agent.config.instance_id,
            image_id: agent.config.image_id.clone(),
        }
    }

    fn matches_identity(&self, agent: &StateAgent) -> bool {
        self.instance_id == agent.config.instance_id && self.image_id == agent.config.image_id
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CheckpointFile {
    id: Uuid,
    name: String,
    generation: String,
    digest: String,
    bytes: u64,
}

/// Explicit backups/seals always receive a fresh ID and a current snapshot.
/// The returned v1 object remains the input to commit_checkpoint; the local
/// compression format does not change the encrypted cloud upload contract.
pub fn write_local_checkpoint(agent: &StateAgent) -> Result<LearnedStateCheckpoint> {
    let _publication = PUBLICATION.lock();
    let (generation, checkpoint) = agent
        .db
        .learned_checkpoint_versioned(agent.config.instance_id, &agent.config.image_id)?;
    let root = &agent.config.paths.checkpoints;
    let file = publish_checkpoint(root, &checkpoint, false, content_generation(&checkpoint))?;
    // A damaged ownership manifest must not prevent an explicit seal/upload.
    // It also must never authorize deletion of files whose ownership is unknown.
    let remember = || -> Result<()> {
        let mut manifest = read_manifest(root)?;
        manifest.head = Some(file);
        manifest.covered = Some(CheckpointSource::new(agent, generation));
        write_manifest(root, &manifest)
    };
    if let Err(error) = remember() {
        tracing::warn!(%error, "could not record explicit checkpoint for periodic coalescing");
    }
    Ok(checkpoint)
}

/// Accept both historical pretty/compact JSON and new streaming zstd JSON.
/// Detect the content, not the extension, so renamed legacy files remain readable.
pub fn read_local_checkpoint(path: &Path) -> Result<LearnedStateCheckpoint> {
    let mut reader = BufReader::new(File::open(path)?);
    if reader.fill_buf()?.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Ok(serde_json::from_reader(zstd::stream::read::Decoder::new(
            reader,
        )?)?)
    } else {
        Ok(serde_json::from_reader(reader)?)
    }
}

pub fn import_checkpoint(agent: &StateAgent, checkpoint: &LearnedStateCheckpoint) -> Result<()> {
    for app in &checkpoint.apps {
        agent.db.upsert_app(app)?;
    }
    for root in &checkpoint.known_roots {
        agent
            .db
            .add_known_root(&root.app_id, &root.kind, &root.path)?;
    }
    for assoc in &checkpoint.associations {
        let path_id = agent.db.upsert_path(&assoc.canonical_path)?;
        agent.db.upsert_association(&PathAssociation {
            app_id: assoc.app_id.clone(),
            path_id,
            confidence: assoc.confidence,
            evidence: assoc.evidence.iter().copied().map(Evidence::new).collect(),
            persistence_class: assoc.persistence_class,
            semantic_role: assoc.semantic_role,
            first_seen_at: checkpoint.created_at,
            last_seen_at: checkpoint.created_at,
        })?;
    }
    Ok(())
}

pub fn maybe_checkpoint(agent: &StateAgent) -> Result<()> {
    let _publication = PUBLICATION.lock();
    let root = &agent.config.paths.checkpoints;
    let mut manifest = read_manifest(root)?;
    let head_intact = match &manifest.head {
        Some(head) => owned_file_is_intact(root, head)?,
        None => true,
    };
    let covered = manifest
        .covered
        .as_ref()
        .filter(|source| head_intact && source.matches_identity(agent))
        .map(|source| &source.database);
    let Some((source, checkpoint)) = agent.db.learned_checkpoint_if_changed(
        agent.config.instance_id,
        &agent.config.image_id,
        covered,
    )?
    else {
        // The DB has read only its singleton revision, not apps/paths/evidence.
        return prune_periodic(agent, &mut manifest);
    };
    // This source belongs to the same DB snapshot as the rows, even if capture
    // or observation changes the database while the file is being published.
    manifest.covered = Some(CheckpointSource::new(agent, source));
    if checkpoint.apps.is_empty() && manifest.head.is_none() {
        // Remember empty state too, including across restarts. A later transition
        // from populated to empty still publishes a checkpoint.
        return write_manifest(root, &manifest);
    }
    let generation = content_generation(&checkpoint);
    let unchanged = manifest
        .head
        .as_ref()
        .is_some_and(|head| head.generation == generation && head_intact);
    if !unchanged {
        let file = publish_checkpoint(root, &checkpoint, true, generation)?;
        manifest.head = Some(file.clone());
        manifest.owned.push(file);
    }
    // Enroll ownership/coverage only AFTER the checkpoint and directory are
    // durable. Also upgrade legacy manifests or coalesce a change that reverted
    // to identical content without publishing another file.
    write_manifest(root, &manifest)?;
    prune_periodic(agent, &mut manifest)?;
    Ok(())
}

fn publish_checkpoint(
    root: &Path,
    checkpoint: &LearnedStateCheckpoint,
    periodic: bool,
    generation: String,
) -> Result<CheckpointFile> {
    let prefix = if periodic { "periodic-v1-" } else { "" };
    let name = format!("{prefix}{}.json.zst", checkpoint.checkpoint_id);
    let path = root.join(&name);
    atomic_write(&path, false, |file| {
        let buffer = BufWriter::new(file);
        let mut encoder = zstd::stream::write::Encoder::new(buffer, 1)?;
        encoder.include_checksum(true)?;
        serde_json::to_writer(&mut encoder, checkpoint)?;
        encoder.finish()?.flush()?;
        Ok(())
    })?;
    let (digest, bytes) = file_digest(&path)?;
    Ok(CheckpointFile {
        id: checkpoint.checkpoint_id,
        name,
        generation,
        digest,
        bytes,
    })
}

fn read_manifest(root: &Path) -> Result<PeriodicManifest> {
    let path = root.join(MANIFEST_NAME);
    let reader = match File::open(&path) {
        Ok(file) => BufReader::new(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PeriodicManifest {
                owner: PERIODIC_OWNER.into(),
                ..Default::default()
            });
        }
        Err(error) => return Err(error.into()),
    };
    let manifest: PeriodicManifest = serde_json::from_reader(reader)?;
    if manifest.owner != PERIODIC_OWNER
        || manifest
            .owned
            .iter()
            .any(|file| file.name != format!("periodic-v1-{}.json.zst", file.id))
        || manifest.head.as_ref().is_some_and(|file| {
            file.name != format!("periodic-v1-{}.json.zst", file.id)
                && file.name != format!("{}.json.zst", file.id)
        })
    {
        return Err(StateError::Integrity(
            "unrecognized periodic checkpoint ownership manifest".into(),
        ));
    }
    Ok(manifest)
}

fn write_manifest(root: &Path, manifest: &PeriodicManifest) -> Result<()> {
    atomic_write(&root.join(MANIFEST_NAME), true, |file| {
        serde_json::to_writer(file, manifest)?;
        Ok(())
    })
}

fn atomic_write(
    path: &Path,
    replace: bool,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| StateError::Invalid("checkpoint needs a parent directory".into()))?;
    let temporary = parent.join(format!(".checkpoint-tmp-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| -> Result<()> {
        write(&mut file)?;
        file.sync_all()?;
        if replace {
            fs::rename(&temporary, path)?;
        } else {
            // Atomic no-clobber publication: even an unlikely UUID collision can
            // never replace an artifact that was not created by this invocation.
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        }
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn file_digest(path: &Path) -> Result<(String, u64)> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok((hash.finalize().to_hex().to_string(), bytes))
}

fn owned_file_is_intact(root: &Path, file: &CheckpointFile) -> Result<bool> {
    let path = root.join(&file.name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() != file.bytes {
        return Ok(false);
    }
    Ok(file_digest(&path)?.0 == file.digest)
}

fn prune_periodic(agent: &StateAgent, manifest: &mut PeriodicManifest) -> Result<()> {
    if manifest.owned.len() <= RETAIN_PERIODIC
        && manifest
            .owned
            .iter()
            .fold(0u64, |sum, file| sum.saturating_add(file.bytes))
            <= RETAIN_PERIODIC_BYTES
    {
        return Ok(());
    }
    let root = &agent.config.paths.checkpoints;
    let candidates: Vec<_> = manifest.owned.iter().map(|file| file.id).collect();
    let protected = agent.db.referenced_checkpoints(&candidates)?;
    let mut remove = HashSet::new();
    let mut retained_count = 0;
    let mut retained_bytes = 0u64;
    for file in manifest.owned.iter().rev() {
        // Referenced and unrecognized/modified files are never garbage collected,
        // and do not consume the quota for ordinary unreferenced checkpoints.
        if protected.contains(&file.id) || !owned_file_is_intact(root, file)? {
            continue;
        }
        let is_head = manifest
            .head
            .as_ref()
            .is_some_and(|head| head.id == file.id);
        if is_head
            || retained_count == 0
            || (retained_count < RETAIN_PERIODIC
                && retained_bytes.saturating_add(file.bytes) <= RETAIN_PERIODIC_BYTES)
        {
            retained_count += 1;
            retained_bytes = retained_bytes.saturating_add(file.bytes);
        } else {
            fs::remove_file(root.join(&file.name))?;
            remove.insert(file.id);
        }
    }
    if !remove.is_empty() {
        File::open(root)?.sync_all()?;
        manifest.owned.retain(|file| !remove.contains(&file.id));
        write_manifest(root, manifest)?;
    }
    Ok(())
}

/// Hash typed content only after a semantic generation change (or an explicit
/// request). UUID/time identify a publication, not a state generation.
fn content_generation(checkpoint: &LearnedStateCheckpoint) -> String {
    struct Digest(blake3::Hasher);
    impl Hasher for Digest {
        fn write(&mut self, bytes: &[u8]) {
            self.0.update(bytes);
        }
        fn finish(&self) -> u64 {
            unreachable!("use the full BLAKE3 digest")
        }
    }
    let mut hash = Digest(blake3::Hasher::new());
    let LearnedStateCheckpoint {
        schema_version,
        checkpoint_id: _,
        instance_id,
        image_id,
        created_at: _,
        apps,
        associations,
        known_roots,
        latest_bundle_refs,
    } = checkpoint;
    (
        "learned-checkpoint-v1",
        schema_version,
        instance_id,
        image_id,
    )
        .hash(&mut hash);
    apps.len().hash(&mut hash);
    for app in apps {
        let AppIdentity {
            app_id,
            display_name,
            canonical_executable,
            desktop_entry_id,
            steam_app_id,
            launcher,
            aliases,
            identity_confidence,
            icon_path,
        } = app;
        (
            app_id,
            display_name,
            canonical_executable,
            desktop_entry_id,
            steam_app_id,
            launcher.map(|l| l.as_str()),
            aliases,
            identity_confidence.to_bits(),
            icon_path,
        )
            .hash(&mut hash);
    }
    associations.len().hash(&mut hash);
    for association in associations {
        let CheckpointAssociation {
            app_id,
            canonical_path,
            logical_root,
            relative_path,
            confidence,
            persistence_class,
            semantic_role,
            evidence,
        } = association;
        (
            app_id,
            canonical_path,
            logical_root,
            relative_path,
            confidence.to_bits(),
            persistence_class.as_str(),
            semantic_role.as_str(),
            evidence.len(),
        )
            .hash(&mut hash);
        for kind in evidence {
            kind.as_str().hash(&mut hash);
        }
    }
    known_roots.len().hash(&mut hash);
    for root in known_roots {
        let CheckpointRoot { app_id, kind, path } = root;
        (app_id, kind, path).hash(&mut hash);
    }
    latest_bundle_refs.len().hash(&mut hash);
    for reference in latest_bundle_refs {
        let CheckpointBundleRef {
            app_id,
            bundle_id,
            commit_id,
        } = reference;
        (app_id, bundle_id, commit_id).hash(&mut hash);
    }
    hash.0.finalize().to_hex().to_string()
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
