use super::*;
use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

struct Fixture {
    db: StateDb,
    root: std::path::PathBuf,
    id: Uuid,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("noland-monitoring-{}", Uuid::new_v4()));
        let db = StateDb::open(&root.join("state.db")).unwrap();
        let id = Uuid::new_v4();
        db.upsert_operation(&OperationRecord {
            operation_id: id,
            kind: "backup".into(),
            app_id: None,
            state: "RECONCILING".into(),
            created_at: "2026-11-16T14:16:11Z".parse().unwrap(),
            updated_at: "2026-11-16T14:19:06Z".parse().unwrap(),
            last_error: None,
            detail_json: serde_json::json!({}),
        })
        .unwrap();
        let progress = OperationProgress::new("reconciling", 72192);
        db.set_operation_progress(id, Some(&progress)).unwrap();
        let mut pack =
            SyncJournalEntry::pending(id, "pack", ContentObjectKind::Pack, SyncDirection::Upload);
        pack.size = Some(1234);
        db.upsert_sync_journal_entry(&pack).unwrap();
        Self { db, root, id }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn publish(conn: &Connection, id: Uuid) {
    let progress = OperationProgress::new("completed", 186735);
    conn.execute(
        "UPDATE operations SET state='COMPLETED', updated_at=updated_at+1 WHERE operation_id=?1",
        [id.to_string()],
    )
    .unwrap();
    conn.execute(
        "UPDATE operation_runtime SET progress_json=?2 WHERE operation_id=?1",
        params![id.to_string(), serde_json::to_string(&progress).unwrap()],
    )
    .unwrap();
    conn.execute(
        "UPDATE sync_journal_items SET state='COMPLETED', bytes_transferred=size WHERE operation_id=?1",
        [id.to_string()],
    )
    .unwrap();
}

#[test]
fn committed_monitoring_finishes_while_writer_mutex_is_still_held() {
    let f = Fixture::new();
    let writer = f.db.lock().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = f.db.clone();
    let id = f.id;
    let thread = std::thread::spawn(move || {
        send.send(reader.operation_monitoring_snapshot(id)).unwrap();
    });
    let result = receive.recv_timeout(Duration::from_secs(3));
    // Release even if the assertion fails, so a regression cannot hang the test.
    drop(writer);
    thread.join().unwrap();
    let snapshot = result
        .expect("monitoring must finish BEFORE the writer is released")
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.operation.operation_id, f.id);
    assert_eq!(snapshot.operation.state, "RECONCILING");
    assert_eq!(snapshot.progress.unwrap().completed_units, 72192);
    assert_eq!(snapshot.pack_transfer.total_bytes, 1234);
}

