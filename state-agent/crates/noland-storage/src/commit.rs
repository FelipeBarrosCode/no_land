use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use noland_crypto::{derive_keys, MasterKey};
use noland_state_core::*;
use uuid::Uuid;

use crate::{
    metadata::MAX_METADATA_ENVELOPE_BYTES, open_metadata, seal_metadata, seal_metadata_json,
    ImmutableUpload, ImmutableUploadCompletion, ImmutableUploadObserver, MetadataBatch,
    MetadataWrite, RemoteKey, RemoteKnownSet, SharedStorageProvider,
};

#[derive(Default)]
struct UploadProgressState {
    in_flight_bytes: HashMap<Uuid, u64>,
    completed_in_transfer: HashMap<Uuid, u64>,
    last_flush: Option<std::time::Instant>,
}

struct JournalUploadObserver<'a> {
    db: &'a noland_state_db::StateDb,
    operation_id: Uuid,
    state: Arc<Mutex<UploadProgressState>>,
    transfer_id: Uuid,
    keys: Vec<String>,
}

impl JournalUploadObserver<'_> {
    fn persist_progress(&self, state: &mut UploadProgressState, force: bool) -> Result<()> {
        if !force
            && state
                .last_flush
                .is_some_and(|last| last.elapsed() < std::time::Duration::from_millis(500))
        {
            return Ok(());
        }
        let summary = self
            .db
            .sync_journal_summary_for_kind(self.operation_id, ContentObjectKind::Pack)?;
        if let Some(mut progress) = self.db.get_operation_progress(self.operation_id)? {
            let in_flight_bytes = state
                .in_flight_bytes
                .values()
                .fold(0u64, |sum, bytes| sum.saturating_add(*bytes));
            let completed_bytes = summary.completed_bytes.saturating_add(in_flight_bytes);
            let transferred_bytes = summary.bytes_transferred.saturating_add(in_flight_bytes);
            progress.completed_units = completed_bytes.min(summary.total_bytes);
            progress.total_units = Some(summary.total_bytes);
            progress.unit = Some("bytes".into());
            progress.detail_json["completed_packs"] = serde_json::json!(summary.completed_items);
            progress.detail_json["total_packs"] = serde_json::json!(summary.total_items);
            progress.detail_json["completed_pack_bytes"] =
                serde_json::json!(completed_bytes.min(summary.total_bytes));
            progress.detail_json["bytes_transferred"] = serde_json::json!(transferred_bytes);
            progress.detail_json["total_transfer_bytes"] = serde_json::json!(summary.total_bytes);
            progress.updated_at = chrono::Utc::now();
            self.db
                .set_operation_progress(self.operation_id, Some(&progress))?;
        }
        state.last_flush = Some(std::time::Instant::now());
        Ok(())
    }
}

impl ImmutableUploadObserver for JournalUploadObserver<'_> {
    fn transfer_started(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StateError::Storage("upload progress lock was poisoned".into()))?;
        state.in_flight_bytes.insert(self.transfer_id, 0);
        state.completed_in_transfer.insert(self.transfer_id, 0);
        Ok(())
    }

    fn transfer_progress(&self, bytes_transferred: u64) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StateError::Storage("upload progress lock was poisoned".into()))?;
        let bytes_transferred = bytes_transferred.saturating_sub(
            state
                .completed_in_transfer
                .get(&self.transfer_id)
                .copied()
                .unwrap_or(0),
        );
        let in_flight = state.in_flight_bytes.entry(self.transfer_id).or_default();
        *in_flight = (*in_flight).max(bytes_transferred);
        self.persist_progress(&mut state, false)
    }

    fn transfer_diagnostics(&self, diagnostics: &crate::UploadTransferDiagnostics) -> Result<()> {
        let mut detail = serde_json::to_value(diagnostics)?;
        detail["group_id"] = serde_json::json!(self.transfer_id);
        detail["group_objects"] = serde_json::json!(self.keys.len());
        // Group timings/traffic are shared measurements, not additive per-pack bytes.
        self.db.update_upload_diagnostics(
            self.operation_id,
            &self.keys,
            diagnostics.attempt,
            &detail,
        )?;
        self.transfer_progress(diagnostics.attempt_bytes)
    }

    fn completed(
        &self,
        completed: &[ImmutableUpload],
        completion: ImmutableUploadCompletion,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StateError::Storage("upload progress lock was poisoned".into()))?;
        let entries = completed
            .iter()
            .map(|upload| {
                let size = std::fs::metadata(&upload.local)?.len();
                Ok((
                    upload.key.as_str().to_owned(),
                    size,
                    if completion == ImmutableUploadCompletion::Uploaded {
                        size
                    } else {
                        0
                    },
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        self.db
            .complete_sync_journal_items(self.operation_id, &entries)?;
        let bytes = entries.iter().map(|(_, _, bytes)| *bytes).sum::<u64>();
        *state
            .completed_in_transfer
            .entry(self.transfer_id)
            .or_default() += bytes;
        if let Some(inflight) = state.in_flight_bytes.get_mut(&self.transfer_id) {
            *inflight = inflight.saturating_sub(bytes);
        }
        self.persist_progress(&mut state, false)
    }
}

impl Drop for JournalUploadObserver<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight_bytes.remove(&self.transfer_id);
            state.completed_in_transfer.remove(&self.transfer_id);
            let _ = self.persist_progress(&mut state, true);
        }
    }
}

/// The inventory and live progress belong to one operation, not to a provider
/// instance or one compression wave. Initialization failures propagate; they are
/// never cached as an empty remote. A retry creates a fresh inventory.
pub struct PackUploadSession<'a> {
    provider: &'a dyn SharedStorageProvider,
    db: Option<&'a noland_state_db::StateDb>,
    operation_id: Option<Uuid>,
    known: tokio::sync::OnceCell<RemoteKnownSet>,
    root_ready: tokio::sync::OnceCell<()>,
    progress: Arc<Mutex<UploadProgressState>>,
}

impl<'a> PackUploadSession<'a> {
    pub fn new(
        provider: &'a dyn SharedStorageProvider,
        db: Option<&'a noland_state_db::StateDb>,
        operation_id: Option<Uuid>,
    ) -> Self {
        Self {
            provider,
            db,
            operation_id,
            known: tokio::sync::OnceCell::new(),
            root_ready: tokio::sync::OnceCell::new(),
            progress: Arc::default(),
        }
    }

    pub async fn upload(&self, pack_files: &[(String, std::path::PathBuf)]) -> Result<()> {
        upload_packs_in_session(self, pack_files, None).await
    }

