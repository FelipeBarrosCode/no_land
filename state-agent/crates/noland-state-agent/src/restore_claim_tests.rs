use super::*;
use crate::{rpc_handler::AgentRpc, AgentConfig};
use noland_rpc::{RpcHandler, RpcRequest};
use noland_storage::*;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn terminal_restore_publication_waits_for_release_even_after_cancel_or_cleanup_failure() {
    use crate::repository::cleanup_tests::CredentialsProvider;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    for (fail_release, cancel_during_release, retained_ack) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, false, true),
        (false, true, true),
    ] {
        let root = crate::index_test_support::root("gated-restore-completion");
        let cloud = root.join("cloud");
        let source = StateAgent::boot(AgentConfig::isolated(root.join("source"))).unwrap();
        let install = source.config.home.join("Games/FinalizeFixture");
        std::fs::create_dir_all(&install).unwrap();
        let executable = install.join("game.bin");
        let bytes = b"durable restore must not advertise completion before release";
        std::fs::write(&executable, bytes).unwrap();
        let mut app = AppIdentity::new(AppId::desktop("finalize-fixture"), "Finalize Fixture");
        app.canonical_executable = Some(executable);
        source.db.upsert_app(&app).unwrap();
        source
            .db
            .add_known_root(&app.app_id, "install", install.to_str().unwrap())
            .unwrap();
        source.db.mark_dirty(&app.app_id, None, true).unwrap();
        let master = MasterKey::generate();
        let manifest = crate::backup::run_backup_to_local(
            &source,
            &app.app_id,
            BackupMode::CompleteApplication,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.join("target"))).unwrap());
        let id = queued(&agent, &app.app_id);
        let session = crate::index_test_support::session(&cloud);
        let (config, credentials) =
            write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
        let mut provider = CredentialsProvider::new(cloud.clone(), config.clone());
        let started = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        provider.release_started = Some(started.clone());
        provider.release_continue = Some(resume.clone());
        provider.fail_delete.store(fail_release, Ordering::SeqCst);
        provider.retain_claim.store(retained_ack, Ordering::SeqCst);
        let delete_calls = provider.delete_calls.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_callback = cancelled.clone();
        let task_agent = agent.clone();
        let task_master = master.clone();
        let (done, result) = tokio::sync::oneshot::channel();
        let app_id = app.app_id.clone();
        assert!(agent.operations.spawn_cancellable_on_lane(
            id,
            crate::operation_manager::OperationLane::Transfer,
            async move {
                let _credentials = credentials;
                let result = run_restore_with_provider(
                    &task_agent,
                    &app_id,
                    manifest.bundle_id,
                    RestoreMode::CompleteApplication,
                    &session,
                    &task_master,
                    id,
                    &provider,
                    |_| Ok(LocalStorage::new(cloud)),
                )
                .await;
                let _ = done.send(result);
            },
            move || {
                cancelled_callback.store(true, Ordering::SeqCst);
            },
            |_| panic!("restore finalization must not panic"),
        ));
        tokio::time::timeout(Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        let active = agent.db.get_operation(id).unwrap().unwrap();
        let progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        assert_eq!(active.state, "VALIDATING");
        assert_eq!(progress.phase, "VALIDATING");
        assert_eq!(active.detail_json["completion"]["durable"], true);
        assert_eq!(
            active.detail_json["completion"]["status"],
            "FINALIZING_REPOSITORY"
        );
        assert_eq!(
            std::fs::read(agent.config.home.join("Games/FinalizeFixture/game.bin")).unwrap(),
            bytes
        );
        assert!(!progress.detail_json["milestones"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "COMPLETE"));
        assert!(
            config.is_file(),
            "the blocked release still needs its credentials"
        );
        if cancel_during_release {
            assert_eq!(
                agent.operations.cancel(id),
                crate::operation_manager::CancelOutcome::Requested
            );
            tokio::task::yield_now().await;
            assert!(agent.operations.is_running(id));
            assert_eq!(
                agent.db.get_operation(id).unwrap().unwrap().state,
                "VALIDATING"
            );
            assert!(config.is_file());
        }
        resume.notify_one();
        tokio::time::timeout(Duration::from_secs(10), result)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while agent.operations.is_running(id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !cancelled.load(Ordering::SeqCst),
            "durable completion drains release rather than dropping it"
        );
        let completed = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(completed.state, "COMPLETED");
        assert_eq!(completed.detail_json["completion"]["status"], "FINALIZED");
        assert_eq!(
            completed.detail_json["repository_admission"]["cleanup_required"],
            fail_release || retained_ack
        );
        assert_eq!(
            delete_calls.load(Ordering::SeqCst),
            1,
            "uncertain claim deletion is never retried"
        );
        assert_eq!(completed.detail_json["complete_published_files"], 1);
        assert!(completed.last_error.is_none());
        assert!(
            !config.parent().unwrap().exists(),
            "shred follows the returned release attempt"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn queued(agent: &StateAgent, app: &AppId) -> Uuid {
    let id = Uuid::new_v4();
    agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: id,
            kind: "restore".into(),
            app_id: Some(app.clone()),
            state: "QUEUED".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({}),
        })
        .unwrap();
    id
}