#[test]
fn monitoring_remains_available_with_187282_states_and_repeated_real_invalidations() {
    let fixture = Fixture::new();
    let app = AppId::desktop("monitoring-scale");
    fixture
        .db
        .upsert_app(&AppIdentity::new(app.clone(), "Synthetic scale"))
        .unwrap();
    {
        let mut writer = fixture.db.lock().unwrap();
        let tx = writer.transaction().unwrap();
        tx.execute(
            "WITH RECURSIVE files(i) AS (
                 VALUES(1) UNION ALL SELECT i+1 FROM files WHERE i<187282
             ) INSERT INTO file_state_index(
                 app_id,logical_root,relative_path,canonical_path,file_type,
                 size,mtime_ns,trust_state,last_seen_at_ms
             ) SELECT ?1,'$HOME','synthetic/'||i,NULL,'file',4,1,'VERIFY_REQUIRED',1
               FROM files",
            [app.as_str()],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let generation = fixture.db.capture_generation(&app).unwrap();
    let writer = fixture.db.clone();
    let writer_app = app.clone();
    let start = Arc::new(Barrier::new(2));
    let writer_start = Arc::clone(&start);
    let (held, holding) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let writing = std::thread::spawn(move || {
        writer_start.wait();
        for _ in 0..64 {
            // Exercise the unchanged production transaction/barrier, not only
            // an equivalent hand-written no-op SQL benchmark.
            writer.mark_dirty(&writer_app, None, true).unwrap();
        }
        let _held = writer.lock().unwrap();
        held.send(()).unwrap();
        released.recv().unwrap();
    });
    let reader = fixture.db.clone();
    let id = fixture.id;
    let (observed, snapshots) = mpsc::channel();
    let reading = std::thread::spawn(move || {
        start.wait();
        for index in 0..65 {
            if index == 64 {
                // First overlap real invalidations, then force the last probe
                // to finish while the writer remains explicitly locked.
                holding.recv().unwrap();
            }
            let snapshot = reader.operation_monitoring_snapshot(id).unwrap().unwrap();
            assert_eq!(snapshot.operation.operation_id, id);
            assert_eq!(snapshot.operation.state, "RECONCILING");
            assert_eq!(snapshot.progress.unwrap().completed_units, 72192);
            assert_eq!(snapshot.pack_transfer.total_bytes, 1234);
        }
        observed.send(()).unwrap();
    });
    let result = snapshots.recv_timeout(Duration::from_secs(3));
    // Always release/join before asserting: a regression must fail, not hang.
    release.send(()).unwrap();
    writing.join().unwrap();
    reading.join().unwrap();
    result.expect("all committed status reads must finish before releasing the writer");
    assert_eq!(
        fixture.db.capture_generation(&app).unwrap(),
        generation + 64
    );
    let count: i64 = fixture
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM file_state_index WHERE app_id=?1 AND trust_state='VERIFY_REQUIRED'",
            [app.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 187282);
}

#[test]
fn uncommitted_state_progress_and_totals_never_leak_and_next_read_sees_commit() {
    let f = Fixture::new();
    let mut writer = f.db.lock().unwrap();
    let tx = writer.transaction().unwrap();
    publish(&tx, f.id);
    let before = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(before.operation.state, "RECONCILING");
    assert_eq!(before.progress.unwrap().completed_units, 72192);
    assert_eq!(before.pack_transfer.completed_items, 0);
    assert_eq!(before.pack_transfer.bytes_transferred, 0);
    tx.commit().unwrap();
    // Still hold the writer mutex: only the SQL commit should matter.
    let after = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(after.operation.state, "COMPLETED");
    assert_eq!(after.progress.unwrap().completed_units, 186735);
    assert_eq!(after.pack_transfer.completed_items, 1);
    assert_eq!(after.pack_transfer.bytes_transferred, 1234);
}

#[test]
fn one_read_transaction_cannot_mix_old_runtime_with_new_pack_totals() {
    let f = Fixture::new();
    let mut reader = f.db.browse_lock().unwrap();
    let tx = reader.transaction().unwrap();
    let snapshot = read_snapshot(&tx, f.id, || {
        let mut writer = f.db.lock().unwrap();
        let update = writer.transaction().unwrap();
        publish(&update, f.id);
        update.commit().unwrap();
    })
    .unwrap()
    .unwrap();
    assert_eq!(snapshot.operation.state, "RECONCILING");
    assert_eq!(snapshot.progress.unwrap().completed_units, 72192);
    assert_eq!(snapshot.pack_transfer.completed_items, 0);
    tx.commit().unwrap();
    drop(reader);
    let latest = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(latest.operation.state, "COMPLETED");
    assert_eq!(latest.pack_transfer.completed_items, 1);
}

#[test]
fn large_legacy_terminal_results_need_no_runtime_or_backfill_and_do_not_change_sql_rows() {
    let f = Fixture::new();
    let other = Uuid::new_v4();
    {
        let writer = f.db.lock().unwrap();
        writer.execute("DELETE FROM operation_runtime", []).unwrap();
        writer
            .execute("DELETE FROM operation_projections", [])
            .unwrap();
        writer
            .execute(
                "UPDATE operations SET state='COMPLETED', last_error=NULL, detail_json=?2
                 WHERE operation_id=?1",
                params![
                    f.id.to_string(),
                    serde_json::json!({
                        "result":"result-".repeat(150_000),
                        "retry":{"params":{"legacy_payload":"retry-".repeat(100_000)}},
                        "metrics":{"not_valid_operation_metrics":true},
                        "progress":{"operation_id":other,
                            "phase":"stale_previous_attempt","completed_units":999}
                    })
                    .to_string()
                ],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO operations(operation_id,kind,state,created_at,updated_at,detail_json)
             VALUES (?1,'backup','FAILED',1,2,?2)",
                params![other.to_string(), "{}"],
            )
            .unwrap();
        writer
            .execute(
                "UPDATE sync_journal_items SET state='COMPLETED', bytes_transferred=size
                 WHERE operation_id=?1",
                [f.id.to_string()],
            )
            .unwrap();
    }
    let foreign_pack = SyncJournalEntry::pending(
        other,
        "foreign-pack",
        ContentObjectKind::Pack,
        SyncDirection::Upload,
    );
    f.db.upsert_sync_journal_entry(&foreign_pack).unwrap();
    let writer_changes = f.db.lock().unwrap().total_changes();
    let reader_changes = f.db.browse_lock().unwrap().total_changes();
    let snapshot = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(snapshot.operation.operation_id, f.id);
    assert_eq!(snapshot.operation.state, "COMPLETED");
    assert!(snapshot.operation.last_error.is_none());
    assert!(snapshot.progress.is_none(), "never revive a prior attempt");
    assert_eq!(snapshot.pack_transfer.total_items, 1);
    assert_eq!(snapshot.pack_transfer.completed_items, 1);
    assert_eq!(snapshot.pack_transfer.total_bytes, 1234);
    assert_eq!(snapshot.pack_transfer.bytes_transferred, 1234);
    assert!(f
        .db
        .operation_monitoring_snapshot(Uuid::new_v4())
        .unwrap()
        .is_none());
    let writer = f.db.lock().unwrap();
    assert_eq!(writer.total_changes(), writer_changes);
    for table in ["operation_projections", "operation_runtime"] {
        let count: i64 = writer
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "monitoring must never backfill {table}");
    }
    drop(writer);
    assert_eq!(f.db.browse_lock().unwrap().total_changes(), reader_changes);
    assert!(f
        .db
        .browse_lock()
        .unwrap()
        .execute("DELETE FROM operations", [])
        .is_err());
}

