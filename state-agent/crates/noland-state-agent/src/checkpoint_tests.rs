use super::*;
use crate::AgentConfig;
use chrono::Utc;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    agent: StateAgent,
    app: AppIdentity,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("noland-checkpoints-{}", Uuid::new_v4()));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test"), "Test");
        agent.db.upsert_app(&app).unwrap();
        Self { root, agent, app }
    }
    fn manifest(&self) -> PeriodicManifest {
        read_manifest(&self.agent.config.paths.checkpoints).unwrap()
    }
    fn change(&mut self, n: usize) {
        self.app.display_name = format!("Test {n}");
        self.agent.db.upsert_app(&self.app).unwrap();
        maybe_checkpoint(&self.agent).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn unchanged_state_coalesces_across_restarts_and_observation_updates() {
    let fixture = Fixture::new();
    let path_id = fixture.agent.db.upsert_path("/home/gamer/save").unwrap();
    let mut association = PathAssociation {
        app_id: fixture.app.app_id.clone(),
        path_id,
        confidence: 0.9,
        persistence_class: PersistenceClass::PersistentState,
        semantic_role: SemanticRole::UserState,
        evidence: vec![Evidence::new(EvidenceKind::KnownUserStateRoot)],
        first_seen_at: Utc::now(),
        last_seen_at: Utc::now(),
    };
    fixture.agent.db.upsert_association(&association).unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    let first = fixture.manifest().head.unwrap();
    association.last_seen_at = Utc::now();
    association.evidence[0].observed_at = Utc::now();
    fixture.agent.db.upsert_association(&association).unwrap();
    fixture.agent.db.upsert_app(&fixture.app).unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert_eq!(fixture.manifest().owned.len(), 1);
    assert_eq!(fixture.manifest().head.unwrap().id, first.id);

    let mut config = AgentConfig::isolated(fixture.root.clone());
    config.instance_id = fixture.agent.config.instance_id;
    let restarted = StateAgent::boot(config).unwrap();
    maybe_checkpoint(&restarted).unwrap();
    assert_eq!(fixture.manifest().head.unwrap().id, first.id);

    association.semantic_role = SemanticRole::AppContent;
    fixture.agent.db.upsert_association(&association).unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert_ne!(fixture.manifest().head.unwrap().id, first.id);
}

#[test]
fn roots_commits_and_app_metadata_change_the_content_generation() {
    let fixture = Fixture::new();
    let snapshot = || {
        fixture
            .agent
            .db
            .learned_checkpoint(fixture.agent.config.instance_id, "test-image")
            .unwrap()
    };
    let first = content_generation(&snapshot());
    fixture
        .agent
        .db
        .add_known_root(&fixture.app.app_id, "state", "/home/gamer/save")
        .unwrap();
    let rooted = content_generation(&snapshot());
    assert_ne!(first, rooted);
    fixture
        .agent
        .db
        .record_commit(
            Uuid::new_v4(),
            &fixture.app.app_id,
            Uuid::new_v4(),
            "hash",
            None,
            CommitVisibility::Committed,
        )
        .unwrap();
    let committed = content_generation(&snapshot());
    assert_ne!(rooted, committed);
    let mut app = fixture.app.clone();
    app.icon_path = Some("/icons/game.png".into());
    fixture.agent.db.upsert_app(&app).unwrap();
    assert_ne!(committed, content_generation(&snapshot()));
}

#[test]
fn explicit_checkpoints_remain_fresh_unique_and_roundtrip_with_legacy_json() {
    let fixture = Fixture::new();
    let first = write_local_checkpoint(&fixture.agent).unwrap();
    let second = write_local_checkpoint(&fixture.agent).unwrap();
    assert_ne!(first.checkpoint_id, second.checkpoint_id);
    assert_eq!(first.apps[0].app_id, fixture.app.app_id);
    assert_eq!(content_generation(&first), content_generation(&second));
    let root = &fixture.agent.config.paths.checkpoints;
    let path = root.join(format!("{}.json.zst", second.checkpoint_id));
    assert_eq!(
        read_local_checkpoint(&path).unwrap().checkpoint_id,
        second.checkpoint_id
    );
    let legacy = root.join("legacy.json");
    fs::write(&legacy, serde_json::to_vec_pretty(&first).unwrap()).unwrap();
    assert_eq!(
        read_local_checkpoint(&legacy).unwrap().checkpoint_id,
        first.checkpoint_id
    );
    maybe_checkpoint(&fixture.agent).unwrap();
    assert!(fixture.manifest().owned.is_empty());
}

#[test]
fn retention_preserves_old_operation_and_seal_references_and_unowned_files() {
    let mut fixture = Fixture::new();
    let root = fixture.agent.config.paths.checkpoints.clone();
    let old = root.join("preexisting.json");
    let unknown = root.join(format!("periodic-v1-{}.json.zst", Uuid::new_v4()));
    fs::write(&old, b"old checkpoint").unwrap();
    fs::write(&unknown, b"unrecognized artifact").unwrap();
    fixture.change(0);
    let operation_ref = fixture.manifest().head.unwrap();
    fixture
        .agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: Uuid::new_v4(),
            kind: "checkpoint".into(),
            app_id: None,
            state: "COMPLETED".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({"result": {"checkpoint_id": operation_ref.id}}),
        })
        .unwrap();
    fixture.change(1);
    let seal_ref = fixture.manifest().head.unwrap();
    for id in [Some(seal_ref.id), None] {
        fixture
            .agent
            .db
            .record_seal(
                &SealRecord {
                    seal_id: Uuid::new_v4(),
                    instance_id: fixture.agent.config.instance_id,
                    image_id: "test".into(),
                    sealed_at: Utc::now(),
                    app_bundle_commits: vec![],
                    checkpoint_id: id,
                    state: "SEALED".into(),
                },
                SealState::Sealed,
            )
            .unwrap();
    }
    for n in 2..12 {
        fixture.change(n);
    }
    assert_eq!(fixture.manifest().owned.len(), RETAIN_PERIODIC + 2);
    for path in [
        old,
        unknown,
        root.join(operation_ref.name),
        root.join(seal_ref.name),
    ] {
        assert!(path.is_file(), "{} must survive retention", path.display());
    }
}

