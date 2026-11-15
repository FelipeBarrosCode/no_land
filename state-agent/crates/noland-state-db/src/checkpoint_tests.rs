use super::*;
use crate::schema::{MIGRATION_V1, MIGRATION_V2, MIGRATION_V3};

fn fixture() -> (StateDb, AppIdentity, PathAssociation) {
    let db = StateDb::open_in_memory().unwrap();
    let app = AppIdentity::new(AppId::desktop("checkpoint"), "Checkpoint");
    db.upsert_app(&app).unwrap();
    let path_id = db.upsert_path("/home/user/save").unwrap();
    let association = PathAssociation {
        app_id: app.app_id.clone(),
        path_id,
        confidence: 0.8,
        persistence_class: PersistenceClass::PersistentState,
        semantic_role: SemanticRole::UserState,
        evidence: vec![Evidence::new(EvidenceKind::KnownUserStateRoot)],
        first_seen_at: Utc::now(),
        last_seen_at: Utc::now(),
    };
    db.upsert_association(&association).unwrap();
    (db, app, association)
}

fn content(db: &StateDb) -> serde_json::Value {
    let snapshot = db.learned_checkpoint(Uuid::nil(), "image").unwrap();
    let mut value = serde_json::to_value(snapshot).unwrap();
    value.as_object_mut().unwrap().remove("checkpoint_id");
    value.as_object_mut().unwrap().remove("created_at");
    value
}

fn mutation(db: &StateDb, changes_content: bool, write: impl FnOnce()) {
    let before = db.learned_checkpoint_generation().unwrap();
    let snapshot = content(db);
    write();
    assert_eq!(
        before != db.learned_checkpoint_generation().unwrap(),
        changes_content
    );
    assert_eq!(snapshot != content(db), changes_content);
}

#[test]
fn semantic_generation_tracks_evidence_kinds_and_every_association_writer() {
    let (db, app, mut association) = fixture();
    mutation(&db, false, || {
        db.upsert_app(&app).unwrap();
        association.last_seen_at += chrono::Duration::seconds(1);
        association.evidence[0].observed_at += chrono::Duration::seconds(1);
        association.evidence[0].detail = Some("new observation detail".into());
        association.evidence[0].session_id = Some(Uuid::new_v4());
        db.upsert_association(&association).unwrap();
        db.mark_dirty(&app.app_id, Some(association.path_id), true)
            .unwrap();
    });
    mutation(&db, true, || {
        association
            .evidence
            .push(Evidence::new(EvidenceKind::DirectCgroupWrite));
        db.upsert_association(&association).unwrap();
    });
    mutation(&db, true, || {
        association.evidence.reverse();
        db.upsert_association(&association).unwrap();
    });
    mutation(&db, true, || {
        association.evidence.push(association.evidence[0].clone());
        db.upsert_association(&association).unwrap();
    });
    mutation(&db, true, || {
        assert_eq!(
            db.update_classifications(&[(
                association.clone(),
                PersistenceClass::SharedState,
                SemanticRole::AppContent
            )])
            .unwrap(),
            1
        );
    });
    mutation(&db, false, || {
        // A stale classification snapshot loses the CAS; it cannot dirty coverage.
        assert_eq!(
            db.update_classifications(&[(
                association.clone(),
                PersistenceClass::PersistentState,
                SemanticRole::UserState
            )])
            .unwrap(),
            0
        );
    });
    let (mut path, mut current) = db.associations_for_app(&app.app_id).unwrap().pop().unwrap();
    mutation(&db, true, || {
        let old = current.clone();
        current.confidence = 0.9;
        db.upsert_scanned_paths(&[(path.clone(), current.clone(), Some(old))])
            .unwrap();
    });
    mutation(&db, false, || {
        path.size = Some(99);
        path.last_scanned_at = Some(100);
        db.upsert_scanned_paths(&[(path.clone(), current.clone(), Some(current.clone()))])
            .unwrap();
    });
    mutation(&db, true, || {
        path.logical_root = Some("$HOME".into());
        path.relative_path = Some("save".into());
        db.update_path_meta(path.path_id, &path).unwrap();
    });
    mutation(&db, true, || {
        db.upsert_restored_path_associations(
            &app.app_id,
            &[crate::RestoredPathAssociationInput {
                canonical_path: path.canonical_path.clone(),
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
            }],
        )
        .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute("DELETE FROM path_associations", [])
            .unwrap();
    });
    mutation(&db, false, || {
        // Unassociated paths are not checkpoint content.
        path.logical_root = Some("other".into());
        db.update_path_meta(path.path_id, &path).unwrap();
    });
}