    pub async fn upload_reserved(
        &self,
        pack_files: &[(String, std::path::PathBuf)],
        reservation: crate::UploadReservation,
    ) -> Result<()> {
        upload_packs_in_session(self, pack_files, Some(reservation)).await
    }
}

struct UploadObserver<'a> {
    journal: Option<JournalUploadObserver<'a>>,
    reservation: Option<crate::UploadReservation>,
    completed: Mutex<std::collections::HashSet<RemoteKey>>,
}
impl ImmutableUploadObserver for UploadObserver<'_> {
    fn transfer_started(&self) -> Result<()> {
        self.journal
            .as_ref()
            .map_or(Ok(()), |j| j.transfer_started())
    }
    fn transfer_progress(&self, bytes: u64) -> Result<()> {
        self.journal
            .as_ref()
            .map_or(Ok(()), |j| j.transfer_progress(bytes))
    }
    fn transfer_diagnostics(&self, diagnostics: &crate::UploadTransferDiagnostics) -> Result<()> {
        self.journal
            .as_ref()
            .map_or(Ok(()), |j| j.transfer_diagnostics(diagnostics))
    }
    fn completed(
        &self,
        uploads: &[ImmutableUpload],
        completion: ImmutableUploadCompletion,
    ) -> Result<()> {
        let mut seen = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = uploads
            .iter()
            .filter(|upload| !seen.contains(&upload.key))
            .cloned()
            .collect::<Vec<_>>();
        if fresh.is_empty() {
            return Ok(());
        }
        if let Some(journal) = &self.journal {
            journal.completed(&fresh, completion)?;
        }
        if let Some(reservation) = &self.reservation {
            let bytes = fresh
                .iter()
                .map(|upload| std::fs::metadata(&upload.local).map(|m| m.len()))
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .sum();
            reservation.completed(fresh.len(), bytes);
        }
        seen.extend(fresh.into_iter().map(|upload| upload.key));
        Ok(())
    }
}

pub struct CatalogStore {
    pub document: CatalogDocument,
}