#[tokio::test]
#[ignore = "requires rclone on PATH; explicit local-only configuration and fixture repository"]
async fn completed_restores_on_two_agent_roots_leave_no_claim_or_credentials() {
    let root = crate::index_test_support::root("real-rclone-claim-lifetime");
    let cloud = root.join("cloud");
    let storage = LocalStorage::new(cloud.clone());
    let master = MasterKey::generate();
    let app = AppIdentity::new(
        AppId::desktop("claim-lifetime-fixture"),
        "Claim Lifetime Fixture",
    );
    let payload = b"local-only restore claim lifetime regression fixture";
    let hash = noland_cas::blake3_hex(payload);
    let mut manifest = BundleManifest::new(
        ManifestApp::from(&app),
        ManifestSource {
            instance_id: Uuid::new_v4(),
            image_id: "local-only".into(),
            captured_at: chrono::Utc::now(),
        },
        BackupMode::CompleteApplication,
    );
    manifest.files.push(ManifestFile {
        logical_root: "$HOME".into(),
        relative_path: "Games/ClaimLifetime/game.bin".into(),
        source_path_hint: None,
        file_type: "file".into(),
        size: payload.len() as u64,
        file_hash: hash.clone(),
        chunks: vec![ChunkRef {
            hash: hash.clone(),
            size: payload.len() as u64,
        }],
        mode: Some(0o600),
        mtime_ns: None,
        uid: None,
        gid: None,
        symlink_target: None,
        persistence_class: PersistenceClass::ReconstructableApp,
        semantic_role: SemanticRole::AppContent,
        association_confidence: 1.0,
        shared_app_ids: Vec::new(),
    });
    let packs = noland_pack::pack_chunks(
        &root.join("packs"),
        &master,
        [(hash, payload.to_vec())],
        |_| false,
    )
    .unwrap();
    let index = packs
        .iter()
        .flat_map(|pack| pack.entries.clone())
        .collect::<Vec<_>>();
    let files = packs
        .iter()
        .map(|pack| (pack.pack_id.clone(), pack.path.clone()))
        .collect::<Vec<_>>();
    commit_bundle_with_index(
        &storage,
        &master,
        &manifest,
        &files,
        Some(&serde_json::to_vec(&index).unwrap()),
        None,
    )
    .await
    .unwrap();
    let session = crate::index_test_support::session(&cloud);
    let first = Arc::new(StateAgent::boot(AgentConfig::isolated(root.join("first"))).unwrap());
    let second = Arc::new(StateAgent::boot(AgentConfig::isolated(root.join("second"))).unwrap());
    let first_id = queued(&first, &app.app_id);
    let second_id = queued(&second, &app.app_id);
    let (left, right) = tokio::join!(
        run_restore_with_session(
            &first,
            &app.app_id,
            manifest.bundle_id,
            RestoreMode::CompleteApplication,
            &session,
            &master,
            first_id
        ),
        run_restore_with_session(
            &second,
            &app.app_id,
            manifest.bundle_id,
            RestoreMode::CompleteApplication,
            &session,
            &master,
            second_id
        ),
    );
    left.unwrap();
    right.unwrap();
    for (agent, id) in [(&first, first_id), (&second, second_id)] {
        assert_eq!(
            std::fs::read(agent.config.home.join("Games/ClaimLifetime/game.bin")).unwrap(),
            payload
        );
        let op = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(op.state, "COMPLETED");
        assert_eq!(op.detail_json["repository_admission"]["status"], "RELEASED");
        assert_eq!(
            op.detail_json["repository_admission"]["access"],
            "restore_read"
        );
        assert_eq!(op.detail_json["restored_all_manifest_files"], true);
        assert_eq!(op.detail_json["complete_published_files"], 1);
        assert_eq!(
            std::fs::read_dir(agent.config.paths.run_root.join("storage"))
                .unwrap()
                .count(),
            0
        );
        let status = AgentRpc((*agent).clone())
            .handle(&RpcRequest {
                id: "claim-lifetime".into(),
                method: "GetRestoreStatus".into(),
                params: serde_json::json!({"operation_id": id}),
            })
            .await
            .unwrap();
        assert_eq!(status["state"], "COMPLETED");
        assert_eq!(status["repository_admission"]["cleanup_required"], false);
    }
    assert!(repository_activities(&storage, &master)
        .await
        .unwrap()
        .is_empty());

    // Normal errors also finalize with the same live credential file.
    let failed = queued(&first, &app.app_id);
    assert!(run_restore_with_session(
        &first,
        &app.app_id,
        Uuid::new_v4(),
        RestoreMode::CompleteApplication,
        &session,
        &master,
        failed,
    )
    .await
    .is_err());
    assert_eq!(
        first.db.get_operation(failed).unwrap().unwrap().state,
        "FAILED"
    );
    assert!(repository_activities(&storage, &master)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read_dir(first.config.paths.run_root.join("storage"))
            .unwrap()
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}