#[test]
fn semantic_generation_covers_all_app_columns_roots_and_ignores_progress() {
    let (db, app, _) = fixture();
    for sql in [
        "UPDATE apps SET display_name='renamed'",
        "UPDATE apps SET canonical_executable='/opt/game'",
        "UPDATE apps SET desktop_entry_id='game.desktop'",
        "UPDATE apps SET steam_app_id=77",
        "UPDATE apps SET launcher_kind='steam'",
        "UPDATE apps SET aliases_json='[\"game\"]'",
        "UPDATE apps SET identity_confidence=0.95",
        "UPDATE apps SET icon_path='/icons/game.png'",
    ] {
        mutation(&db, true, || {
            db.lock().unwrap().execute(sql, []).unwrap();
        });
    }
    mutation(&db, false, || {
        db.lock()
            .unwrap()
            .execute("UPDATE apps SET updated_at=updated_at+1", [])
            .unwrap();
        let id = Uuid::new_v4();
        db.upsert_operation(&OperationRecord {
            operation_id: id,
            kind: "backup".into(),
            app_id: Some(app.app_id.clone()),
            state: "RUNNING".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({}),
        })
        .unwrap();
        db.set_operation_progress(id, Some(&OperationProgress::new("upload", 2)))
            .unwrap();
        db.mark_dirty_root(&app.app_id, "/home/user", Some("$HOME"), true)
            .unwrap();
    });
    mutation(&db, true, || {
        db.add_known_root(&app.app_id, "state", "/home/user")
            .unwrap();
    });
    mutation(&db, false, || {
        db.add_known_root(&app.app_id, "state", "/home/user")
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute("UPDATE known_roots SET kind='content'", [])
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute("DELETE FROM known_roots", [])
            .unwrap();
    });
    mutation(&db, true, || {
        db.upsert_app(&AppIdentity::new(AppId::desktop("other"), "Other"))
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute(
                "DELETE FROM apps WHERE app_id=?1",
                [AppId::desktop("other").as_str()],
            )
            .unwrap();
    });
}

fn insert_commit(db: &StateDb, app: &AppId, id: Uuid, time: i64, state: &str) {
    db.lock().unwrap().execute(
        "INSERT INTO bundle_commits (commit_id, app_id, bundle_id, manifest_hash, state, created_at, committed_at)
         VALUES (?1,?2,?3,'hash',?4,0,?5)",
        params![id.to_string(), app.as_str(), Uuid::new_v4().to_string(), state, time],
    ).unwrap();
}

#[test]
fn only_changes_to_latest_committed_references_advance_generation() {
    let (db, app, _) = fixture();
    let first = Uuid::new_v4();
    let older = Uuid::new_v4();
    let tied = Uuid::new_v4();
    mutation(&db, true, || {
        insert_commit(&db, &app.app_id, first, 20, "COMMITTED");
    });
    mutation(&db, false, || {
        insert_commit(&db, &app.app_id, older, 10, "COMMITTED");
    });
    mutation(&db, false, || {
        insert_commit(&db, &app.app_id, tied, 20, "PENDING");
    });
    mutation(&db, false, || {
        db.lock()
            .unwrap()
            .execute(
                "UPDATE bundle_commits SET cloud_path='elsewhere', manifest_hash='other'",
                [],
            )
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute(
                "UPDATE bundle_commits SET state='COMMITTED' WHERE commit_id=?1",
                [tied.to_string()],
            )
            .unwrap();
    });
    assert_eq!(
        db.learned_checkpoint(Uuid::nil(), "image")
            .unwrap()
            .latest_bundle_refs[0]
            .commit_id,
        tied
    );
    mutation(&db, false, || {
        db.lock()
            .unwrap()
            .execute(
                "UPDATE bundle_commits SET committed_at=21 WHERE commit_id=?1",
                [tied.to_string()],
            )
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute(
                "UPDATE bundle_commits SET bundle_id=?2 WHERE commit_id=?1",
                params![tied.to_string(), Uuid::new_v4().to_string()],
            )
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute(
                "DELETE FROM bundle_commits WHERE commit_id=?1",
                [tied.to_string()],
            )
            .unwrap();
    });
    assert_eq!(
        db.learned_checkpoint(Uuid::nil(), "image")
            .unwrap()
            .latest_bundle_refs[0]
            .commit_id,
        first
    );
    mutation(&db, false, || {
        db.lock()
            .unwrap()
            .execute(
                "DELETE FROM bundle_commits WHERE commit_id=?1",
                [older.to_string()],
            )
            .unwrap();
    });
    mutation(&db, true, || {
        db.lock()
            .unwrap()
            .execute("UPDATE bundle_commits SET state='FAILED'", [])
            .unwrap();
    });
    assert!(db
        .learned_checkpoint(Uuid::nil(), "image")
        .unwrap()
        .latest_bundle_refs
        .is_empty());
}