#[test]
fn modified_owned_files_are_not_collected() {
    let mut fixture = Fixture::new();
    fixture.change(0);
    let owned = fixture.manifest().head.unwrap();
    let path = fixture.agent.config.paths.checkpoints.join(owned.name);
    fs::write(&path, b"external replacement").unwrap();
    for n in 1..10 {
        fixture.change(n);
    }
    assert_eq!(fs::read(&path).unwrap(), b"external replacement");
}

#[test]
fn failed_publication_never_exposes_partial_or_overwrites_existing_artifacts() {
    let fixture = Fixture::new();
    let root = &fixture.agent.config.paths.checkpoints;
    let path = root.join("checkpoint.json.zst");
    assert!(atomic_write(&path, false, |file| {
        file.write_all(b"partial")?;
        Err(StateError::msg("simulated interruption before publication"))
    })
    .is_err());
    assert!(!path.exists());
    assert!(!fs::read_dir(root).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".checkpoint-tmp-")));
    fs::write(&path, b"preexisting").unwrap();
    assert!(atomic_write(&path, false, |file| {
        file.write_all(b"new")?;
        Ok(())
    })
    .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"preexisting");
    // A fully published checkpoint orphaned before manifest enrollment is also
    // preserved. Naming alone is never ownership authority.
    let checkpoint = fixture
        .agent
        .db
        .learned_checkpoint(fixture.agent.config.instance_id, "test")
        .unwrap();
    let orphan =
        publish_checkpoint(root, &checkpoint, true, content_generation(&checkpoint)).unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert!(root.join(orphan.name).exists());
}

#[test]
fn invalid_ownership_manifest_fails_closed_but_explicit_writes_still_work() {
    let fixture = Fixture::new();
    let path = fixture.agent.config.paths.checkpoints.join(MANIFEST_NAME);
    fs::write(&path, b"unrecognized manifest").unwrap();
    assert!(maybe_checkpoint(&fixture.agent).is_err());
    assert!(write_local_checkpoint(&fixture.agent).is_ok());
    assert_eq!(fs::read(&path).unwrap(), b"unrecognized manifest");
}

#[test]
fn publishing_an_older_snapshot_cannot_cover_a_newer_db_generation() {
    let mut fixture = Fixture::new();
    let root = &fixture.agent.config.paths.checkpoints;
    let (generation, snapshot) = fixture
        .agent
        .db
        .learned_checkpoint_versioned(
            fixture.agent.config.instance_id,
            &fixture.agent.config.image_id,
        )
        .unwrap();
    fixture.app.display_name = "Changed during publication".into();
    fixture.agent.db.upsert_app(&fixture.app).unwrap();
    let file = publish_checkpoint(root, &snapshot, true, content_generation(&snapshot)).unwrap();
    let id = file.id;
    write_manifest(
        root,
        &PeriodicManifest {
            owner: PERIODIC_OWNER.into(),
            head: Some(file.clone()),
            owned: vec![file],
            covered: Some(CheckpointSource::new(&fixture.agent, generation)),
        },
    )
    .unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    let manifest = fixture.manifest();
    assert_ne!(manifest.head.as_ref().unwrap().id, id);
    assert_eq!(
        manifest.covered.unwrap().database,
        fixture.agent.db.learned_checkpoint_generation().unwrap()
    );
    let saved = read_local_checkpoint(&root.join(manifest.head.unwrap().name)).unwrap();
    assert_eq!(saved.apps[0].display_name, fixture.app.display_name);
}

#[test]
fn legacy_manifest_upgrades_without_republishing_and_missing_head_recovers() {
    let fixture = Fixture::new();
    maybe_checkpoint(&fixture.agent).unwrap();
    let mut manifest = fixture.manifest();
    let head = manifest.head.clone().unwrap();
    let root = &fixture.agent.config.paths.checkpoints;
    manifest.covered = None;
    let mut legacy = serde_json::to_value(&manifest).unwrap();
    legacy.as_object_mut().unwrap().remove("covered");
    atomic_write(&root.join(MANIFEST_NAME), true, |file| {
        serde_json::to_writer(file, &legacy)?;
        Ok(())
    })
    .unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert_eq!(fixture.manifest().head.unwrap().id, head.id);
    assert!(fixture.manifest().covered.is_some());
    fs::remove_file(root.join(head.name)).unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert_ne!(fixture.manifest().head.unwrap().id, head.id);
}

#[test]
fn reverted_changes_coalesce_and_identity_changes_force_a_new_snapshot() {
    let mut fixture = Fixture::new();
    maybe_checkpoint(&fixture.agent).unwrap();
    let first = fixture.manifest().head.unwrap().id;
    let original = fixture.app.clone();
    fixture.app.display_name = "temporary".into();
    fixture.agent.db.upsert_app(&fixture.app).unwrap();
    fixture.agent.db.upsert_app(&original).unwrap();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert_eq!(fixture.manifest().head.unwrap().id, first);
    assert_eq!(
        fixture.manifest().covered.unwrap().database,
        fixture.agent.db.learned_checkpoint_generation().unwrap()
    );
    fixture.agent.config.image_id = "new-image".into();
    maybe_checkpoint(&fixture.agent).unwrap();
    assert_ne!(fixture.manifest().head.unwrap().id, first);
}
