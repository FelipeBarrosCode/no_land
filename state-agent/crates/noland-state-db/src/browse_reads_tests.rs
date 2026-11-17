use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn fixture() -> (std::path::PathBuf, StateDb, AppIdentity, OperationRecord) {
    let root = std::env::temp_dir().join(format!("browse-reader-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let db = StateDb::open(&root.join("state.db")).unwrap();
    let app = AppIdentity::new(AppId::desktop("reader-fixture"), "Committed");
    db.upsert_app(&app).unwrap();
    let now = Utc::now();
    let operation = OperationRecord {
        operation_id: Uuid::new_v4(),
        kind: "backup".into(),
        app_id: Some(app.app_id.clone()),
        state: "QUEUED".into(),
        created_at: now,
        updated_at: now,
        last_error: None,
        detail_json: serde_json::json!({}),
    };
    (root, db, app, operation)
}

#[test]
fn health_and_index_reads_finish_before_a_busy_writer_is_released() {
    let (root, db, app, operation) = fixture();
    db.upsert_operation(&operation).unwrap();
    let writer = db.lock().unwrap();
    let reader = db.clone();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = (|| -> Result<_> {
            Ok((
                reader.indexed_apps_for_browse()?,
                reader.unfinished_operation_count_for_health()?,
            ))
        })();
        send.send(result).unwrap();
    });
    // The writer is deliberately held until AFTER this result. A generous
    // deadline detects serialization without relying on sub-millisecond timing.
    let result = receive.recv_timeout(Duration::from_secs(3));
    drop(writer);
    worker.join().unwrap();
    let (apps, unfinished) = result
        .expect("small reads must not wait for the observer/checkpoint writer")
        .unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].app_id, app.app_id);
    assert_eq!(
        unfinished, 1,
        "never report a guessed zero under contention"
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn indexed_reads_observe_commits_without_leaking_uncommitted_changes() {
    let (root, db, app, mut operation) = fixture();
    operation.state = "COMPLETED".into();
    db.upsert_operation(&operation).unwrap();
    let writer = db.lock().unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer
        .execute(
            "UPDATE apps SET display_name='Changed' WHERE app_id=?1",
            [app.app_id.as_str()],
        )
        .unwrap();
    writer
        .execute(
            "UPDATE operations SET state='QUEUED' WHERE operation_id=?1",
            [operation.operation_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        db.indexed_apps_for_browse().unwrap()[0].display_name,
        "Committed"
    );
    assert_eq!(db.unfinished_operation_count_for_health().unwrap(), 0);
    writer.execute_batch("COMMIT").unwrap();
    drop(writer);
    assert_eq!(
        db.indexed_apps_for_browse().unwrap()[0].display_name,
        "Changed"
    );
    assert_eq!(db.unfinished_operation_count_for_health().unwrap(), 1);
    operation.state = "CANCELLED".into();
    db.upsert_operation(&operation).unwrap();
    assert_eq!(db.unfinished_operation_count_for_health().unwrap(), 0);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn browse_connection_is_query_only_and_cannot_mutate_or_migrate() {
    let (root, db, _, _) = fixture();
    let reader = db.browse_reader.as_ref().unwrap().lock().unwrap();
    let query_only: bool = reader
        .pragma_query_value(None, "query_only", |row| row.get(0))
        .unwrap();
    assert!(query_only);
    let error = reader.execute("DELETE FROM apps", []).unwrap_err();
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(ref error, _) if error.code == rusqlite::ErrorCode::ReadOnly
    ));
    drop(reader);
    assert_eq!(db.list_apps().unwrap().len(), 1);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn independent_reads_do_not_mask_a_poisoned_writer() {
    let (root, db, _, _) = fixture();
    let worker_db = db.clone();
    assert!(std::thread::spawn(move || {
        let _writer = worker_db.lock().unwrap();
        panic!("synthetic writer poison");
    })
    .join()
    .is_err());
    assert!(db.indexed_apps_for_browse().is_err());
    assert!(db.unfinished_operation_count_for_health().is_err());
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn queued_browse_reads_fail_closed_if_the_writer_becomes_poisoned() {
    let (root, db, _, _) = fixture();
    let held_reader = db.browse_reader.as_ref().unwrap().lock().unwrap();
    let pending = db.clone();
    let (ready, waiting) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        ready.send(()).unwrap();
        pending.indexed_apps_for_browse()
    });
    waiting.recv().unwrap();
    let writer_db = db.clone();
    assert!(std::thread::spawn(move || {
        let _writer = writer_db.lock().unwrap();
        panic!("synthetic concurrent writer poison");
    })
    .join()
    .is_err());
    drop(held_reader);
    assert!(reader.join().unwrap().is_err());
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn wal_reads_survive_rolled_back_ddl_and_propagate_schema_errors() {
    let (root, db, _, _) = fixture();
    {
        let reader = db.browse_reader.as_ref().unwrap().lock().unwrap();
        let journal: String = reader
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        let timeout: i64 = reader
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        assert_eq!(timeout, 250);
    }
    let writer = db.lock().unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; ALTER TABLE apps RENAME TO transient_apps;")
        .unwrap();
    assert_eq!(db.indexed_apps_for_browse().unwrap().len(), 1);
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    assert_eq!(db.indexed_apps_for_browse().unwrap().len(), 1);
    db.lock()
        .unwrap()
        .execute_batch("ALTER TABLE apps RENAME TO unavailable_apps")
        .unwrap();
    assert!(
        db.indexed_apps_for_browse().is_err(),
        "schema failures are not empty catalogs"
    );
    db.lock()
        .unwrap()
        .execute_batch("ALTER TABLE unavailable_apps RENAME TO apps")
        .unwrap();
    db.lock()
        .unwrap()
        .execute_batch("ALTER TABLE operations RENAME TO unavailable_operations")
        .unwrap();
    assert!(
        db.unfinished_operation_count_for_health().is_err(),
        "read failures are not zero operations"
    );
    db.lock()
        .unwrap()
        .execute_batch("ALTER TABLE unavailable_operations RENAME TO operations")
        .unwrap();
    assert_eq!(db.unfinished_operation_count_for_health().unwrap(), 0);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn memory_databases_keep_the_same_index_and_health_semantics() {
    for db in [
        StateDb::open_in_memory().unwrap(),
        StateDb::open(Path::new(":memory:")).unwrap(),
    ] {
        assert!(db.browse_reader.is_none());
        let app = AppIdentity::new(AppId::desktop("memory-reader"), "Memory");
        db.upsert_app(&app).unwrap();
        assert_eq!(db.indexed_apps_for_browse().unwrap()[0].app_id, app.app_id);
        assert_eq!(
            db.unfinished_operation_count_for_health().unwrap(),
            db.unfinished_operation_count().unwrap()
        );
    }
}

#[test]
fn repeated_gaps_keep_generation_barriers_without_rewriting_invalid_states() {
    let (root, db, app, _) = fixture();
    for (index, trust) in [
        FileStateTrust::Trusted,
        FileStateTrust::Dirty,
        FileStateTrust::VerifyRequired,
    ]
    .into_iter()
    .enumerate()
    {
        db.upsert_file_state(&FileStateRecord {
            app_id: app.app_id.clone(),
            logical_root: "$HOME".into(),
            relative_path: format!("save-{index}"),
            canonical_path: Some(format!("/home/user/save-{index}")),
            file_type: FileType::File,
            size: 1,
            mtime_ns: 1,
            inode: Some(index as u64 + 1),
            mount_id: Some(1),
            mode: Some(0o100600),
            content_hash: Some("synthetic".into()),
            trust,
            last_seen_at: Utc::now(),
            last_hashed_at: Some(Utc::now()),
        })
        .unwrap();
    }
    db.lock()
        .unwrap()
        .execute_batch(
            "CREATE TEMP TABLE observed_trust_updates(n INTEGER);
         CREATE TEMP TRIGGER observe_trust_updates AFTER UPDATE OF trust_state ON file_state_index
         BEGIN INSERT INTO observed_trust_updates VALUES(1); END;",
        )
        .unwrap();
    let captured = db.capture_generation(&app.app_id).unwrap();
    db.mark_dirty(&app.app_id, None, true).unwrap();
    let first = db.capture_generation(&app.app_id).unwrap();
    assert!(first > captured);
    db.mark_dirty_root(&app.app_id, "/home/user/Game", None, true)
        .unwrap();
    let second = db.capture_generation(&app.app_id).unwrap();
    assert!(
        second > first,
        "a repeated gap must still invalidate older captures"
    );
    db.mark_dirty(&app.app_id, None, true).unwrap();
    assert!(db.capture_generation(&app.app_id).unwrap() > second);
    let updates: i64 = db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM observed_trust_updates", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        updates, 2,
        "already invalid identities need no repeated UPDATE"
    );
    assert!(db
        .list_file_states(&app.app_id, None)
        .unwrap()
        .iter()
        .all(|state| state.trust == FileStateTrust::VerifyRequired));
    db.finish_backup_mutations(&app.app_id, &[], captured)
        .unwrap();
    assert!(db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|state| state.app_id == app.app_id && state.requires_reconciliation));
    assert!(db.list_dirty_roots(Some(&app.app_id)).unwrap()[0].requires_reconciliation);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