#[test]
fn generation_and_projection_roll_back_with_failed_or_aborted_writes() {
    let (db, app, _) = fixture();
    let before = db.learned_checkpoint_generation().unwrap();
    let snapshot = content(&db);
    {
        let mut conn = db.lock().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute("UPDATE apps SET display_name='uncommitted'", [])
            .unwrap();
        tx.execute(
            "INSERT INTO bundle_commits VALUES (?1,?2,?3,'hash',NULL,'COMMITTED',1,1)",
            params![
                Uuid::new_v4().to_string(),
                app.app_id.as_str(),
                Uuid::new_v4().to_string()
            ],
        )
        .unwrap();
        assert!(read_generation(&tx).unwrap().generation > before.generation);
        // Dropping the transaction rolls back rows, projection and counter.
    }
    assert_eq!(db.learned_checkpoint_generation().unwrap(), before);
    assert_eq!(content(&db), snapshot);
    assert!(db.lock().unwrap().execute("INSERT INTO path_associations VALUES (999,'unknown',1,'PERSISTENT_STATE','USER_STATE','[]',0,0)", []).is_err());
    assert_eq!(db.learned_checkpoint_generation().unwrap(), before);
}

#[test]
fn unchanged_fast_path_does_not_even_prepare_snapshot_queries() {
    let (db, _, _) = fixture();
    let generation = db.learned_checkpoint_generation().unwrap();
    // A schema sentinel makes any attempt to read a full snapshot fail. Schema
    // changes are test-only; ordinary row mutations are covered by the triggers.
    db.lock()
        .unwrap()
        .execute("ALTER TABLE apps RENAME TO hidden_apps", [])
        .unwrap();
    assert!(db
        .learned_checkpoint_if_changed(Uuid::nil(), "image", Some(&generation))
        .unwrap()
        .is_none());
    assert!(db.learned_checkpoint(Uuid::nil(), "image").is_err());
}

#[test]
fn version_three_upgrade_seeds_latest_once_and_persists_generation_across_reopen() {
    let path = std::env::temp_dir().join(format!("noland-generation-{}.sqlite", Uuid::new_v4()));
    let app = AppIdentity::new(AppId::desktop("upgrade"), "Upgrade");
    let commit = Uuid::new_v4();
    {
        let conn = Connection::open(&path).unwrap();
        for sql in [MIGRATION_V1, MIGRATION_V2, MIGRATION_V3] {
            conn.execute_batch(sql).unwrap();
        }
        for version in 1..=3 {
            conn.execute("INSERT INTO schema_migrations VALUES (?1,0)", [version])
                .unwrap();
        }
        conn.execute("INSERT INTO apps (app_id,display_name,identity_confidence,created_at,updated_at) VALUES (?1,'Upgrade',0.9,0,0)", [app.app_id.as_str()]).unwrap();
        conn.execute(
            "INSERT INTO bundle_commits VALUES (?1,?2,?3,'hash',NULL,'COMMITTED',1,1)",
            params![
                commit.to_string(),
                app.app_id.as_str(),
                Uuid::new_v4().to_string()
            ],
        )
        .unwrap();
    }
    let db = StateDb::open(&path).unwrap();
    assert_eq!(
        db.lock()
            .unwrap()
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        crate::SCHEMA_VERSION
    );
    assert_eq!(
        db.learned_checkpoint(Uuid::nil(), "image")
            .unwrap()
            .latest_bundle_refs[0]
            .commit_id,
        commit
    );
    let (captured, snapshot) = db
        .learned_checkpoint_versioned(Uuid::nil(), "image")
        .unwrap();
    db.add_known_root(&app.app_id, "state", "/home/user")
        .unwrap();
    assert!(snapshot.known_roots.is_empty());
    let newer = db.learned_checkpoint_generation().unwrap();
    assert_ne!(captured, newer);
    drop(db);
    let db = StateDb::open(&path).unwrap();
    assert_eq!(db.learned_checkpoint_generation().unwrap(), newer);
    assert!(db
        .learned_checkpoint_if_changed(Uuid::nil(), "image", Some(&captured))
        .unwrap()
        .is_some());
    assert!(db
        .learned_checkpoint_if_changed(Uuid::nil(), "image", Some(&newer))
        .unwrap()
        .is_none());
    assert_ne!(
        StateDb::open_in_memory()
            .unwrap()
            .learned_checkpoint_generation()
            .unwrap()
            .database_id,
        newer.database_id
    );
    drop(db);
    std::fs::remove_file(&path).unwrap();
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
}
