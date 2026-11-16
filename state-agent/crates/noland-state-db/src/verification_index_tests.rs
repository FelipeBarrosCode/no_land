//! Synthetic DB-only coverage: no runtime payloads or remote providers.
use std::path::PathBuf;

use chrono::Utc;
use noland_state_core::*;
use rusqlite::{params, types::Value, Connection, StatementStatus};
use uuid::Uuid;

use crate::{schema::*, StateDb, SCHEMA_VERSION};

const INDEX: &str = "idx_file_state_needs_verification";
// The unchanged V6 statements in mark_dirty and mark_dirty_root.
const INVALIDATE: &str = "UPDATE file_state_index SET trust_state='VERIFY_REQUIRED'
    WHERE app_id=?1 AND trust_state<>'VERIFY_REQUIRED'";

fn create_v6(conn: &mut Connection) {
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = conn.transaction().unwrap();
    for migration in [
        MIGRATION_V1,
        MIGRATION_V2,
        MIGRATION_V3,
        MIGRATION_V4,
        MIGRATION_V5,
        MIGRATION_V6,
    ] {
        tx.execute_batch(migration).unwrap();
    }
    for version in 1..=6 {
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            params![version, 1000 + version],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

struct LegacyFixture {
    root: PathBuf,
    path: PathBuf,
}

impl LegacyFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("verification-migration-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.sqlite");
        let mut conn = Connection::open(&path).unwrap();
        create_v6(&mut conn);
        conn.execute_batch(
            "INSERT INTO apps(app_id, display_name, identity_confidence, created_at, updated_at)
                 VALUES ('desktop:legacy', 'Synthetic legacy app', 1, 1, 1);
             INSERT INTO paths(path_id, canonical_path, logical_root, relative_path, file_type)
                 VALUES (1, '/synthetic/save', '$HOME', 'save', 'file');
             INSERT INTO path_associations VALUES
                 (1, 'desktop:legacy', 1, 'PERSISTENT_STATE', 'USER_STATE', '[]', 1, 1);
             INSERT INTO known_roots VALUES
                 ('desktop:legacy', 'runtime-binding-required', '/synthetic/executable');
             INSERT INTO capture_generations VALUES ('desktop:legacy', 41);
             INSERT INTO dirty_apps VALUES ('desktop:legacy', 1, 2, 1);
             INSERT INTO dirty_paths VALUES ('desktop:legacy', 1);
             INSERT INTO dirty_roots VALUES
                 ('desktop:legacy', '/synthetic', '$HOME', 1, 2, 3, 1);
             INSERT INTO file_state_index(
                 app_id, logical_root, relative_path, canonical_path, file_type,
                 size, mtime_ns, content_hash, trust_state, last_seen_at_ms)
                 VALUES ('desktop:legacy', '$HOME', 'save', '/synthetic/save',
                         'file', 12, 34, 'synthetic-hash', 'TRUSTED', 56);
             INSERT INTO operations(operation_id, kind, state, created_at, updated_at, detail_json)
                 VALUES ('synthetic-operation', 'backup', 'COMPLETED', 1, 2,
                         '{\"completion\":{\"durable\":true}}');
             INSERT INTO immutable_metadata_cache VALUES
                 ('synthetic-binding', 'synthetic/manifest.enc', X'010203', 'synthetic-receipt', 1);",
        )
        .unwrap();
        Self { root, path }
    }
}

impl Drop for LegacyFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn index_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
        [INDEX],
        |row| row.get(0),
    )
    .unwrap()
}

