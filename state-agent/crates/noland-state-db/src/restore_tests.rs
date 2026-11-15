use super::*;

fn fixture(
    count: usize,
) -> (
    StateDb,
    AppId,
    Vec<RestoredPathAssociationInput>,
    Vec<FileStateRecord>,
) {
    let db = StateDb::open_in_memory().unwrap();
    let app = AppIdentity::new(AppId::desktop("restore-batch"), "Restore");
    db.upsert_app(&app).unwrap();
    let entries: Vec<_> = (0..count)
        .map(|n| RestoredPathAssociationInput {
            canonical_path: format!("/home/user/file-{n}"),
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::UserState,
        })
        .collect();
    let states = entries
        .iter()
        .enumerate()
        .map(|(n, entry)| FileStateRecord {
            app_id: app.app_id.clone(),
            logical_root: "$HOME".into(),
            relative_path: format!("file-{n}"),
            canonical_path: Some(entry.canonical_path.clone()),
            file_type: FileType::File,
            size: 4,
            mtime_ns: 12,
            inode: Some(10 + n as u64),
            mount_id: Some(3),
            mode: Some(0o600),
            content_hash: Some(format!("hash-{n}")),
            trust: FileStateTrust::Trusted,
            last_seen_at: Utc::now(),
            last_hashed_at: Some(Utc::now()),
        })
        .collect();
    (db, app.app_id, entries, states)
}

#[test]
fn batched_restore_merges_prior_evidence_and_checks_every_eligible_identity() {
    let (db, app, entries, states) = fixture(BATCH * 2 + 5);
    let prior = Evidence::new(EvidenceKind::DirectCgroupWrite).with_detail("keep provenance");
    for entry in entries.iter().step_by(2) {
        db.upsert_association(&PathAssociation {
            app_id: app.clone(),
            path_id: db.upsert_path(&entry.canonical_path).unwrap(),
            confidence: 0.7,
            persistence_class: PersistenceClass::Unknown,
            semantic_role: SemanticRole::Unknown,
            evidence: vec![prior.clone()],
            first_seen_at: prior.observed_at,
            last_seen_at: prior.observed_at,
        })
        .unwrap();
    }
    for (index, trust) in [(1, FileStateTrust::Dirty), (BATCH, FileStateTrust::Missing)] {
        let mut state = states[index].clone();
        state.trust = trust;
        state.content_hash = Some("newer-local-hash".into());
        db.upsert_file_state(&state).unwrap();
    }
    let mut verified = 0;
    let before = db.learned_checkpoint_generation().unwrap();
    let started = std::time::Instant::now();
    let trusted = db
        .commit_restored_paths(
            &app,
            &entries,
            &states,
            Some(db.capture_generation(&app).unwrap()),
            |state| {
                verified += 1;
                state.relative_path != format!("file-{}", BATCH + 1)
            },
        )
        .unwrap();
    eprintln!(
        "restore publication: {} rows, {trusted} trusted, {:?}",
        states.len(),
        started.elapsed()
    );
    assert_eq!(verified, states.len() - 2);
    assert_eq!(trusted, states.len() - 3);
    assert_ne!(before, db.learned_checkpoint_generation().unwrap());
    for (index, entry) in entries.iter().enumerate() {
        let path = db
            .get_path_by_canonical(&entry.canonical_path)
            .unwrap()
            .unwrap();
        let associations = db.associations_for_path(path.path_id).unwrap();
        let evidence = &associations[0].evidence;
        assert_eq!(evidence.len(), if index % 2 == 0 { 3 } else { 2 });
        assert!(evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::ExplicitUserBinding));
        assert!(evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::RestoredFromCommittedBundle));
        if index % 2 == 0 {
            assert_eq!(evidence[0].detail, prior.detail);
            assert_eq!(evidence[0].observed_at, prior.observed_at);
        }
        let state = db
            .get_file_state(&app, "$HOME", &states[index].relative_path)
            .unwrap()
            .unwrap();
        match index {
            1 => assert_eq!(state.trust, FileStateTrust::Dirty),
            BATCH => assert_eq!(state.trust, FileStateTrust::Missing),
            n if n == BATCH + 1 => assert_eq!(state.trust, FileStateTrust::VerifyRequired),
            _ => {
                assert_eq!(state.trust, FileStateTrust::Trusted);
                assert_eq!(path.content_hash, states[index].content_hash);
                assert_eq!(path.logical_root.as_deref(), Some("$HOME"));
            }
        }
    }
    // An identical replay preserves merged evidence and semantic coverage.
    let generation = db.learned_checkpoint_generation().unwrap();
    db.commit_restored_paths(
        &app,
        &entries,
        &states,
        Some(db.capture_generation(&app).unwrap()),
        |state| state.relative_path != format!("file-{}", BATCH + 1),
    )
    .unwrap();
    assert_eq!(generation, db.learned_checkpoint_generation().unwrap());
    assert!(db.list_dirty_apps().unwrap()[0].requires_reconciliation);
}

