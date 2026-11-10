//! Bounded pack sealing / upload overlap with authenticated per-pack receipts.
//! The captured revision is persisted once, before any network side effects.
use super::*;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct PackingState {
    pub plans: Vec<PackPlan>,
    pub lease: PathBuf,
    pub workers: usize,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct PackPlan {
    chunks: Vec<(String, PathBuf)>,
    target: u64,
    max: u64,
}

pub(super) fn plans(
    small: Vec<(String, PathBuf)>,
    regular: Vec<(String, PathBuf)>,
) -> Result<Vec<PackPlan>> {
    let mut plans = Vec::new();
    for (chunks, target, max) in [
        (small, 16 * 1024 * 1024, 32 * 1024 * 1024),
        (regular, constants::PACK_TARGET, constants::PACK_MAX),
    ] {
        plans.extend(
            plan_pack_chunks(chunks, target, max)?
                .into_iter()
                .map(|chunks| PackPlan {
                    chunks,
                    target,
                    max,
                }),
        );
    }
    Ok(plans)
}

struct Workers {
    receiver: tokio::sync::mpsc::Receiver<Result<BuiltPack>>,
    cancelled: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Wake blocked senders before joining. Cancellation never leaves a pack
        // writer running against a lease that its caller might release.
        self.receiver.close();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn receipt_path(agent: &StateAgent, operation: Uuid, index: usize) -> PathBuf {
    prepared_backup_path(agent, operation)
        .with_extension("packs")
        .join(format!("{index}.enc"))
}

fn receipt_aad(operation: Uuid, index: usize) -> String {
    format!("backup-pack:{operation}:{index}")
}

fn write_receipt(path: &Path, master: &MasterKey, aad: &str, pack: &BuiltPack) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().expect("receipt parent");
    std::fs::create_dir_all(parent)?;
    let encrypted = noland_crypto::wrap_envelope(
        &noland_crypto::derive_keys(master).manifest,
        aad.as_bytes(),
        &serde_json::to_vec(pack)?,
    )?;
    let partial = path.with_extension("partial");
    disk::ensure_space(parent, encrypted.len() as u64)?;
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(&encrypted)?;
    file.sync_all()?;
    std::fs::rename(partial, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    if let Some(parent) = parent.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_receipt(path: &Path, master: &MasterKey, aad: &str) -> Result<Option<BuiltPack>> {
    let encrypted = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let plain = noland_crypto::unwrap_envelope(
        &noland_crypto::derive_keys(master).manifest,
        aad.as_bytes(),
        &encrypted,
    )?;
    Ok(Some(serde_json::from_slice(&plain)?))
}

fn validate_receipt(
    agent: &StateAgent,
    operation: Uuid,
    directory: &Path,
    plan: &PackPlan,
    pack: &BuiltPack,
) -> Result<()> {
    let hashes = plan
        .chunks
        .iter()
        .map(|(hash, _)| hash.as_str())
        .collect::<BTreeSet<_>>();
    let stored_hashes = pack
        .entries
        .iter()
        .map(|entry| entry.chunk_hash.as_str())
        .collect::<BTreeSet<_>>();
    if Uuid::parse_str(&pack.pack_id).is_err()
        || pack.path != directory.join(format!("{}.pack", pack.pack_id))
        || pack.bytes > plan.max
        || hashes != stored_hashes
        || pack.entries.len() != hashes.len()
        || pack
            .entries
            .iter()
            .any(|entry| entry.pack_id != pack.pack_id)
    {
        return Err(StateError::Integrity(
            "pack receipt does not match its captured plan".into(),
        ));
    }
    if !agent
        .db
        .sync_journal_completed(operation, &pack_key(&pack.pack_id))?
        && (std::fs::symlink_metadata(&pack.path)?
            .file_type()
            .is_symlink()
            || std::fs::metadata(&pack.path)?.len() != pack.bytes
            || noland_cas::blake3_file(&pack.path)? != pack.checksum)
    {
        return Err(StateError::Integrity(
            "captured pack changed before retry".into(),
        ));
    }
    Ok(())
}

pub(super) async fn pack_and_upload(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &mut PreparedBackup,
    resumed: bool,
) -> Result<()> {
    let Some(packing) = prepared.packing.take() else {
        return release_prepared_lease(agent, operation, prepared);
    };
    let started = Instant::now();
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    if packing.lease.parent() != Some(cas.root().join(".leases").as_path())
        || std::fs::symlink_metadata(&packing.lease).is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(StateError::UnsafePath("capture lease outside CAS".into()));
    }
    let directory = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let mut pending = VecDeque::new();
    let mut existing = VecDeque::new();
    let mut pending_bytes = 0u64;
    for (index, plan) in packing.plans.iter().enumerate() {
        if let Some(pack) = read_receipt(
            &receipt_path(agent, operation, index),
            master,
            &receipt_aad(operation, index),
        )? {
            validate_receipt(agent, operation, &directory, plan, &pack)?;
            existing.push_back(pack);
        } else {
            for (hash, path) in &plan.chunks {
                let canonical = cas.path_for_hash(hash)?;
                if path
                    != &packing
                        .lease
                        .join(canonical.file_name().expect("chunk filename"))
                    || std::fs::symlink_metadata(path)?.file_type().is_symlink()
                {
                    return Err(StateError::UnsafePath("invalid capture chunk path".into()));
                }
                if resumed && noland_cas::blake3_file(path)? != *hash {
                    return Err(StateError::Integrity(
                        "captured chunk changed before retry".into(),
                    ));
                }
                pending_bytes = pending_bytes
                    .saturating_add(std::fs::metadata(path)?.len())
                    .saturating_add(52);
            }
            pending_bytes = pending_bytes.saturating_add(24);
            pending.push_back((index, plan.clone(), receipt_path(agent, operation, index)));
        }
    }
    let _budget = disk::DiskReservation::reserve(&[(directory.clone(), pending_bytes)])?;
    let count = packing.workers.clamp(1, 8);
    let (sender, receiver) = tokio::sync::mpsc::channel(count);
    let cancelled = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(Mutex::new(pending));
    let mut workers = Workers {
        receiver,
        cancelled,
        threads: Vec::new(),
    };
    for _ in 0..count {
        let pending = Arc::clone(&pending);
        let stopped = Arc::clone(&workers.cancelled);
        let sender = sender.clone();
        let master = master.clone();
        let directory = directory.clone();
        let operations = agent.operations.clone();
        workers.threads.push(std::thread::spawn(move || loop {
            if stopped.load(Ordering::Acquire) || operations.cancel_requested(operation) {
                break;
            }
            let Some((index, plan, receipt)) =
                pending.lock().expect("pack queue poisoned").pop_front()
            else {
                break;
            };
            let result = (|| {
                let mut packs = pack_chunk_files_with_limits(
                    &directory,
                    &master,
                    plan.chunks,
                    |_| false,
                    plan.target,
                    plan.max,
                )?;
                if packs.len() != 1 {
                    return Err(StateError::Integrity(
                        "captured plan must seal exactly one pack".into(),
                    ));
                }
                let pack = packs.pop().expect("one pack");
                write_receipt(&receipt, &master, &receipt_aad(operation, index), &pack)?;
                Ok(pack)
            })();
            let failed = result.is_err();
            if sender.blocking_send(result).is_err() || failed {
                break;
            }
        }));
    }
    drop(sender);
    let mut progress = OperationProgress::new("packing_and_uploading", 0);
    progress.detail_json =
        serde_json::json!({"total_pack_plans": packing.plans.len(), "prepared_packs": 0});
    agent
        .db
        .set_operation_progress(operation, Some(&progress))?;
    loop {
        let next = match existing.pop_front() {
            Some(pack) => Some(Ok(pack)),
            None => workers.receiver.recv().await,
        };
        let Some(first) = next else {
            break;
        };
        let mut batch = vec![first?];
        while batch.len() < count {
            if let Some(pack) = existing.pop_front() {
                batch.push(pack);
            } else if let Ok(result) = workers.receiver.try_recv() {
                batch.push(result?);
            } else {
                break;
            }
        }
        if agent.operations.cancel_requested(operation) {
            return Err(cancellation_error());
        }
        let files = batch
            .iter()
            .map(|pack| (pack.pack_id.clone(), pack.path.clone()))
            .collect::<Vec<_>>();
        let upload_started = Instant::now();
        noland_storage::upload_packs_for_operation(
            provider,
            &files,
            Some(&agent.db),
            Some(operation),
        )
        .await?;
        prepared.metrics.upload_duration_ms = prepared
            .metrics
            .upload_duration_ms
            .saturating_add(elapsed_ms(upload_started));
        for pack in batch {
            prepared.incremental = prepared.incremental.saturating_add(pack.bytes);
            prepared.metrics.bytes_packed =
                prepared.metrics.bytes_packed.saturating_add(pack.bytes);
            prepared
                .checksums
                .insert(pack.pack_id.clone(), pack.checksum);
            prepared.pack_files.push((pack.pack_id, pack.path));
            prepared.new_pack_entries.extend(pack.entries);
        }
        if let Some(mut progress) = agent.db.get_operation_progress(operation)? {
            progress.detail_json["prepared_packs"] = serde_json::json!(prepared.pack_files.len());
            progress.detail_json["packing_upload_elapsed_ms"] =
                serde_json::json!(elapsed_ms(started));
            agent
                .db
                .set_operation_progress(operation, Some(&progress))?;
        }
    }
    drop(workers);
    if prepared.pack_files.len() != packing.plans.len() {
        return Err(cancellation_error());
    }
    prepared.metrics.packing_duration_ms = prepared
        .metrics
        .packing_duration_ms
        .saturating_add(elapsed_ms(started));
    prepared
        .pack_index
        .extend(prepared.new_pack_entries.iter().cloned());
    prepared
        .pack_index
        .sort_by(|left, right| left.chunk_hash.cmp(&right.chunk_hash));
    prepared
        .pack_index
        .dedup_by(|left, right| left.chunk_hash == right.chunk_hash);
    // After this durable replacement, retry needs only packs, not plaintext chunks.
    prepared.release_lease = Some(packing.lease);
    write_prepared_backup(agent, master, operation, prepared)?;
    release_prepared_lease(agent, operation, prepared)
}

fn release_prepared_lease(
    agent: &StateAgent,
    operation: Uuid,
    prepared: &PreparedBackup,
) -> Result<()> {
    let Some(lease) = &prepared.release_lease else {
        return Ok(());
    };
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    cas.release_lease(lease)?;
    let max = std::env::var("NOLAND_CAS_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1024 * 1024 * 1024);
    cas.evict_to(max)?;
    let _ = std::fs::remove_dir_all(prepared_backup_path(agent, operation).with_extension("packs"));
    Ok(())
}