fn versions(conn: &Connection) -> Vec<(i64, i64)> {
    conn.prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn schema_without_new_index(conn: &Connection) -> Vec<(String, String, String)> {
    conn.prepare(
        "SELECT type, name, sql FROM sqlite_master
         WHERE name NOT LIKE 'sqlite_%' AND name<>?1 ORDER BY type, name",
    )
    .unwrap()
    .query_map([INDEX], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

// Compare every user-table value, including learned/capture generations and
// cached ciphertext. The sole new migration receipt is checked separately.
fn user_data(conn: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
    let names = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table'
             AND name NOT LIKE 'sqlite_%' AND name<>'schema_migrations' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT * FROM \"{}\" ORDER BY rowid",
                    name.replace('"', "\"\"")
                ))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get::<_, Value>(column))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

#[test]
fn v6_upgrade_adds_only_the_partial_index_and_receipt_and_is_idempotent() {
    let f = LegacyFixture::new();
    let conn = Connection::open(&f.path).unwrap();
    assert!(!index_exists(&conn));
    let before_schema = schema_without_new_index(&conn);
    let before_data = user_data(&conn);
    let before_versions = versions(&conn);
    drop(conn);

    let mut index_rootpage = None;
    let mut upgraded_versions = None;
    for _ in 0..2 {
        let db = StateDb::open(&f.path).unwrap();
        let conn = db.lock().unwrap();
        assert!(index_exists(&conn));
        assert_eq!(schema_without_new_index(&conn), before_schema);
        assert_eq!(user_data(&conn), before_data);
        let current_versions = versions(&conn);
        assert_eq!(&current_versions[..6], before_versions);
        assert_eq!(current_versions.len(), 7);
        assert_eq!(current_versions[6].0, SCHEMA_VERSION);
        let rootpage: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name=?1",
                [INDEX],
                |row| row.get(0),
            )
            .unwrap();
        let partial: bool = conn
            .query_row(
                "SELECT partial FROM pragma_index_list('file_state_index') WHERE name=?1",
                [INDEX],
                |row| row.get(0),
            )
            .unwrap();
        assert!(partial);
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_index_info(?1)")
            .unwrap()
            .query_map([INDEX], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(columns, ["app_id"]);
        if let Some(previous) = &upgraded_versions {
            assert_eq!(&current_versions, previous);
            assert_eq!(
                Some(rootpage),
                index_rootpage,
                "reopen must not rebuild the index"
            );
        }
        upgraded_versions = Some(current_versions);
        index_rootpage = Some(rootpage);
    }
}

#[test]
fn uncommitted_v7_index_and_receipt_roll_back_when_the_connection_closes() {
    let f = LegacyFixture::new();
    let conn = Connection::open(&f.path).unwrap();
    let before = user_data(&conn);
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    conn.execute_batch(MIGRATION_V7).unwrap();
    conn.execute("INSERT INTO schema_migrations VALUES (7, 7000)", [])
        .unwrap();
    assert!(index_exists(&conn));
    // An abandoned connection leaves neither half of the upgrade durable.
    drop(conn);

    let conn = Connection::open(&f.path).unwrap();
    assert!(!index_exists(&conn));
    assert_eq!(versions(&conn).len(), 6);
    assert_eq!(user_data(&conn), before);
    drop(conn);
    let db = StateDb::open(&f.path).unwrap();
    assert!(index_exists(&db.lock().unwrap()));
    assert_eq!(db.integrity_check().unwrap(), "ok");
}

#[test]
fn failure_recording_v7_rolls_back_the_production_migration_and_allows_retry() {
    let f = LegacyFixture::new();
    let conn = Connection::open(&f.path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fixture_reject_v7 BEFORE INSERT ON schema_migrations
         WHEN NEW.version=7 BEGIN SELECT RAISE(ABORT, 'synthetic V7 receipt failure'); END;",
    )
    .unwrap();
    let before = user_data(&conn);
    let before_schema = schema_without_new_index(&conn);
    drop(conn);

    let error = match StateDb::open(&f.path) {
        Err(error) => error,
        Ok(_) => panic!("fixture must abort the migration after index creation"),
    };
    assert!(error.to_string().contains("synthetic V7 receipt failure"));
    let conn = Connection::open(&f.path).unwrap();
    assert!(!index_exists(&conn));
    assert_eq!(versions(&conn).len(), 6);
    assert_eq!(schema_without_new_index(&conn), before_schema);
    assert_eq!(user_data(&conn), before);
    conn.execute_batch("DROP TRIGGER fixture_reject_v7")
        .unwrap();
    drop(conn);
    let db = StateDb::open(&f.path).unwrap();
    assert!(index_exists(&db.lock().unwrap()));
    assert_eq!(user_data(&db.lock().unwrap()), before);
}

#[test]
fn v6_sql_remains_read_write_compatible_after_the_additive_upgrade() {
    let f = LegacyFixture::new();
    let db = StateDb::open(&f.path).unwrap();
    let conn = db.lock().unwrap();
    let app = "desktop:legacy";
    let hash: String = conn
        .query_row(
            "SELECT content_hash FROM file_state_index
             WHERE app_id=?1 AND logical_root='$HOME' AND relative_path='save'",
            [app],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hash, "synthetic-hash");
    // Older unconditional invalidation SQL still works; the index is not an
    // authority filter and imposes no new constraint on old writers.
    assert_eq!(
        conn.execute(
            "UPDATE file_state_index SET trust_state='VERIFY_REQUIRED' WHERE app_id=?1",
            [app],
        )
        .unwrap(),
        1
    );
    conn.execute(
        "UPDATE file_state_index SET trust_state='TRUSTED'
         WHERE app_id=?1 AND logical_root='$HOME' AND relative_path='save'",
        [app],
    )
    .unwrap();
    assert_eq!(conn.execute(INVALIDATE, [app]).unwrap(), 1);
    assert_eq!(conn.execute(INVALIDATE, [app]).unwrap(), 0);
    assert!(index_exists(&conn));
    assert_eq!(versions(&conn).last().unwrap().0, 7);
}

fn noop_update_work(conn: &Connection, app: &str) -> i32 {
    let mut stmt = conn.prepare(INVALIDATE).unwrap();
    assert_eq!(stmt.execute([app]).unwrap(), 0);
    stmt.get_status(StatementStatus::VmStep)
}

#[test]
fn no_match_update_work_stays_bounded_from_4096_to_187282_states() {
    let mut indexed_work = Vec::new();
    let mut unindexed_work = Vec::new();
    for count in [4096, 187282] {
        let mut conn = Connection::open_in_memory().unwrap();
        create_v6(&mut conn);
        let app = "desktop:verification-perf";
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO apps(app_id, display_name, identity_confidence, created_at, updated_at)
             VALUES (?1, 'Synthetic performance app', 1, 1, 1)",
            [app],
        )
        .unwrap();
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO file_state_index(
                         app_id, logical_root, relative_path, file_type, size,
                         mtime_ns, trust_state, last_seen_at_ms)
                     VALUES (?1, '$HOME', ?2, 'file', 1, 1, 'VERIFY_REQUIRED', 1)",
                )
                .unwrap();
            // One bulk transaction per cardinality; no files or per-row commits.
            for index in 0..count {
                insert
                    .execute(params![app, format!("synthetic-{index:08}")])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        let unindexed = noop_update_work(&conn, app);
        conn.execute_batch(MIGRATION_V7).unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {INVALIDATE}"))
            .unwrap()
            .query_map([app], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(plan.iter().any(|detail| detail.contains(INDEX)), "{plan:?}");
        let indexed = noop_update_work(&conn, app);
        eprintln!(
            "verification no-match UPDATE: states={count}, V6 VM steps={unindexed}, V7 VM steps={indexed}, plan={plan:?}"
        );
        unindexed_work.push(unindexed);
        indexed_work.push(indexed);
    }
    assert!(indexed_work[0] > 0);
    assert!(
        indexed_work[1] <= indexed_work[0].saturating_mul(2),
        "zero-match index lookup grew with file count: {indexed_work:?}"
    );
    assert!(
        unindexed_work[1] > unindexed_work[0].saturating_mul(10),
        "control must expose the old app-wide scan: {unindexed_work:?}"
    );
}

fn state(app: &AppId, name: &str, trust: FileStateTrust) -> FileStateRecord {
    FileStateRecord {
        app_id: app.clone(),
        logical_root: "$HOME".into(),
        relative_path: name.into(),
        canonical_path: Some(format!("/synthetic/{name}")),
        file_type: FileType::File,
        size: 12,
        mtime_ns: 34,
        inode: Some(1),
        mount_id: Some(2),
        mode: Some(0o100600),
        content_hash: Some("synthetic-hash".into()),
        trust,
        last_seen_at: Utc::now(),
        last_hashed_at: Some(Utc::now()),
    }
}

#[test]
fn gaps_invalidate_real_and_new_candidates_but_keep_zero_match_barriers_and_other_apps() {
    for initially_empty in [false, true] {
        let db = StateDb::open_in_memory().unwrap();
        let app = AppId::desktop("verification-selected");
        let other = AppId::desktop("verification-other");
        for id in [&app, &other] {
            db.upsert_app(&AppIdentity::new(id.clone(), "Synthetic app"))
                .unwrap();
        }
        let trusts = [
            FileStateTrust::Trusted,
            FileStateTrust::Dirty,
            FileStateTrust::VerifyRequired,
            FileStateTrust::Missing,
        ];
        for id in [&app, &other] {
            if id == &app && initially_empty {
                continue;
            }
            let states = trusts
                .iter()
                .enumerate()
                .map(|(index, trust)| state(id, &format!("save-{index}"), *trust))
                .collect::<Vec<_>>();
            db.upsert_file_states(&states).unwrap();
        }
        let other_before =
            serde_json::to_value(db.list_file_states(&other, None).unwrap()).unwrap();
        let path_id = db.upsert_path("/synthetic/save-0").unwrap();
        let captured = db.capture_generation(&app).unwrap();
        db.mark_dirty(&app, Some(path_id), true).unwrap();
        assert_eq!(db.capture_generation(&app).unwrap(), captured + 1);
        let states = db.list_file_states(&app, None).unwrap();
        assert_eq!(states.len(), if initially_empty { 0 } else { 4 });
        assert!(states
            .iter()
            .all(|state| state.trust == FileStateTrust::VerifyRequired));
        db.mark_dirty_root(&app, "/synthetic", Some("$HOME"), true)
            .unwrap();
        assert_eq!(db.capture_generation(&app).unwrap(), captured + 2);

        // A new capture can restore trust after a prior gap. Neither an earlier
        // dirty flag nor an empty partial index may hide these new candidates.
        db.upsert_file_states(&[
            state(&app, "save-0", FileStateTrust::Trusted),
            state(&app, "save-1", FileStateTrust::Dirty),
        ])
        .unwrap();
        db.mark_dirty_root(&app, "/synthetic", Some("$HOME"), true)
            .unwrap();
        assert_eq!(db.capture_generation(&app).unwrap(), captured + 3);
        assert!(db
            .list_file_states(&app, None)
            .unwrap()
            .iter()
            .all(|state| state.trust == FileStateTrust::VerifyRequired));
        db.mark_dirty(&app, Some(path_id), true).unwrap();
        assert_eq!(db.capture_generation(&app).unwrap(), captured + 4);
        db.finish_backup_mutations(&app, &[], captured).unwrap();
        let states = db.list_file_states(&app, None).unwrap();
        assert_eq!(states.len(), if initially_empty { 2 } else { 4 });
        assert!(states.iter().all(|state| {
            state.trust == FileStateTrust::VerifyRequired
                && state.content_hash.as_deref() == Some("synthetic-hash")
                && state.size == 12
                && state.mtime_ns == 34
        }));
        assert_eq!(db.capture_generation(&app).unwrap(), captured + 4);
        let dirty = db.list_dirty_apps().unwrap();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].app_id, app);
        assert!(dirty[0].requires_reconciliation);
        assert_eq!(dirty[0].dirty_paths, [path_id]);
        let roots = db.list_dirty_roots(Some(&app)).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].canonical_root, "/synthetic");
        assert_eq!(roots[0].logical_root.as_deref(), Some("$HOME"));
        assert_eq!(roots[0].mutation_count, 2);
        assert!(roots[0].requires_reconciliation);
        assert_eq!(
            serde_json::to_value(db.list_file_states(&other, None).unwrap()).unwrap(),
            other_before
        );
        assert_eq!(db.capture_generation(&other).unwrap(), 0);
        assert!(db.list_dirty_roots(Some(&other)).unwrap().is_empty());
    }
}