#[test]
fn restore_rolls_back_earlier_batches_and_both_generations_on_late_error() {
    let (db, app, entries, mut states) = fixture(BATCH * 2 + 1);
    states.last_mut().unwrap().size = u64::MAX;
    let semantic = db.learned_checkpoint_generation().unwrap();
    let capture = db.capture_generation(&app).unwrap();
    assert!(db
        .commit_restored_paths(&app, &entries, &states, Some(capture), |_| true)
        .is_err());
    assert!(db.associations_for_app(&app).unwrap().is_empty());
    assert!(db.list_file_states(&app, None).unwrap().is_empty());
    assert!(db
        .get_path_by_canonical(&entries[0].canonical_path)
        .unwrap()
        .is_none());
    assert_eq!(semantic, db.learned_checkpoint_generation().unwrap());
    assert_eq!(capture, db.capture_generation(&app).unwrap());
}

#[test]
fn duplicate_restore_keys_preserve_ordered_trust_and_metadata_semantics() {
    let (db, app, entries, mut states) = fixture(1);
    states.push(states[0].clone());
    let mut calls = 0;
    assert_eq!(
        db.commit_restored_paths(&app, &entries, &states, Some(0), |_| {
            calls += 1;
            calls == 1
        })
        .unwrap(),
        1
    );
    let state = db.get_file_state(&app, "$HOME", "file-0").unwrap().unwrap();
    assert_eq!(state.trust, FileStateTrust::VerifyRequired);
    assert_eq!(state.content_hash, states[0].content_hash);
    assert_eq!(
        db.get_path_by_canonical(&entries[0].canonical_path)
            .unwrap()
            .unwrap()
            .content_hash,
        states[0].content_hash
    );
    let generation = db.capture_generation(&app).unwrap();
    let mut calls = 0;
    db.commit_restored_paths(&app, &entries, &states, Some(generation), |_| {
        calls += 1;
        calls == 2
    })
    .unwrap();
    assert_eq!(
        db.get_file_state(&app, "$HOME", "file-0")
            .unwrap()
            .unwrap()
            .trust,
        FileStateTrust::Trusted
    );
}

#[test]
fn invalidation_waiting_for_restore_wins_after_atomic_publication() {
    let (db, app, entries, states) = fixture(1);
    let writer = db.clone();
    let writer_app = app.clone();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        start_rx.recv().unwrap();
        ready_tx.send(()).unwrap();
        writer
            .set_file_state_trust(&writer_app, "$HOME", "file-0", FileStateTrust::Dirty)
            .unwrap();
    });
    db.commit_restored_paths(&app, &entries, &states, Some(0), |_| {
        start_tx.send(()).unwrap();
        ready_rx.recv().unwrap();
        true
    })
    .unwrap();
    thread.join().unwrap();
    assert_eq!(
        db.get_file_state(&app, "$HOME", "file-0")
            .unwrap()
            .unwrap()
            .trust,
        FileStateTrust::Dirty
    );
    assert_eq!(db.capture_generation(&app).unwrap(), 2);
}

#[test]
fn prior_evidence_lookup_work_is_bounded_as_the_app_grows() {
    let (db, app, entries, states) = fixture(16_384);
    db.commit_restored_paths(&app, &entries[..512], &states[..512], Some(0), |_| true)
        .unwrap();
    let lookup_work = || {
        let conn = db.lock().unwrap();
        let mut stmt = conn.prepare(&prior_evidence_sql(BATCH)).unwrap();
        let mut values = vec![text(app.as_str())];
        values.extend(
            entries[..BATCH]
                .iter()
                .map(|entry| text(&entry.canonical_path)),
        );
        let mut count = 0;
        {
            let mut rows = stmt
                .query(params_from_iter(values.iter().copied().map(SqlValue)))
                .unwrap();
            while let Some(row) = rows.next().unwrap() {
                assert!(ordinal(row).unwrap() < BATCH);
                let evidence: Vec<Evidence> =
                    serde_json::from_str(&row.get::<_, String>(1).unwrap()).unwrap();
                assert!(evidence
                    .iter()
                    .any(|item| item.kind == EvidenceKind::RestoredFromCommittedBundle));
                count += 1;
            }
        }
        assert_eq!(count, BATCH);
        stmt.get_status(rusqlite::StatementStatus::VmStep)
    };
    let small = lookup_work();
    db.commit_restored_paths(
        &app,
        &entries[512..],
        &states[512..],
        Some(db.capture_generation(&app).unwrap()),
        |_| true,
    )
    .unwrap();
    let large = lookup_work();
    eprintln!("prior-evidence VM steps: 512 associations={small}, 16384 associations={large}");
    assert!(small > 0);
    // Counts VM instructions, not wall time. An app-first nested-loop plan grows
    // with all app associations for each batch; indexed input-first probes do not.
    assert!(
        large <= small.saturating_mul(2),
        "unbounded lookup work: {small} -> {large}"
    );
}