#[test]
fn results_and_diagnostic_metrics_are_not_read_or_parsed_for_progress() {
    let f = Fixture::new();
    f.db.lock()
        .unwrap()
        .execute(
            "UPDATE operations SET detail_json=?2 WHERE operation_id=?1",
            params![
                f.id.to_string(),
                format!("not even JSON {}", "x".repeat(4 * 1024 * 1024))
            ],
        )
        .unwrap();
    for metrics in ["{".to_string(), "x".repeat(1024 * 1024)] {
        f.db.lock()
            .unwrap()
            .execute(
                "UPDATE operation_runtime SET metrics_json=?2 WHERE operation_id=?1",
                params![f.id.to_string(), metrics],
            )
            .unwrap();
        let snapshot = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
        assert_eq!(snapshot.operation.detail_json, serde_json::json!({}));
        assert_eq!(snapshot.progress.unwrap().completed_units, 72192);
    }
}

#[test]
fn invalid_or_oversized_progress_is_an_error_not_empty_progress() {
    let mut oversized = OperationProgress::new("reconciling", 72192);
    oversized.message = Some("x".repeat(MAX_PROGRESS_BYTES as usize));
    for value in ["{".to_string(), serde_json::to_string(&oversized).unwrap()] {
        let f = Fixture::new();
        f.db.lock()
            .unwrap()
            .execute(
                "UPDATE operation_runtime SET progress_json=?2 WHERE operation_id=?1",
                params![f.id.to_string(), value],
            )
            .unwrap();
        assert!(f.db.operation_monitoring_snapshot(f.id).is_err());
    }
}

#[test]
fn database_errors_and_writer_or_reader_poison_do_not_manufacture_status() {
    let f = Fixture::new();
    f.db.lock()
        .unwrap()
        .execute("DROP TABLE operation_runtime", [])
        .unwrap();
    assert!(f.db.operation_monitoring_snapshot(f.id).is_err());
    for poison_writer in [true, false] {
        let f = Fixture::new();
        let db = f.db.clone();
        assert!(std::thread::spawn(move || {
            let _guard = if poison_writer {
                db.lock().unwrap()
            } else {
                db.browse_lock().unwrap()
            };
            panic!("intentional monitoring mutex poison");
        })
        .join()
        .is_err());
        assert!(f.db.operation_monitoring_snapshot(f.id).is_err());
        assert!(f.db.unfinished_operation_count_for_health().is_err());
    }
}