pub async fn load_catalog(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<CatalogDocument> {
    let key = RemoteKey::new(catalog_latest_key());
    let latest = provider.stat(&key).await?;
    let mut pointed_commit = None;
    if let Some(meta) = &latest {
        if meta.size <= 1024 {
            match download_metadata(provider, &key, 1024).await {
                Ok(bytes) => {
                    pointed_commit = std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(|pointer| canonical_commit_id(pointer.trim()));
                }
                // A missing/raced or malformed pointer is recoverable. Transport,
                // permission and authentication failures must reach the caller.
                Err(StateError::NotFound(_)) => {}
                Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let mut catalog = if let Some(id) = pointed_commit {
        let marker = RemoteKey::new(format!("catalog/commits/{id}.{COMMITTED_MARKER}"));
        if provider.stat(&marker).await?.is_some() {
            // Do not hide a wrong key/corrupt committed object by falling back to
            // an older catalog (or, worse, silently creating an empty repository).
            fetch_catalog_commit(provider, master, id).await?
        } else {
            recover_latest_catalog(provider, master)
                .await?
                .ok_or(StateError::IncompleteCommit)?
        }
    } else {
        match recover_latest_catalog(provider, master).await? {
            Some(catalog) => catalog,
            None if latest.is_none() => CatalogDocument::empty(),
            None => return Err(StateError::IncompleteCommit),
        }
    };
    // Catalog schema v1 did not persist mode-specific heads. Reconstruct them
    // from bundle history whenever an old catalog is read.
    crate::deletion::filter_deleted(provider, master, &mut catalog).await?;
    catalog.refresh_bundle_heads();
    Ok(catalog)
}

async fn fetch_catalog_commit(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    commit_id: Uuid,
) -> Result<CatalogDocument> {
    let key = RemoteKey::new(format!("catalog/commits/{commit_id}.enc"));
    let bytes = download_metadata(provider, &key, MAX_METADATA_ENVELOPE_BYTES).await?;
    let keys = derive_keys(master);
    let catalog: CatalogDocument = open_metadata(&keys.catalog, b"catalog", bytes)?;
    if catalog.catalog_commit_id != commit_id {
        return Err(StateError::Integrity("catalog commit ID mismatch".into()));
    }
    Ok(catalog)
}

async fn recover_latest_catalog(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<Option<CatalogDocument>> {
    let entries = provider
        .list_prefix(&RemoteKey::new("catalog/commits"))
        .await?;
    let mut commits = Vec::new();
    for entry in entries {
        if entry.is_prefix {
            continue;
        }
        let Some(name) = entry.key.as_str().strip_prefix("catalog/commits/") else {
            continue;
        };
        let Some(stem) = name.strip_suffix(&format!(".{COMMITTED_MARKER}")) else {
            continue;
        };
        if let Some(id) = canonical_commit_id(stem) {
            commits.push(id);
        }
    }
    commits.sort_unstable();
    commits.dedup();
    let mut best: Option<CatalogDocument> = None;
    // Deliberately one download/decode at a time: retain at most the current best
    // and one bounded candidate, rather than all history or unbounded tasks.
    for id in commits {
        let doc = fetch_catalog_commit(provider, master, id).await?;
        if best.as_ref().is_none_or(|b| {
            (doc.created_at, doc.catalog_commit_id) > (b.created_at, b.catalog_commit_id)
        }) {
            best = Some(doc);
        }
    }
    Ok(best)
}

fn canonical_commit_id(text: &str) -> Option<Uuid> {
    Uuid::parse_str(text)
        .ok()
        .filter(|id| id.to_string() == text)
}

struct MetadataTemp(std::path::PathBuf);
impl Drop for MetadataTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) async fn download_metadata(
    provider: &dyn SharedStorageProvider,
    key: &RemoteKey,
    limit: u64,
) -> Result<Vec<u8>> {
    use std::io::Read;
    let tmp =
        MetadataTemp(std::env::temp_dir().join(format!("noland-metadata-{}", Uuid::new_v4())));
    provider.download(key, &tmp.0).await?;
    let file = std::fs::File::open(&tmp.0)?;
    if file.metadata()?.len() > limit {
        return Err(StateError::Integrity(
            "metadata envelope exceeds size limit".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(StateError::Integrity(
            "metadata envelope exceeds size limit".into(),
        ));
    }
    Ok(bytes)
}

pub async fn commit_bundle(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    db: Option<&noland_state_db::StateDb>,
) -> Result<()> {
    commit_bundle_with_index(provider, master, manifest, pack_files, None, db).await
}

pub async fn commit_bundle_with_index(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    pack_index_json: Option<&[u8]>,
    db: Option<&noland_state_db::StateDb>,
) -> Result<()> {
    commit_bundle_with_index_for_operation(
        provider,
        master,
        manifest,
        pack_files,
        pack_index_json,
        db,
        None,
    )
    .await
}

pub async fn upload_packs_for_operation(
    provider: &dyn SharedStorageProvider,
    pack_files: &[(String, std::path::PathBuf)],
    db: Option<&noland_state_db::StateDb>,
    operation_id: Option<Uuid>,
) -> Result<()> {
    provider.ensure_root().await?;
    PackUploadSession::new(provider, db, operation_id)
        .upload(pack_files)
        .await
}

async fn upload_packs_in_session(
    session: &PackUploadSession<'_>,
    pack_files: &[(String, std::path::PathBuf)],
    reservation: Option<crate::UploadReservation>,
) -> Result<()> {
    let PackUploadSession {
        provider,
        db,
        operation_id,
        ..
    } = session;
    let (db, operation_id) = (*db, *operation_id);
    let all_pack_uploads: Vec<_> = pack_files
        .iter()
        .map(|(pack_id, path)| {
            ImmutableUpload::new(path.clone(), RemoteKey::new(pack_key(pack_id)))
        })
        .collect();
    let mut pack_uploads = Vec::new();
    for upload in &all_pack_uploads {
        let completed = match (db, operation_id) {
            (Some(db), Some(operation_id)) => {
                db.sync_journal_completed(operation_id, upload.key.as_str())?
            }
            _ => false,
        };
        if completed {
            continue;
        }
        if let (Some(db), Some(operation_id)) = (db, operation_id) {
            db.start_sync_journal_item(
                operation_id,
                upload.key.as_str(),
                ContentObjectKind::Pack,
                SyncDirection::Upload,
                Some(&upload.local.to_string_lossy()),
                Some(upload.key.as_str()),
                Some(std::fs::metadata(&upload.local)?.len()),
            )?;
        }
        pack_uploads.push(upload.clone());
    }
    if pack_uploads.is_empty() {
        return Ok(()); // Completed receipts may legitimately have no local pack.
    }
    let upload_observer = match (db, operation_id) {
        (Some(db), Some(operation_id)) => Some(JournalUploadObserver {
            db,
            operation_id,
            state: Arc::clone(&session.progress),
            transfer_id: Uuid::new_v4(),
            keys: pack_uploads
                .iter()
                .map(|upload| upload.key.as_str().to_owned())
                .collect(),
        }),
        _ => None,
    };
    let observer = UploadObserver {
        journal: upload_observer,
        reservation,
        completed: Mutex::default(),
    };
    let upload_result = async {
        session
            .root_ready
            .get_or_try_init(|| provider.ensure_root())
            .await?;
        let known = if provider.supports_targeted_uploads() {
            None
        } else {
            Some(
                session
                    .known
                    .get_or_try_init(|| async {
                        provider.ensure_root().await?;
                        crate::list_remote_known(*provider, &RemoteKey::new("packs")).await
                    })
                    .await?,
            )
        };
        let result = match known {
            Some(known) => {
                provider
                    .upload_immutable_bulk_known_observed(&pack_uploads, known, Some(&observer))
                    .await?
            }
            None => {
                provider
                    .upload_immutable_bulk_observed(&pack_uploads, Some(&observer))
                    .await?
            }
        };
        let mut returned = HashMap::new();
        for meta in &result {
            if returned.insert(meta.key.clone(), meta.size).is_some() {
                return Err(StateError::Integrity(
                    "duplicate immutable upload result".into(),
                ));
            }
        }
        if returned.len() != pack_uploads.len()
            || pack_uploads.iter().any(|upload| {
                std::fs::metadata(&upload.local)
                    .ok()
                    .is_none_or(|meta| returned.get(&upload.key) != Some(&meta.len()))
            })
        {
            return Err(StateError::Integrity(
                "incomplete immutable upload results".into(),
            ));
        }
        if let Some(known) = known {
            known.remember(&result)?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = upload_result {
        if let (Some(db), Some(operation_id)) = (db, operation_id) {
            for upload in &pack_uploads {
                if !db.sync_journal_completed(operation_id, upload.key.as_str())? {
                    let _ = db.fail_sync_journal_item(
                        operation_id,
                        upload.key.as_str(),
                        &error.to_string(),
                    );
                }
            }
        }
        return Err(error);
    }
    Ok(())
}

pub async fn commit_bundle_with_index_for_operation(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    pack_index_json: Option<&[u8]>,
    db: Option<&noland_state_db::StateDb>,
    operation_id: Option<Uuid>,
) -> Result<()> {
    commit_bundle_metadata(
        provider,
        master,
        manifest,
        pack_files,
        pack_index_json.map(PackIndexInput::Json),
        db,
        operation_id,
    )
    .await
}

/// Typed-index path: serialize directly into compressed/encrypted metadata,
/// avoiding the caller's full pack-index JSON allocation.
pub async fn commit_bundle_with_pack_index_for_operation(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    pack_index: Option<&[noland_pack::PackIndexEntry]>,
    db: Option<&noland_state_db::StateDb>,
    operation_id: Option<Uuid>,
) -> Result<()> {
    commit_bundle_metadata(
        provider,
        master,
        manifest,
        pack_files,
        pack_index.map(PackIndexInput::Entries),
        db,
        operation_id,
    )
    .await
}

enum PackIndexInput<'a> {
    Json(&'a [u8]),
    Entries(&'a [noland_pack::PackIndexEntry]),
}

async fn commit_bundle_metadata(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    pack_index: Option<PackIndexInput<'_>>,
    db: Option<&noland_state_db::StateDb>,
    operation_id: Option<Uuid>,
) -> Result<()> {
    if crate::bundle_is_deleted(provider, master, manifest.bundle_id).await? {
        return Err(StateError::Conflict(
            "cannot republish an explicitly deleted backup".into(),
        ));
    }
    let retired = crate::retired_pack_ids(provider, master).await?;
    let references = match &pack_index {
        Some(PackIndexInput::Entries(entries)) => entries
            .iter()
            .map(|e| e.pack_id.clone())
            .collect::<Vec<_>>(),
        Some(PackIndexInput::Json(bytes)) => {
            serde_json::from_slice::<Vec<noland_pack::PackIndexEntry>>(bytes)?
                .into_iter()
                .map(|e| e.pack_id)
                .collect()
        }
        None => Vec::new(),
    };
    if references.iter().any(|id| retired.contains(id)) {
        return Err(StateError::Conflict("captured backup references explicitly deleted data; its frozen revision cannot be republished".into()));
    }
    upload_packs_for_operation(provider, pack_files, db, operation_id).await?;
    let keys = derive_keys(master);
    let bundle_prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);
    if let Some(db) = db {
        for (pack_id, _) in pack_files {
            db.journal_put(
                &manifest.commit_id.to_string(),
                &pack_key(pack_id),
                "upload",
                "ok",
                None,
            )?;
        }
    }

    let mut metadata = Vec::new();
    if let Some(index) = pack_index {
        let sealed = match index {
            PackIndexInput::Json(json) => seal_metadata_json(&keys.manifest, b"pack-index", json)?,
            PackIndexInput::Entries(entries) => {
                seal_metadata(&keys.manifest, b"pack-index", entries)?
            }
        };
        metadata.push(MetadataWrite::new(
            RemoteKey::new(format!("{bundle_prefix}/index.enc")),
            Bytes::from(sealed.envelope),
        ));
    }

    let sealed = seal_metadata(&keys.manifest, b"manifest", manifest)?;
    let manifest_hash = sealed.json_hash;
    metadata.push(MetadataWrite::new(
        RemoteKey::new(format!("{bundle_prefix}/manifest.enc")),
        Bytes::from(sealed.envelope),
    ));
    metadata.push(MetadataWrite::new(
        RemoteKey::new(format!("{bundle_prefix}/manifest.hash")),
        Bytes::from(manifest_hash.clone()),
    ));
    // COMMITTED is the final batch phase. Incomplete bundles stay invisible.
    provider
        .put_metadata_batch(&MetadataBatch::with_committed(
            metadata,
            MetadataWrite::new(
                RemoteKey::new(format!("{bundle_prefix}/{COMMITTED_MARKER}")),
                Bytes::from(manifest.commit_id.to_string()),
            ),
        ))
        .await?;

    if let Some(db) = db {
        db.record_commit(
            manifest.commit_id,
            &manifest.app.app_id,
            manifest.bundle_id,
            &manifest_hash,
            Some(&bundle_prefix),
            CommitVisibility::Committed,
        )?;
    }
    Ok(())
}

pub async fn update_catalog_with_bundle(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    stored_incremental_size: u64,
) -> Result<CatalogDocument> {
    let committed = provider
        .stat(&RemoteKey::new(format!(
            "{}/{COMMITTED_MARKER}",
            bundle_dir(&manifest.app.app_id, manifest.bundle_id)
        )))
        .await?;
    if committed.is_none() {
        return Err(StateError::IncompleteCommit);
    }

    let mut catalog = load_catalog(provider, master).await?;
    catalog.catalog_commit_id = Uuid::new_v4();
    catalog.created_at = chrono::Utc::now();
    catalog.upsert_bundle_from_manifest(
        &manifest.app,
        CatalogBundle {
            bundle_id: manifest.bundle_id,
            commit_id: manifest.commit_id,
            parent_bundle_id: manifest.parent_bundle_id,
            captured_at: manifest.source.captured_at,
            source_instance_id: manifest.source.instance_id,
            mode: manifest.mode,
            logical_size: manifest.logical_size(),
            stored_incremental_size,
        },
    );
    write_catalog(provider, master, &catalog).await?;
    Ok(catalog)
}

pub async fn write_catalog(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    catalog: &CatalogDocument,
) -> Result<()> {
    let commit_key = RemoteKey::new(catalog_commit_key(catalog.catalog_commit_id));
    let marker_key = RemoteKey::new(format!(
        "catalog/commits/{}.{}",
        catalog.catalog_commit_id, COMMITTED_MARKER
    ));
    if provider.stat(&marker_key).await?.is_some() {
        // A pointer-publication retry must not rewrite committed ciphertext or
        // let a different document reuse the same immutable history ID.
        let existing = fetch_catalog_commit(provider, master, catalog.catalog_commit_id).await?;
        if crate::compact_json_hash(&existing)? != crate::compact_json_hash(catalog)? {
            return Err(StateError::Conflict(
                "committed catalog differs from publication retry".into(),
            ));
        }
    } else {
        let keys = derive_keys(master);
        let sealed = seal_metadata(&keys.catalog, b"catalog", catalog)?;
        provider
            .put_metadata_batch(&MetadataBatch::with_committed(
                vec![MetadataWrite::new(commit_key, Bytes::from(sealed.envelope))],
                MetadataWrite::new(
                    marker_key,
                    Bytes::from(COMMITTED_MARKER.as_bytes().to_vec()),
                ),
            ))
            .await?;
    }
    // A stale but valid LATEST is trusted by normal reads, so publication must
    // succeed before reporting success. Keep committed history on failure and
    // allow the caller to retry with the same document/ID. Avoid backend error
    // text here: it may contain authentication material.
    provider
        .put_small_versioned(
            Bytes::from(catalog.catalog_commit_id.to_string()),
            &RemoteKey::new(catalog_latest_key()),
        )
        .await
        .map_err(|_| {
            StateError::Storage(
                "catalog committed but LATEST publication failed; retry the same catalog document"
                    .into(),
            )
        })?;
    Ok(())
}

pub async fn read_pack_index(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    app_id: &AppId,
    bundle_id: Uuid,
) -> Result<Vec<noland_pack::PackIndexEntry>> {
    read_pack_index_for_operation(provider, master, app_id, bundle_id, None, None).await
}

pub async fn read_pack_index_for_operation(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    app_id: &AppId,
    bundle_id: Uuid,
    db: Option<&noland_state_db::StateDb>,
    operation_id: Option<Uuid>,
) -> Result<Vec<noland_pack::PackIndexEntry>> {
    let prefix = bundle_dir(app_id, bundle_id);
    let remote_key = format!("{prefix}/index.enc");
    if let (Some(db), Some(operation_id)) = (db, operation_id) {
        db.start_sync_journal_item(
            operation_id,
            &remote_key,
            ContentObjectKind::Other,
            SyncDirection::Download,
            None,
            Some(&remote_key),
            None,
        )?;
    }
    let bytes = match download_metadata(
        provider,
        &RemoteKey::new(remote_key.clone()),
        MAX_METADATA_ENVELOPE_BYTES,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            if let (Some(db), Some(operation_id)) = (db, operation_id) {
                let _ = db.fail_sync_journal_item(operation_id, &remote_key, &error.to_string());
            }
            return Err(error);
        }
    };
    let size = bytes.len() as u64;
    let keys = derive_keys(master);
    let decoded = open_metadata(&keys.manifest, b"pack-index", bytes);
    match decoded {
        Ok(index) => {
            if let (Some(db), Some(operation_id)) = (db, operation_id) {
                db.complete_sync_journal_item(operation_id, &remote_key, size)?;
            }
            Ok(index)
        }
        Err(error) => {
            if let (Some(db), Some(operation_id)) = (db, operation_id) {
                let _ = db.fail_sync_journal_item(operation_id, &remote_key, &error.to_string());
            }
            Err(error)
        }
    }
}

pub async fn read_committed_manifest(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    app_id: &AppId,
    bundle_id: Uuid,
) -> Result<BundleManifest> {
    if crate::deletion::read_deletion(provider, master, bundle_id)
        .await?
        .is_some()
    {
        return Err(StateError::NotFound("backup was explicitly deleted".into()));
    }
    let prefix = bundle_dir(app_id, bundle_id);
    if provider
        .stat(&RemoteKey::new(format!("{prefix}/{COMMITTED_MARKER}")))
        .await?
        .is_none()
    {
        return Err(StateError::IncompleteCommit);
    }
    let bytes = download_metadata(
        provider,
        &RemoteKey::new(format!("{prefix}/manifest.enc")),
        MAX_METADATA_ENVELOPE_BYTES,
    )
    .await?;
    let keys = derive_keys(master);
    let manifest: BundleManifest = open_metadata(&keys.manifest, b"manifest", bytes)?;
    if manifest.app.app_id != *app_id || manifest.bundle_id != bundle_id {
        return Err(StateError::Integrity("manifest identity mismatch".into()));
    }
    Ok(manifest)
}

pub async fn commit_checkpoint(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    checkpoint: &LearnedStateCheckpoint,
) -> Result<()> {
    let keys = derive_keys(master);
    let dir = checkpoint_dir(checkpoint.instance_id, checkpoint.checkpoint_id);
    let sealed = seal_metadata(&keys.catalog, b"checkpoint", checkpoint)?;
    provider
        .put_metadata_batch(&MetadataBatch::with_committed(
            vec![
                MetadataWrite::new(
                    RemoteKey::new(format!("{dir}/state.enc")),
                    Bytes::from(sealed.envelope),
                ),
                MetadataWrite::new(
                    RemoteKey::new(format!("{dir}/checkpoint.enc")),
                    Bytes::from(enc_meta(checkpoint)),
                ),
            ],
            MetadataWrite::new(
                RemoteKey::new(format!("{dir}/{COMMITTED_MARKER}")),
                Bytes::from(checkpoint.checkpoint_id.to_string()),
            ),
        ))
        .await?;
    Ok(())
}

pub async fn commit_seal(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    seal: &SealRecord,
) -> Result<()> {
    let keys = derive_keys(master);
    let dir = seal_dir(seal.instance_id, seal.seal_id);
    let sealed = seal_metadata(&keys.catalog, b"seal", seal)?;
    provider
        .put_metadata_batch(&MetadataBatch::with_committed(
            vec![MetadataWrite::new(
                RemoteKey::new(format!("{dir}/seal.enc")),
                Bytes::from(sealed.envelope),
            )],
            MetadataWrite::new(
                RemoteKey::new(format!("{dir}/{COMMITTED_MARKER}")),
                Bytes::from(seal.seal_id.to_string()),
            ),
        ))
        .await?;
    Ok(())
}

fn enc_meta(checkpoint: &LearnedStateCheckpoint) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "checkpoint_id": checkpoint.checkpoint_id,
        "instance_id": checkpoint.instance_id,
        "created_at": checkpoint.created_at,
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Health, LocalStorage, RemoteEntry, RemoteMeta};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct InventoryProvider {
        inner: LocalStorage,
        lists: AtomicUsize,
        catalog_lists: AtomicUsize,
        uploads: AtomicUsize,
        fail_listing: AtomicBool,
        fail_upload: AtomicUsize,
        fail_download: AtomicBool,
        fail_latest: AtomicBool,
        metadata: Mutex<Vec<RemoteKey>>,
    }

    impl InventoryProvider {
        fn new(root: &Path) -> Self {
            Self {
                inner: LocalStorage::new(root.join("remote")),
                lists: 0.into(),
                catalog_lists: 0.into(),
                uploads: 0.into(),
                fail_listing: false.into(),
                fail_upload: usize::MAX.into(),
                fail_download: false.into(),
                fail_latest: false.into(),
                metadata: Mutex::default(),
            }
        }
    }

    #[async_trait]
    impl SharedStorageProvider for InventoryProvider {
        async fn health_check(&self) -> Result<Health> {
            self.inner.health_check().await
        }
        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }
        async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            self.inner.stat(key).await
        }
        async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
            let call = self.uploads.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_upload.load(Ordering::SeqCst) {
                return Err(StateError::Storage("injected upload failure".into()));
            }
            self.inner.upload_immutable(local, key).await
        }
        async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
            if self.fail_download.load(Ordering::SeqCst) {
                return Err(StateError::Storage("download permission denied".into()));
            }
            self.inner.download(key, dest).await
        }
        async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            if prefix.as_str() == "catalog/commits" {
                self.catalog_lists.fetch_add(1, Ordering::SeqCst);
            }
            if self.fail_listing.load(Ordering::SeqCst) {
                return Err(StateError::Storage("listing permission denied".into()));
            }
            self.inner.list_prefix(prefix).await
        }
        async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
            self.metadata.lock().unwrap().push(key.clone());
            if key.as_str() == catalog_latest_key() && self.fail_latest.load(Ordering::SeqCst) {
                return Err(StateError::Storage("injected pointer failure".into()));
            }
            self.inner.put_small_versioned(bytes, key).await
        }
    }

    fn fixture() -> (std::path::PathBuf, noland_state_db::StateDb, Uuid) {
        let operation = Uuid::new_v4();
        let root = std::env::temp_dir().join(format!("noland-upload-session-{operation}"));
        std::fs::create_dir_all(&root).unwrap();
        let db = noland_state_db::StateDb::open_in_memory().unwrap();
        let now = chrono::Utc::now();
        db.upsert_operation(&OperationRecord {
            operation_id: operation,
            kind: "backup".into(),
            app_id: None,
            state: "UPLOADING".into(),
            created_at: now,
            updated_at: now,
            last_error: None,
            detail_json: serde_json::json!({}),
        })
        .unwrap();
        db.set_operation_progress(operation, Some(&OperationProgress::new("uploading", 0)))
            .unwrap();
        (root, db, operation)
    }

    fn pack(root: &Path, bytes: &[u8]) -> (String, std::path::PathBuf) {
        let id = Uuid::new_v4().to_string();
        let path = root.join(format!("{id}.pack"));
        std::fs::write(&path, bytes).unwrap();
        (id, path)
    }

    #[tokio::test]
    async fn catalog_pointer_failure_preserves_history_and_same_document_retry_only_publishes_latest(
    ) {
        let (root, _, _) = fixture();
        let provider = InventoryProvider::new(&root);
        let master = MasterKey::generate();
        provider.ensure_root().await.unwrap();
        let mut old = CatalogDocument::empty();
        old.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        write_catalog(&provider, &master, &old).await.unwrap();
        let old_path = provider
            .inner
            .root
            .join(catalog_commit_key(old.catalog_commit_id));
        let old_ciphertext = std::fs::read(&old_path).unwrap();
        let newest = CatalogDocument::empty();
        let commit_key = RemoteKey::new(catalog_commit_key(newest.catalog_commit_id));
        let marker_key = RemoteKey::new(format!(
            "catalog/commits/{}.{}",
            newest.catalog_commit_id, COMMITTED_MARKER
        ));
        let latest_key = RemoteKey::new(catalog_latest_key());
        let latest_path = provider.inner.root.join(latest_key.as_str());
        provider.fail_latest.store(true, Ordering::SeqCst);
        let attempt_start = provider.metadata.lock().unwrap().len();
        let error = write_catalog(&provider, &master, &newest)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "storage: catalog committed but LATEST publication failed; retry the same catalog document");
        assert_eq!(
            &provider.metadata.lock().unwrap()[attempt_start..],
            &[commit_key.clone(), marker_key.clone(), latest_key.clone()]
        );
        assert_eq!(
            std::fs::read_to_string(&latest_path).unwrap(),
            old.catalog_commit_id.to_string()
        );
        let lists = provider.catalog_lists.load(Ordering::SeqCst);
        assert_eq!(
            load_catalog(&provider, &master)
                .await
                .unwrap()
                .catalog_commit_id,
            old.catalog_commit_id
        );
        assert_eq!(
            provider.catalog_lists.load(Ordering::SeqCst),
            lists,
            "valid pointers do not scan history"
        );
        let committed_path = provider.inner.root.join(commit_key.as_str());
        let marker_path = provider.inner.root.join(marker_key.as_str());
        let committed_ciphertext = std::fs::read(&committed_path).unwrap();
        let marker = std::fs::read(&marker_path).unwrap();
        assert_eq!(marker, COMMITTED_MARKER.as_bytes());
        assert_eq!(std::fs::read(&old_path).unwrap(), old_ciphertext);

        // Synthetic pointer loss still recovers the immutable committed revision.
        std::fs::remove_file(&latest_path).unwrap();
        let writes = provider.metadata.lock().unwrap().len();
        assert_eq!(
            load_catalog(&provider, &master)
                .await
                .unwrap()
                .catalog_commit_id,
            newest.catalog_commit_id
        );
        assert!(!latest_path.exists());
        assert_eq!(provider.metadata.lock().unwrap().len(), writes);
        assert_eq!(
            std::fs::read(&committed_path).unwrap(),
            committed_ciphertext
        );
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);

        // Retry the same document/ID against a stale pointer, without resealing.
        std::fs::write(&latest_path, old.catalog_commit_id.to_string()).unwrap();
        provider.fail_latest.store(false, Ordering::SeqCst);
        write_catalog(&provider, &master, &newest).await.unwrap();
        assert_eq!(&provider.metadata.lock().unwrap()[writes..], &[latest_key]);
        assert_eq!(
            std::fs::read(&committed_path).unwrap(),
            committed_ciphertext
        );
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);
        assert_eq!(std::fs::read(&old_path).unwrap(), old_ciphertext);
        let lists = provider.catalog_lists.load(Ordering::SeqCst);
        assert_eq!(
            load_catalog(&provider, &master)
                .await
                .unwrap()
                .catalog_commit_id,
            newest.catalog_commit_id
        );
        assert_eq!(provider.catalog_lists.load(Ordering::SeqCst), lists);

        let mut conflicting = newest.clone();
        conflicting.created_at += chrono::Duration::seconds(1);
        let writes = provider.metadata.lock().unwrap().len();
        assert!(matches!(
            write_catalog(&provider, &master, &conflicting).await,
            Err(StateError::Conflict(_))
        ));
        assert_eq!(provider.metadata.lock().unwrap().len(), writes);
        assert_eq!(
            std::fs::read(&committed_path).unwrap(),
            committed_ciphertext
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn catalog_recovery_uses_only_committed_history_and_never_repairs_remote_state() {
        let (root, _, _) = fixture();
        let provider = InventoryProvider::new(&root);
        let master = MasterKey::generate();
        provider.ensure_root().await.unwrap();
        assert!(load_catalog(&provider, &master)
            .await
            .unwrap()
            .apps
            .is_empty());
        let mut committed = CatalogDocument::empty();
        committed.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        write_catalog(&provider, &master, &committed).await.unwrap();
        let metadata = provider.metadata.lock().unwrap().clone();
        assert!(metadata[metadata.len() - 2]
            .as_str()
            .ends_with(COMMITTED_MARKER));
        assert_eq!(metadata.last().unwrap().as_str(), catalog_latest_key());
        // A newer uploaded .enc without its marker must stay invisible.
        let orphan = CatalogDocument::empty();
        let keys = derive_keys(&master);
        let envelope = seal_metadata(&keys.catalog, b"catalog", &orphan)
            .unwrap()
            .envelope;
        provider
            .put_small_versioned(
                Bytes::from(envelope),
                &RemoteKey::new(catalog_commit_key(orphan.catalog_commit_id)),
            )
            .await
            .unwrap();
        let latest = provider.inner.root.join(catalog_latest_key());
        std::fs::remove_file(&latest).unwrap();
        let writes = provider.metadata.lock().unwrap().len();
        assert_eq!(
            load_catalog(&provider, &master)
                .await
                .unwrap()
                .catalog_commit_id,
            committed.catalog_commit_id
        );
        assert!(!latest.exists());
        assert_eq!(provider.metadata.lock().unwrap().len(), writes);
        for broken in [
            Vec::new(),
            b"../not-a-commit".to_vec(),
            vec![0xff],
            orphan.catalog_commit_id.to_string().into_bytes(),
        ] {
            std::fs::write(&latest, broken).unwrap();
            assert_eq!(
                load_catalog(&provider, &master)
                    .await
                    .unwrap()
                    .catalog_commit_id,
                committed.catalog_commit_id
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn catalog_recovery_propagates_listing_transport_authentication_and_corruption() {
        let (root, _, _) = fixture();
        let provider = InventoryProvider::new(&root);
        let master = MasterKey::generate();
        provider.ensure_root().await.unwrap();
        provider.fail_listing.store(true, Ordering::SeqCst);
        assert!(matches!(
            load_catalog(&provider, &master).await,
            Err(StateError::Storage(_))
        ));
        provider.fail_listing.store(false, Ordering::SeqCst);
        let catalog = CatalogDocument::empty();
        write_catalog(&provider, &master, &catalog).await.unwrap();
        provider.fail_download.store(true, Ordering::SeqCst);
        assert!(matches!(
            load_catalog(&provider, &master).await,
            Err(StateError::Storage(_))
        ));
        provider.fail_download.store(false, Ordering::SeqCst);
        assert!(load_catalog(&provider, &MasterKey::generate())
            .await
            .is_err());
        std::fs::remove_file(provider.inner.root.join(catalog_latest_key())).unwrap();
        assert!(load_catalog(&provider, &MasterKey::generate())
            .await
            .is_err());
        provider.fail_download.store(true, Ordering::SeqCst);
        assert!(matches!(
            load_catalog(&provider, &master).await,
            Err(StateError::Storage(_))
        ));
        provider.fail_download.store(false, Ordering::SeqCst);
        let path = provider
            .inner
            .root
            .join(catalog_commit_key(catalog.catalog_commit_id));
        let mut corrupt = std::fs::read(&path).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;
        std::fs::write(&path, corrupt).unwrap();
        assert!(load_catalog(&provider, &master).await.is_err());
        std::fs::remove_file(path).unwrap();
        assert!(load_catalog(&provider, &master).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn typed_metadata_commit_keeps_exact_hash_schema_and_marker_order() {
        let (root, _, _) = fixture();
        let provider = InventoryProvider::new(&root);
        let master = MasterKey::generate();
        let manifest = BundleManifest::new(
            ManifestApp::from(&AppIdentity::new(
                AppId::desktop("codec-test"),
                "Codec Test",
            )),
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "test".into(),
                captured_at: chrono::Utc::now(),
            },
            BackupMode::PersonalState,
        );
        let index = vec![noland_pack::PackIndexEntry {
            chunk_hash: "test-hash".repeat(1000),
            pack_id: Uuid::new_v4().to_string(),
            offset: 24,
            ciphertext_len: 19,
            plaintext_len: 3,
            compression: noland_pack::PackCompression::None,
            nonce: vec![0; 24],
        }];
        commit_bundle_with_pack_index_for_operation(
            &provider,
            &master,
            &manifest,
            &[],
            Some(&index),
            None,
            None,
        )
        .await
        .unwrap();
        let prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);
        let keys = provider.metadata.lock().unwrap().clone();
        assert_eq!(
            keys.iter().map(|key| key.as_str()).collect::<Vec<_>>(),
            [
                format!("{prefix}/index.enc"),
                format!("{prefix}/manifest.enc"),
                format!("{prefix}/manifest.hash"),
                format!("{prefix}/{COMMITTED_MARKER}"),
            ]
        );
        let expected = serde_json::to_vec(&manifest).unwrap();
        assert_eq!(
            std::fs::read_to_string(provider.inner.root.join(format!("{prefix}/manifest.hash")))
                .unwrap(),
            format!("blake3:{}", blake3::hash(&expected).to_hex())
        );
        let restored =
            read_committed_manifest(&provider, &master, &manifest.app.app_id, manifest.bundle_id)
                .await
                .unwrap();
        assert_eq!(serde_json::to_vec(&restored).unwrap(), expected);
        let restored_index =
            read_pack_index(&provider, &master, &manifest.app.app_id, manifest.bundle_id)
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_vec(&restored_index).unwrap(),
            serde_json::to_vec(&index).unwrap()
        );
        assert!(read_pack_index(
            &provider,
            &MasterKey::generate(),
            &manifest.app.app_id,
            manifest.bundle_id
        )
        .await
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn operation_inventory_is_reused_and_partial_failure_preserves_only_completed_receipts() {
        let (root, db, operation) = fixture();
        let provider = InventoryProvider::new(&root);
        let a = pack(&root, b"a");
        let b = pack(&root, b"bb");
        let c = pack(&root, b"ccc");
        let session = PackUploadSession::new(&provider, Some(&db), Some(operation));
        session.upload(std::slice::from_ref(&a)).await.unwrap();
        provider.fail_upload.store(3, Ordering::SeqCst);
        assert!(session.upload(&[b.clone(), c.clone()]).await.is_err());
        assert!(db
            .sync_journal_completed(operation, &pack_key(&a.0))
            .unwrap());
        assert!(db
            .sync_journal_completed(operation, &pack_key(&b.0))
            .unwrap());
        assert!(!db
            .sync_journal_completed(operation, &pack_key(&c.0))
            .unwrap());
        // A completed receipt remains usable even after local ciphertext eviction.
        std::fs::remove_file(&a.1).unwrap();
        std::fs::remove_file(&b.1).unwrap();
        session.upload(&[a, b, c]).await.unwrap();
        assert_eq!(provider.lists.load(Ordering::SeqCst), 1);
        assert_eq!(provider.uploads.load(Ordering::SeqCst), 4);
        assert_eq!(
            db.sync_journal_summary_for_kind(operation, ContentObjectKind::Pack)
                .unwrap()
                .completed_items,
            3
        );
        assert!(provider.metadata.lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn confirmed_uploads_update_inventory_even_without_a_database() {
        let (root, _, _) = fixture();
        let provider = InventoryProvider::new(&root);
        let file = pack(&root, b"immutable");
        let session = PackUploadSession::new(&provider, None, None);
        session.upload(std::slice::from_ref(&file)).await.unwrap();
        session.upload(std::slice::from_ref(&file)).await.unwrap();
        assert_eq!(provider.lists.load(Ordering::SeqCst), 1);
        assert_eq!(provider.uploads.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn listing_failure_is_never_treated_as_an_empty_inventory() {
        let (root, db, operation) = fixture();
        let provider = InventoryProvider::new(&root);
        let file = pack(&root, b"immutable");
        provider.fail_listing.store(true, Ordering::SeqCst);
        let session = PackUploadSession::new(&provider, Some(&db), Some(operation));
        assert!(session.upload(std::slice::from_ref(&file)).await.is_err());
        assert_eq!(provider.uploads.load(Ordering::SeqCst), 0);
        assert!(!db
            .sync_journal_completed(operation, &pack_key(&file.0))
            .unwrap());
        provider.fail_listing.store(false, Ordering::SeqCst);
        session.upload(&[file]).await.unwrap();
        assert_eq!(provider.lists.load(Ordering::SeqCst), 2);
        assert_eq!(provider.uploads.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn inventory_conflicts_and_racing_writes_cannot_overwrite_immutable_objects() {
        let (root, db, operation) = fixture();
        let provider = InventoryProvider::new(&root);
        let file = pack(&root, b"local");
        let key = RemoteKey::new(pack_key(&file.0));
        provider.ensure_root().await.unwrap();
        let remote = provider.inner.root.join(key.as_str());
        std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
        std::fs::write(&remote, b"different size").unwrap();
        let session = PackUploadSession::new(&provider, Some(&db), Some(operation));
        assert!(session.upload(std::slice::from_ref(&file)).await.is_err());
        assert_eq!(provider.uploads.load(Ordering::SeqCst), 0);
        assert!(!db.sync_journal_completed(operation, key.as_str()).unwrap());
        std::fs::remove_file(&remote).unwrap();
        let known = crate::list_remote_known(&provider, &RemoteKey::new("packs"))
            .await
            .unwrap();
        std::fs::write(&remote, b"raced").unwrap(); // Same size, but a conflicting writer.
        assert!(provider
            .upload_immutable_bulk_known_observed(
                &[ImmutableUpload::new(file.1, key)],
                &known,
                None
            )
            .await
            .is_err());
        assert_eq!(std::fs::read(remote).unwrap(), b"raced");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn bundle_metadata_is_not_published_until_every_pack_succeeds() {
        let (root, _, _) = fixture();
        let provider = InventoryProvider::new(&root);
        provider.fail_upload.store(2, Ordering::SeqCst);
        let files = vec![pack(&root, b"first"), pack(&root, b"second")];
        let manifest = BundleManifest::new(
            ManifestApp::from(&AppIdentity::new(
                AppId::desktop("upload-test"),
                "Upload Test",
            )),
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "test".into(),
                captured_at: chrono::Utc::now(),
            },
            BackupMode::PersonalState,
        );
        let master = MasterKey::generate();
        assert!(commit_bundle(&provider, &master, &manifest, &files, None)
            .await
            .is_err());
        assert!(provider.metadata.lock().unwrap().is_empty());
        commit_bundle(&provider, &master, &manifest, &files, None)
            .await
            .unwrap();
        assert!(provider
            .metadata
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .ends_with(COMMITTED_MARKER));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overlapping_upload_progress_preserves_other_transfers_and_clears_cancellation() {
        let (root, db, operation) = fixture();
        let a = pack(&root, &[0; 10]);
        let b = pack(&root, &[1; 20]);
        for (id, path) in [&a, &b] {
            db.start_sync_journal_item(
                operation,
                &pack_key(id),
                ContentObjectKind::Pack,
                SyncDirection::Upload,
                None,
                None,
                Some(std::fs::metadata(path).unwrap().len()),
            )
            .unwrap();
        }
        let state = Arc::default();
        let first = JournalUploadObserver {
            db: &db,
            operation_id: operation,
            state: Arc::clone(&state),
            transfer_id: Uuid::new_v4(),
            keys: vec![pack_key(&a.0)],
        };
        let second = JournalUploadObserver {
            db: &db,
            operation_id: operation,
            state,
            transfer_id: Uuid::new_v4(),
            keys: vec![pack_key(&b.0)],
        };
        first.transfer_started().unwrap();
        second.transfer_started().unwrap();
        first.transfer_progress(4).unwrap();
        second.transfer_progress(7).unwrap();
        second
            .persist_progress(&mut second.state.lock().unwrap(), true)
            .unwrap();
        assert_eq!(
            db.get_operation_progress(operation)
                .unwrap()
                .unwrap()
                .completed_units,
            11
        );
        first
            .completed(
                &[ImmutableUpload::new(a.1, RemoteKey::new(pack_key(&a.0)))],
                ImmutableUploadCompletion::Uploaded,
            )
            .unwrap();
        first
            .persist_progress(&mut first.state.lock().unwrap(), true)
            .unwrap();
        assert_eq!(
            db.get_operation_progress(operation)
                .unwrap()
                .unwrap()
                .completed_units,
            17
        );
        drop(second);
        assert_eq!(
            db.get_operation_progress(operation)
                .unwrap()
                .unwrap()
                .completed_units,
            10
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upload_observer_separates_ready_bytes_from_network_bytes() {
        let db = noland_state_db::StateDb::open_in_memory().unwrap();
        let operation_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        db.upsert_operation(&OperationRecord {
            operation_id,
            kind: "backup".into(),
            app_id: None,
            state: "UPLOADING".into(),
            created_at: now,
            updated_at: now,
            last_error: None,
            detail_json: serde_json::json!({}),
        })
        .unwrap();
        db.set_operation_progress(operation_id, Some(&OperationProgress::new("uploading", 0)))
            .unwrap();
        let root = std::env::temp_dir().join(format!("noland-upload-observer-{operation_id}"));
        std::fs::create_dir_all(&root).unwrap();
        let uploaded = ImmutableUpload::new(root.join("uploaded.pack"), RemoteKey::new("packs/a"));
        let reused = ImmutableUpload::new(root.join("reused.pack"), RemoteKey::new("packs/b"));
        std::fs::write(&uploaded.local, vec![0_u8; 10]).unwrap();
        std::fs::write(&reused.local, vec![0_u8; 20]).unwrap();
        for upload in [&uploaded, &reused] {
            db.start_sync_journal_item(
                operation_id,
                upload.key.as_str(),
                ContentObjectKind::Pack,
                SyncDirection::Upload,
                Some(&upload.local.to_string_lossy()),
                Some(upload.key.as_str()),
                Some(std::fs::metadata(&upload.local).unwrap().len()),
            )
            .unwrap();
        }
        let observer = JournalUploadObserver {
            db: &db,
            operation_id,
            state: Arc::default(),
            transfer_id: Uuid::new_v4(),
            keys: vec!["packs/a".into(), "packs/b".into()],
        };

        observer.transfer_started().unwrap();
        observer.transfer_progress(4).unwrap();
        let live = db.get_operation_progress(operation_id).unwrap().unwrap();
        assert_eq!(live.completed_units, 4);
        assert_eq!(live.detail_json["bytes_transferred"], 4);

        observer
            .completed(&[uploaded], ImmutableUploadCompletion::Uploaded)
            .unwrap();
        observer
            .completed(&[reused], ImmutableUploadCompletion::ReusedRemote)
            .unwrap();
        drop(observer);
        let summary = db
            .sync_journal_summary_for_kind(operation_id, ContentObjectKind::Pack)
            .unwrap();
        assert_eq!(summary.completed_items, 2);
        assert_eq!(summary.completed_bytes, 30);
        assert_eq!(summary.bytes_transferred, 10);
        let final_progress = db.get_operation_progress(operation_id).unwrap().unwrap();
        assert_eq!(final_progress.completed_units, 30);
        assert_eq!(final_progress.detail_json["bytes_transferred"], 10);
        std::fs::remove_dir_all(root).unwrap();
    }
}
