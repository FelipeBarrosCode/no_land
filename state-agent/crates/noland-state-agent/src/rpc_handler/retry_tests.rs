use super::*;
use std::sync::Arc;

use noland_crypto::MasterKey;
use noland_rpc::{RpcHandler, RpcRequest};
use noland_state_core::{
    AppId, ContentObjectKind, SyncDirection, SyncJournalEntry, SyncJournalState,
};
use uuid::Uuid;

use crate::operation_manager::{CancelOutcome, OperationLane};
use crate::rpc_handler::{remember_retry_request, submission, upsert_queued_operation, AgentRpc};
use crate::{AgentConfig, StateAgent};

fn session(root: &std::path::Path) -> EphemeralRcloneSession {
    EphemeralRcloneSession {
        operation_id: "synthetic-credentials-one".into(),
        provider: "local".into(),
        backend_type: "local".into(),
        remote_name: "store".into(),
        root: root.join("remote").to_string_lossy().into_owned(),
        config_ini: "[store]\ntype = local\n# synthetic-secret\n".into(),
        expires_at_unix: 0,
        upload_concurrency: None,
    }
}

fn key_hex(master: &MasterKey) -> String {
    master.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn persist_submission(
    agent: &StateAgent,
    kind: &str,
    selection: &Value,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    id: Uuid,
) -> Value {
    let mut params = selection.clone();
    params["client_operation_id"] = json!(id);
    params["session"] = serde_json::to_value(session).unwrap();
    params["master_key_hex"] = json!(key_hex(master));
    params["client_storage_identity"] = json!({
        "provider": session.provider, "backend_type": session.backend_type,
        "remote_name": session.remote_name, "root": session.root,
        "config_ini": "synthetic-secret-must-not-persist",
    });
    let submission::Claim::New {
        operation_id,
        binding,
    } = submission::resolve(&agent.db, &params, session, master, kind, selection, None).unwrap()
    else {
        panic!("new operation expected")
    };
    upsert_queued_operation(
        agent,
        operation_id,
        kind,
        selection
            .get("app_id")
            .and_then(Value::as_str)
            .filter(|app| *app != "*")
            .map(|app| AppId(app.into())),
        "QUEUED",
        submission::bind_details(selection.clone(), id, binding),
        false,
    )
    .unwrap();
    let method = match kind {
        "backup" => "StartBackup",
        "restore" => "StartRestore",
        "seal" => "StartSeal",
        _ => unreachable!(),
    };
    remember_retry_request(agent, id, method, &params).unwrap();
    params
}

async fn call(agent: &Arc<StateAgent>, method: &str, params: Value) -> Result<Value> {
    AgentRpc(agent.clone())
        .handle(&RpcRequest {
            id: "synthetic-rpc".into(),
            method: method.into(),
            params,
        })
        .await
}

async fn block_lane(agent: &StateAgent, lane: OperationLane) -> Uuid {
    let id = Uuid::new_v4();
    let (started, waiting) = tokio::sync::oneshot::channel();
    assert!(agent.operations.spawn_cancellable_on_lane(
        id,
        lane,
        async move {
            let _ = started.send(());
            std::future::pending::<()>().await;
        },
        || {},
        |_| {}
    ));
    waiting.await.unwrap();
    id
}

async fn cancel_and_wait(agent: &StateAgent, id: Uuid) {
    assert_eq!(agent.operations.cancel(id), CancelOutcome::Requested);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while agent.operations.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn restart_retry_disconnect_keeps_matching_identity_and_submission_binding() {
    for (kind, selection, lane) in [
        (
            "backup",
            json!({"app_id":"*", "mode":"complete_application", "performance_mode":"balanced", "regenerable_caches":"exclude"}),
            OperationLane::Transfer,
        ),
        (
            "restore",
            json!({"app_id":"desktop:synthetic-game", "mode":"complete_application", "bundle_id":Uuid::new_v4()}),
            OperationLane::Transfer,
        ),
        (
            "seal",
            json!({"app_id":"*", "mode":"complete_application"}),
            OperationLane::Transfer,
        ),
    ] {
        let root = std::env::temp_dir().join(format!("noland-retry-{}", Uuid::new_v4()));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let mut session = session(&root);
        let master = MasterKey::generate();
        let id = Uuid::new_v4();
        let original = persist_submission(&agent, kind, &selection, &session, &master, id);
        let saved = agent.db.get_operation(id).unwrap().unwrap();
        let binding = saved.detail_json["submission_binding"].clone();
        let expected_identity = sanitized_identity(&original["client_storage_identity"]).unwrap();
        assert!(!saved.detail_json.to_string().contains("synthetic-secret"));
        assert!(!saved.detail_json.to_string().contains(&key_hex(&master)));
        drop(agent);

        let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
        agent.recover().unwrap();
        assert_eq!(
            agent.db.get_operation(id).unwrap().unwrap().state,
            "INTERRUPTED"
        );
        assert_eq!(
            agent.db.get_operation(id).unwrap().unwrap().detail_json["resume"]["retry_available"],
            true,
            "every supported operation kind retains explicit retry availability"
        );
        assert!(agent.operations.retry_descriptor(id).is_none());
        let blocker = block_lane(&agent, lane).await;
        session.operation_id = "synthetic-credentials-after-restart".into();
        let retried = call(
            &agent,
            "RetryOperation",
            json!({
                "operation_id":id, "session":session, "master_key_hex":key_hex(&master),
            }),
        )
        .await
        .unwrap();
        assert_eq!(retried["operation_id"], id.to_string());
        assert_eq!(retried["retried"], true);

        // Each call creates a fresh RPC handle, just as a disconnected client
        // reconnects. No worker can perform I/O while the test holds its lane.
        let recent = call(
            &agent,
            "ListRecentOperations",
            json!({"limit":100, "projection":"matching", "include_journal":true}),
        )
        .await
        .unwrap();
        assert_eq!(recent["operations"].as_array().unwrap().len(), 1);
        let status = &recent["operations"][0];
        assert_eq!(status["operation_id"], id.to_string());
        assert_eq!(status["running"], true);
        assert_eq!(
            status["detail_json"]["client_storage_identity"],
            expected_identity
        );
        assert_eq!(status["detail_json"]["client_operation_id"], id.to_string());
        assert_eq!(
            status["detail_json"]["retry"]["params"]["client_storage_identity"],
            expected_identity
        );
        for (field, value) in selection.as_object().unwrap() {
            assert_eq!(&status["detail_json"][field], value);
        }
        for omitted in [
            "progress",
            "metrics",
            "sync_journal",
            "sync_journal_entries",
            "pack_transfer",
        ] {
            assert!(status.get(omitted).is_none());
        }
        let targeted = call(
            &agent,
            "GetOperationStatus",
            json!({"operation_id":id, "projection":"matching"}),
        )
        .await
        .unwrap();
        assert_eq!(&targeted, status);
        let full = call(&agent, "GetOperationStatus", json!({"operation_id":id}))
            .await
            .unwrap();
        assert!(full.get("sync_journal").is_some());
        assert_eq!(full["detail_json"]["submission_binding"], binding);
        assert_eq!(
            full["detail_json"]["retry"]["params"]["client_storage_identity"],
            expected_identity
        );
        assert_eq!(
            full["detail_json"]["retry"]["params"]["client_operation_id"],
            id.to_string()
        );
        assert!(!full["detail_json"].to_string().contains("synthetic-secret"));
        assert!(call(&agent, "RetryOperation", json!({"operation_id":id}))
            .await
            .is_err());
        cancel_and_wait(&agent, id).await;
        cancel_and_wait(&agent, blocker).await;
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn rejected_retry_overrides_do_not_reset_the_journal_or_identity() {
    let root = std::env::temp_dir().join(format!("noland-retry-rejected-{}", Uuid::new_v4()));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let session = session(&root);
    let master = MasterKey::generate();
    let id = Uuid::new_v4();
    let selection = json!({"app_id":"*", "mode":"complete_application", "performance_mode":"balanced", "regenerable_caches":"include"});
    persist_submission(&agent, "backup", &selection, &session, &master, id);
    let mut operation = agent.db.get_operation(id).unwrap().unwrap();
    operation.state = "INTERRUPTED".into();
    agent.db.upsert_operation(&operation).unwrap();
    let mut entry = SyncJournalEntry::pending(
        id,
        "synthetic-pack",
        ContentObjectKind::Pack,
        SyncDirection::Upload,
    );
    entry.state = SyncJournalState::Failed;
    agent.db.upsert_sync_journal_entry(&entry).unwrap();
    let retry = json!({"operation_id":id, "session":session, "master_key_hex":key_hex(&master)});
    let mut requests = Vec::new();
    for (field, value) in [
        ("mode", json!("personal_state")),
        ("app_id", json!("another-app")),
        ("regenerable_caches", json!("exclude")),
        ("performance_mode", json!("fast")),
        ("client_operation_id", json!(Uuid::new_v4())),
        ("client_storage_identity", json!({"root":"somewhere-else"})),
        ("_retry_operation_id", json!(id)),
    ] {
        let mut changed = retry.clone();
        changed[field] = value;
        requests.push(changed);
    }
    let mut wrong_key = retry.clone();
    wrong_key["master_key_hex"] = json!(key_hex(&MasterKey::generate()));
    requests.push(wrong_key);
    let mut wrong_repository = retry;
    wrong_repository["session"]["root"] = json!("/different-repository");
    requests.push(wrong_repository);
    for request in requests {
        assert!(call(&agent, "RetryOperation", request).await.is_err());
        assert_eq!(
            agent.db.get_operation(id).unwrap().unwrap().detail_json,
            operation.detail_json
        );
        assert_eq!(
            agent
                .db
                .get_sync_journal_entry(id, "synthetic-pack")
                .unwrap()
                .unwrap()
                .state,
            SyncJournalState::Failed
        );
        assert!(!agent.operations.is_running(id));
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retry_accepts_equivalent_aliases_but_persists_only_sanitized_identity() {
    let mut stored = json!({"app_id":"*", "mode":"complete_application", "client_storage_identity": {
        "provider":"sftp", "backend_type":"sftp", "remote_name":"store", "root":"/absolute"
    }});
    overlay_retry_secrets(
        &mut stored,
        &json!({"app_id":"_all", "mode":"complete", "session":{"config_ini":"synthetic-secret"}}),
    )
    .unwrap();
    assert_eq!(stored["mode"], "complete_application");
    assert_eq!(stored["app_id"], "*");
    let mut wrong_root = stored["client_storage_identity"].clone();
    wrong_root["root"] = json!("absolute");
    assert!(
        overlay_retry_secrets(&mut stored, &json!({"client_storage_identity":wrong_root})).is_err()
    );
    assert!(!sanitize_retry_params(&stored)
        .to_string()
        .contains("synthetic-secret"));
}

#[tokio::test]
async fn seal_replay_is_scoped_and_legacy_submissions_still_get_distinct_ids() {
    let root = std::env::temp_dir().join(format!("noland-seal-replay-{}", Uuid::new_v4()));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let session = session(&root);
    let master = MasterKey::generate();
    let selection = json!({"app_id":"*", "mode":"complete_application"});
    let id = Uuid::new_v4();
    let params = persist_submission(&agent, "seal", &selection, &session, &master, id);
    for state in ["QUEUED", "SEALED", "FAILED", "INTERRUPTED"] {
        let mut operation = agent.db.get_operation(id).unwrap().unwrap();
        operation.state = state.into();
        agent.db.upsert_operation(&operation).unwrap();
        let mut rotated = params.clone();
        rotated["session"]["operation_id"] = json!("rotated-synthetic-session");
        let replay = call(&agent, "StartSeal", rotated).await.unwrap();
        assert_eq!(replay["operation_id"], id.to_string());
        assert_eq!(replay["state"], state);
        assert!(!agent.operations.is_running(id));
    }
    for change in ["mode", "key", "repository"] {
        let mut changed = params.clone();
        match change {
            "mode" => changed["mode"] = json!("personal_state"),
            "key" => changed["master_key_hex"] = json!(key_hex(&MasterKey::generate())),
            _ => {
                changed["session"]["root"] = json!("/elsewhere");
                changed["client_storage_identity"]["root"] = json!("/elsewhere");
            }
        }
        assert!(call(&agent, "StartSeal", changed).await.is_err());
    }
    let blocker = block_lane(&agent, OperationLane::Transfer).await;
    let mut legacy = params;
    legacy
        .as_object_mut()
        .unwrap()
        .remove("client_operation_id");
    let first = call(&agent, "StartSeal", legacy.clone()).await.unwrap();
    let second = call(&agent, "StartSeal", legacy).await.unwrap();
    assert_ne!(first["operation_id"], second["operation_id"]);
    for response in [first, second] {
        cancel_and_wait(
            &agent,
            Uuid::parse_str(response["operation_id"].as_str().unwrap()).unwrap(),
        )
        .await;
    }
    cancel_and_wait(&agent, blocker).await;
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn seal_waits_for_transfer_lane_and_can_cancel_without_internal_side_effects() {
    let root = std::env::temp_dir().join(format!("noland-seal-lane-{}", Uuid::new_v4()));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let blocker = block_lane(&agent, OperationLane::Transfer).await;
    let master = MasterKey::generate();
    let response = call(
        &agent,
        "StartSeal",
        json!({
            "client_operation_id": Uuid::new_v4(), "mode":"personal_state",
            "session": session(&root), "master_key_hex":key_hex(&master)
        }),
    )
    .await
    .unwrap();
    let id = Uuid::parse_str(response["operation_id"].as_str().unwrap()).unwrap();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        agent.db.get_operation(id).unwrap().unwrap().state,
        "REQUESTED"
    );
    assert_eq!(agent.db.recent_operations(100).unwrap().len(), 1);
    assert!(agent
        .operations
        .running_snapshots()
        .iter()
        .find(|op| op.operation_id == id)
        .unwrap()
        .running_for_ms
        .is_none());
    cancel_and_wait(&agent, id).await;
    assert_eq!(
        agent.db.get_operation(id).unwrap().unwrap().state,
        "CANCELLED"
    );
    cancel_and_wait(&agent, blocker).await;
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn fresh_backups_normalize_removed_options_and_replay_the_same_complete_selection() {
    let root = std::env::temp_dir().join(format!("noland-complete-request-{}", Uuid::new_v4()));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let blocker = block_lane(&agent, OperationLane::Transfer).await;
    let id = Uuid::new_v4();
    let master = MasterKey::generate();
    let params = json!({"app_id":"game", "client_operation_id":id,
        "mode":"personal_state", "performance_mode":"balanced", "regenerable_caches":"exclude",
        "session":session(&root), "master_key_hex":key_hex(&master)});
    call(&agent, "StartBackup", params.clone()).await.unwrap();
    let operation = agent.db.get_operation(id).unwrap().unwrap();
    assert_eq!(operation.detail_json["mode"], "complete_application");
    assert_eq!(operation.detail_json["performance_mode"], "automatic");
    assert_eq!(operation.detail_json["regenerable_caches"], "include");
    let replay = call(&agent, "StartBackup", params).await.unwrap();
    assert_eq!(replay["operation_id"], id.to_string());
    assert_eq!(agent.db.recent_operations(100).unwrap().len(), 1);
    cancel_and_wait(&agent, id).await;
    // An API 19 request with omitted settings must still replay the original
    // personal/balanced/include identity, rather than start a complete revision.
    let legacy_id = Uuid::new_v4();
    let selection = json!({"app_id":"game", "mode":"personal_state", "performance_mode":"balanced", "regenerable_caches":"include"});
    let mut legacy = persist_submission(
        &agent,
        "backup",
        &selection,
        &session(&root),
        &master,
        legacy_id,
    );
    for key in ["mode", "performance_mode", "regenerable_caches"] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    let replay = call(&agent, "StartBackup", legacy).await.unwrap();
    assert_eq!(replay["operation_id"], legacy_id.to_string());
    assert!(!agent.operations.is_running(legacy_id));
    cancel_and_wait(&agent, blocker).await;
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
