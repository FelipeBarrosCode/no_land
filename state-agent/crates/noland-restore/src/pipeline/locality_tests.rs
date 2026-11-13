use super::*;
use crate::tests::{test_dir, test_file, test_manifest, test_plan};
use noland_state_core::{
    AppIdentity, AppMutationKind, AppMutationRecord, ChunkRef, FileStateTrust, PersistenceClass,
    SemanticRole,
};

fn file(name: &str, chunks: &[&[u8]]) -> noland_state_core::ManifestFile {
    let mut file = test_file(
        name,
        PersistenceClass::PersistentState,
        SemanticRole::UserState,
        &chunks.concat(),
    );
    file.chunks = chunks
        .iter()
        .map(|chunk| ChunkRef {
            hash: noland_cas::blake3_hex(chunk),
            size: chunk.len() as u64,
        })
        .collect();
    file
}

fn database(plan: &RestorePlan) -> StateDb {
    let db = StateDb::open_in_memory().unwrap();
    db.upsert_app(&AppIdentity::new(
        plan.manifest.app.app_id.clone(),
        "Restored",
    ))
    .unwrap();
    db
}

#[test]
fn shared_restore_plan_keeps_manifest_and_priority_allocations() {
    let root = test_dir("shared-restore-plan");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", &[b"save"])]));
    let files = plan.manifest.files.as_ptr();
    let shared = plan.into_shared();
    let worker = SharedRestorePlan::from(&shared);
    assert_eq!(shared.manifest.files.as_ptr(), files);
    assert_eq!(worker.manifest.files.as_ptr(), files);
    assert!(Arc::ptr_eq(
        &shared.priority_plan(),
        &worker.priority_plan()
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pack_schedule_follows_manifest_chunk_order_independently_of_pack_uuid() {
    let root = test_dir("pack-locality");
    // Reverse lexical filenames to distinguish manifest order from path order.
    let plan = test_plan(
        &root,
        test_manifest(vec![
            file("z-first", &[b"a", b"b"]),
            file("a-last", &[b"c"]),
        ]),
    );
    let needed = [b"a", b"b", b"c"]
        .iter()
        .map(|bytes| noland_cas::blake3_hex(*bytes))
        .collect();
    for ids in [
        ["ffffffff", "00000000", "77777777"],
        ["00000000", "ffffffff", "77777777"],
    ] {
        let index = [b"a", b"b", b"c"]
            .into_iter()
            .zip(ids)
            .map(|(bytes, id)| PackIndexEntry {
                chunk_hash: noland_cas::blake3_hex(bytes),
                pack_id: id.into(),
                offset: 0,
                ciphertext_len: 17,
                plaintext_len: 1,
                compression: Default::default(),
                nonce: vec![0; 24],
            })
            .collect::<Vec<_>>();
        let jobs = pack_jobs(&plan, &plan.priority_plan(), &index, &needed).unwrap();
        assert_eq!(
            jobs.iter()
                .map(|job| job.entries[0].chunk_hash.clone())
                .collect::<Vec<_>>(),
            index
                .iter()
                .map(|entry| entry.chunk_hash.clone())
                .collect::<Vec<_>>()
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn assembly_reuses_hot_handles_and_bounds_scattered_open_files() {
    for count in [1, MAX_ASSEMBLY_HANDLES + 7] {
        let root = test_dir("bounded-handles");
        let plan = test_plan(
            &root,
            test_manifest(
                (0..count)
                    .map(|index| file(&format!("game/{index}"), &[b"a", b"b", b"c"]))
                    .collect(),
            ),
        );
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut tx = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
        for chunk in [b"a", b"b", b"c"] {
            pipeline
                .consume(&noland_cas::blake3_hex(chunk), chunk)
                .unwrap();
            assert!(pipeline.handles.files.len() <= MAX_ASSEMBLY_HANDLES);
        }
        assert!(
            pipeline.handles.files.is_empty(),
            "completed files release cached FDs"
        );
        let report = pipeline.finish().unwrap();
        assert!(report.io.peak_assembly_handles <= MAX_ASSEMBLY_HANDLES as u64);
        assert_eq!(report.io.out_of_order_files, 0);
        if count == 1 {
            assert_eq!(report.io.assembly_opens, 1);
            assert_eq!(report.io.assembly_handle_hits, 2);
        } else {
            assert!(report.io.assembly_handle_evictions > 0);
        }
        tx.commit().unwrap();
        for index in 0..count {
            assert_eq!(
                fs::read(root.join(format!("home/.local/share/game/{index}"))).unwrap(),
                b"abc"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn cached_descriptor_rejects_regular_file_replacement_and_parent_redirection() {
    for redirect_parent in [false, true] {
        let root = test_dir("cached-handle-replaced");
        let plan = test_plan(&root, test_manifest(vec![file("game/save", &[b"a", b"b"])]));
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut tx = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"a"), b"a")
            .unwrap();
        let partial = pipeline.pending.values().next().unwrap().partial.clone();
        let parent = partial.parent().unwrap();
        let moved = root.join("moved");
        if redirect_parent {
            fs::rename(parent, &moved).unwrap();
            std::os::unix::fs::symlink(&moved, parent).unwrap();
        } else {
            fs::rename(&partial, &moved).unwrap();
            fs::write(&partial, b"unrelated").unwrap();
        }
        assert!(pipeline
            .consume(&noland_cas::blake3_hex(b"b"), b"b")
            .is_err());
        if redirect_parent {
            assert_eq!(
                fs::read(moved.join(partial.file_name().unwrap())).unwrap(),
                b"a"
            );
            fs::remove_file(parent).unwrap();
            fs::rename(&moved, parent).unwrap();
        } else {
            assert_eq!(fs::read(&partial).unwrap(), b"unrelated");
            assert_eq!(fs::read(&moved).unwrap(), b"a");
        }
        drop(pipeline);
        drop(tx);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn committed_restore_seeds_actual_destination_identity_and_reused_file_hashes() {
    let root = test_dir("restore-trusted-state");
    let mut source = file("game/save", &[b"save"]);
    source.mtime_ns = Some(1); // Source metadata must never masquerade as destination metadata.
    source.source_path_hint = Some("/another-pc/save".into());
    let plan = test_plan(&root, test_manifest(vec![source.clone()]));
    let db = database(&plan);
    let roots = LogicalRootMap::from_home(root.join("home"));
    let destination = root.join("home/.local/share/game/save");
    let mut tx = RestoreTransaction::new(&plan, &roots, Some(&db));
    let mut pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"save"), b"save")
        .unwrap();
    pipeline.finish().unwrap();
    assert!(db
        .list_file_states(&plan.manifest.app.app_id, None)
        .unwrap()
        .is_empty());
    tx.commit().unwrap();
    let destination = fs::canonicalize(destination).unwrap();
    let states = db
        .list_file_states(&plan.manifest.app.app_id, None)
        .unwrap();
    let metadata = fs::metadata(&destination).unwrap();
    let identity = VerifiedIdentity::from_metadata(&metadata);
    assert_eq!(states.len(), 1);
    assert!(states[0].fast_identity_matches(
        metadata.len(),
        identity.mtime_ns,
        Some(metadata.ino()),
        Some(metadata.dev())
    ));
    assert_eq!(states[0].mode, Some(metadata.mode()));
    assert_eq!(states[0].canonical_path.as_deref(), destination.to_str());
    assert_eq!(
        states[0].content_hash.as_deref(),
        Some(source.file_hash.as_str())
    );
    let record = db
        .get_path_by_canonical(destination.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(record.inode, Some(metadata.ino() as i64));
    assert_eq!(record.mount_id, Some(metadata.dev() as i64));

    let retry = test_plan(&root.join("retry"), test_manifest(vec![source]));
    let mut tx = RestoreTransaction::new(&retry, &roots, Some(&db));
    let pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
    let report = pipeline.finish().unwrap();
    assert_eq!(report.restore.reused_entries, 1);
    tx.commit().unwrap();
    assert_eq!(
        db.list_file_states(&plan.manifest.app.app_id, None)
            .unwrap()[0]
            .trust,
        FileStateTrust::Trusted
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn post_verification_edit_or_observer_event_cannot_seed_trusted_state() {
    for event_only in [false, true] {
        let root = test_dir("restore-index-race");
        let plan = test_plan(&root, test_manifest(vec![file("game/save", &[b"save"])]));
        let db = database(&plan);
        let roots = LogicalRootMap::from_home(root.join("home"));
        let destination = root.join("home/.local/share/game/save");
        let app = &plan.manifest.app.app_id;
        let mut tx = RestoreTransaction::new(&plan, &roots, Some(&db));
        let mut pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"save"), b"save")
            .unwrap();
        pipeline.finish().unwrap();
        if event_only {
            db.append_app_mutation(&AppMutationRecord::new(
                app.clone(),
                destination.to_str().unwrap(),
                AppMutationKind::Modify,
            ))
            .unwrap();
        } else {
            let old_mtime = fs::metadata(&destination).unwrap().modified().unwrap();
            fs::write(&destination, b"edit").unwrap();
            // Same inode, length AND mtime; ctime must still invalidate the observation.
            fs::File::options()
                .write(true)
                .open(&destination)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old_mtime))
                .unwrap();
        }
        tx.commit().unwrap();
        let states = db.list_file_states(app, None).unwrap();
        assert_eq!(states[0].trust, FileStateTrust::VerifyRequired);
        assert!(states[0].content_hash.is_none());
        assert!(db.list_dirty_apps().unwrap()[0].requires_reconciliation);
        if event_only {
            assert_eq!(db.count_pending_app_mutations(app).unwrap(), 1);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn changed_verified_partial_is_rejected_even_with_unchanged_inode_and_size() {
    let root = test_dir("verified-partial-edited");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", &[b"save"])]));
    let db = database(&plan);
    let roots = LogicalRootMap::from_home(root.join("home"));
    {
        let mut tx = RestoreTransaction::new(&plan, &roots, Some(&db));
        let mut pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"save"), b"save")
            .unwrap();
        pipeline.dispatch_assembled().unwrap();
        pipeline.collect_verified(Duration::from_secs(5)).unwrap();
        fs::write(&pipeline.ready[0].pending.partial, b"edit").unwrap();
        assert!(pipeline.finish().is_err());
    }
    assert!(db
        .list_file_states(&plan.manifest.app.app_id, None)
        .unwrap()
        .is_empty());
    assert!(db
        .associations_for_app(&plan.manifest.app.app_id)
        .unwrap()
        .is_empty());
    assert!(!root.join("home/.local/share/game/save").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn aborted_publication_never_seeds_hashes_or_associations() {
    let root = test_dir("restore-index-aborted");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", &[b"save"])]));
    let db = database(&plan);
    let roots = LogicalRootMap::from_home(root.join("home"));
    {
        let mut tx = RestoreTransaction::new(&plan, &roots, Some(&db));
        let mut pipeline = Pipeline::new(&mut tx, 1, |_| Ok(())).unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"save"), b"save")
            .unwrap();
        pipeline.finish().unwrap();
        // Cancellation drops the transaction after publication, before commit.
    }
    assert!(db
        .list_file_states(&plan.manifest.app.app_id, None)
        .unwrap()
        .is_empty());
    assert!(db
        .associations_for_app(&plan.manifest.app.app_id)
        .unwrap()
        .is_empty());
    assert!(!root.join("home/.local/share/game/save").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incomplete_file_hash_fanout_is_bounded_and_eviction_requires_full_verification() {
    for bad_hash in [false, true] {
        let root = test_dir("hash-fanout");
        let count = MAX_ACTIVE_HASHERS + 17;
        let mut files = (0..count)
            .map(|index| file(&format!("game/{index:04}"), &[b"a", b"b"]))
            .collect::<Vec<_>>();
        if bad_hash {
            files[0].file_hash = noland_cas::blake3_hex(b"bad full-file digest");
        }
        let plan = test_plan(&root, test_manifest(files));
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut readback = 0;
        let mut pipeline = Pipeline::new(&mut transaction, 2, |sample| {
            readback = sample.readback_bytes;
            Ok(())
        })
        .unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"a"), b"a")
            .unwrap();
        assert_eq!(pipeline.pending.len(), count);
        assert_eq!(
            pipeline
                .pending
                .values()
                .filter(|file| file.hasher.is_some())
                .count(),
            MAX_ACTIVE_HASHERS
        );
        assert_eq!(pipeline.progress.io.hash_state_evictions, 17);
        assert!(pipeline.pending[&0].readback);
        let result = pipeline
            .consume(&noland_cas::blake3_hex(b"b"), b"b")
            .and_then(|_| pipeline.finish());
        if bad_hash {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("file hash mismatch"));
            drop(transaction);
            assert!(!root.join("home/.local/share/game/0000").exists());
        } else {
            let report = result.unwrap();
            assert_eq!(report.io.peak_active_hashers, MAX_ACTIVE_HASHERS as u64);
            assert_eq!(report.io.out_of_order_files, 0);
            assert_eq!(readback, 34);
            transaction.commit().unwrap();
            for index in 0..count {
                assert_eq!(
                    fs::read(root.join(format!("home/.local/share/game/{index:04}"))).unwrap(),
                    b"ab"
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
#[ignore = "opt-in DB + authenticated codec + restore commit workload"]
async fn database_backed_codec_restore_workload() {
    use noland_storage::{commit_bundle_with_index, LocalStorage};
    let root = test_dir("db-codec-workload");
    let count = 16_384;
    let payload = b"deduplicated authenticated fixture bytes";
    let manifest = test_manifest(
        (0..count)
            .map(|index| file(&format!("game/{:04}/{index:06}", index / 512), &[payload]))
            .collect(),
    );
    let storage = LocalStorage::new(root.join("remote"));
    let master = MasterKey::generate();
    let packs = noland_pack::pack_chunks(
        &root.join("packs"),
        &master,
        vec![(noland_cas::blake3_hex(payload), payload.to_vec())],
        |_| false,
    )
    .unwrap();
    let index = packs
        .iter()
        .flat_map(|pack| pack.entries.clone())
        .collect::<Vec<_>>();
    let pack_files = packs
        .iter()
        .map(|pack| (pack.pack_id.clone(), pack.path.clone()))
        .collect::<Vec<_>>();
    commit_bundle_with_index(
        &storage,
        &master,
        &manifest,
        &pack_files,
        Some(&serde_json::to_vec(&index).unwrap()),
        None,
    )
    .await
    .unwrap();
    let paths = noland_state_core::AgentPaths::from_roots(root.join("state"), root.join("run"));
    let db = StateDb::open(&root.join("fixture.sqlite")).unwrap();
    db.upsert_app(&AppIdentity::new(manifest.app.app_id.clone(), "Fixture"))
        .unwrap();
    let roots = LogicalRootMap::from_home(root.join("home"));
    let started = Instant::now();
    let plan = crate::prepare_restore(
        &storage,
        &master,
        &paths,
        &manifest.app.app_id,
        manifest.bundle_id,
        noland_state_core::RestoreMode::CompleteApplication,
    )
    .await
    .unwrap()
    .into_shared();
    assert!(!plan.staging.join("manifest/manifest.json").exists());
    let report = restore_pipelined(
        &storage,
        &master,
        &plan,
        &index,
        &roots,
        Some(&db),
        None,
        PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(report.restore.published_entries, count);
    assert_eq!(
        db.associations_for_app(&manifest.app.app_id).unwrap().len(),
        count
    );
    let states = db.list_file_states(&manifest.app.app_id, None).unwrap();
    assert_eq!(states.len(), count);
    assert!(states
        .iter()
        .all(|state| state.trust == FileStateTrust::Trusted));
    assert!(report.io.commit.database_us > 0);
    assert!(report.io.commit.elapsed_us >= report.io.commit.database_us);
    assert!(report.pipeline_duration_ms >= report.io.commit.elapsed_us / 1000);
    assert!(report.io.peak_unpublished_files <= MAX_UNPUBLISHED_FILES as u64);
    assert!(report.io.peak_unpublished_bytes <= MAX_UNPUBLISHED_BYTES);
    println!(
        "RESTORE_DB_CODEC {}",
        serde_json::json!({"files": count, "elapsed_ms": started.elapsed().as_millis(), "pipeline_ms": report.pipeline_duration_ms, "io": report.io})
    );
    for index in 0..count {
        assert_eq!(
            fs::read(root.join(format!(
                "home/.local/share/game/{:04}/{index:06}",
                index / 512
            )))
            .unwrap(),
            payload
        );
    }
    fs::remove_dir_all(root).unwrap();
}