#[test]
fn terminal_cleanup_warning_stays_with_its_committed_state_and_operation() {
    let f = Fixture::new();
    let mut progress = OperationProgress::new("completed", 186735);
    progress.detail_json = serde_json::json!({
        "repository_admission":{"status":"CLEANUP_REQUIRED","cleanup_required":true},
        "backup_finalization":{"version":1,"stage":"cleanup_required"},
    });
    let mut writer = f.db.lock().unwrap();
    let tx = writer.transaction().unwrap();
    publish(&tx, f.id);
    tx.execute(
        "UPDATE operation_runtime SET progress_json=?2 WHERE operation_id=?1",
        params![f.id.to_string(), serde_json::to_string(&progress).unwrap()],
    )
    .unwrap();
    tx.commit().unwrap();
    let snapshot = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(snapshot.operation.operation_id, f.id);
    assert_eq!(snapshot.operation.state, "COMPLETED");
    assert!(snapshot.operation.last_error.is_none());
    assert_eq!(snapshot.progress.unwrap().detail_json, progress.detail_json);
}

#[test]
fn in_memory_fallback_distinguishes_missing_and_explicitly_cleared_runtime_data() {
    let f = Fixture::new();
    let operation = f.db.get_operation(f.id).unwrap().unwrap();
    let db = StateDb::open_in_memory().unwrap();
    db.upsert_operation(&operation).unwrap();
    let empty = db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(empty.operation.state, operation.state);
    assert!(empty.progress.is_none());
    assert_eq!(empty.pack_transfer, SyncJournalSummary::default());
    let progress = OperationProgress::new("reconciling", 72192);
    db.set_operation_progress(f.id, Some(&progress)).unwrap();
    let confirmed = db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert_eq!(confirmed.progress.unwrap().completed_units, 72192);
    db.set_operation_progress(f.id, None).unwrap();
    let cleared = db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
    assert!(cleared.progress.is_none());
}

#[test]
fn valid_progress_through_four_mib_is_preserved_without_the_old_smaller_bound() {
    let f = Fixture::new();
    for size in [1024 * 1024, MAX_PROGRESS_BYTES as usize] {
        let mut progress = OperationProgress::new("reconciling", 72192);
        progress.message = Some(String::new());
        let overhead = serde_json::to_vec(&progress).unwrap().len();
        progress.message = Some("x".repeat(size - overhead));
        assert_eq!(serde_json::to_vec(&progress).unwrap().len(), size);
        f.db.set_operation_progress(f.id, Some(&progress)).unwrap();
        let snapshot = f.db.operation_monitoring_snapshot(f.id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(snapshot.progress.unwrap()).unwrap(),
            serde_json::to_value(progress).unwrap()
        );
    }
}

#[test]
fn embedded_progress_identity_must_match_the_requested_operation() {
    let f = Fixture::new();
    for in_detail in [false, true] {
        for embedded in [
            serde_json::json!(f.id),
            serde_json::json!(Uuid::new_v4()),
            serde_json::json!("invalid-id"),
            serde_json::json!(17),
        ] {
            let mut progress =
                serde_json::to_value(OperationProgress::new("reconciling", 72192)).unwrap();
            let envelope = if in_detail {
                &mut progress["detail_json"]
            } else {
                &mut progress
            };
            envelope["operation_id"] = embedded.clone();
            f.db.lock()
                .unwrap()
                .execute(
                    "UPDATE operation_runtime SET progress_json=?2 WHERE operation_id=?1",
                    params![f.id.to_string(), progress.to_string()],
                )
                .unwrap();
            let result = f.db.operation_monitoring_snapshot(f.id);
            if embedded == serde_json::json!(f.id) {
                assert_eq!(
                    result.unwrap().unwrap().progress.unwrap().completed_units,
                    72192
                );
            } else {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("identity mismatch"));
            }
        }
    }
}
