use super::*;
use crate::AgentConfig;

fn fixture() -> (PathBuf, Arc<StateAgent>, OperationRecord) {
    let root = std::env::temp_dir().join(format!("noland-progress-rpc-{}", Uuid::new_v4()));
    let agent = Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
    let operation = OperationRecord {
        operation_id: Uuid::new_v4(),
        kind: "restore".into(),
        app_id: Some(AppId::desktop("synthetic-game")),
        state: "DOWNLOADING".into(),
        created_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        updated_at: "2026-01-02T03:05:06Z".parse().unwrap(),
        last_error: Some("synthetic recoverable transfer error".into()),
        detail_json: json!({}),
    };
    agent.db.upsert_operation(&operation).unwrap();
    (root, agent, operation)
}

async fn status(
    agent: &Arc<StateAgent>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    AgentRpc(agent.clone())
        .handle(&RpcRequest {
            id: "synthetic-progress-rpc".into(),
            method: method.into(),
            params,
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn progress_projection_preserves_cancellation_and_all_terminal_states() {
    let (root, agent, mut operation) = fixture();
    let id = operation.operation_id;
    let cancelled = agent.clone();
    assert!(agent.operations.spawn_cancellable(
        id,
        std::future::pending::<()>(),
        move || mark_operation_cancelled(&cancelled, id),
        |_| {}
    ));
    let active = status(
        &agent,
        "GetOperationStatus",
        json!({"operation_id":id,"projection":"progress"}),
    )
    .await;
    assert_eq!(active["running"], true);
    assert_eq!(active["cancel_requested"], false);
    assert_eq!(active["progress"], serde_json::Value::Null);
    assert_eq!(active["pack_transfer"]["total_items"], 0);
    let accepted = status(&agent, "CancelOperation", json!({"operation_id":id})).await;
    assert_eq!(accepted["cancel_requested"], true);
    // These RPCs do not yield, so the current-thread worker has not yet handled
    // cancellation. Preserve the acknowledged request separately from its state.
    let cancelling = status(
        &agent,
        "GetOperationStatus",
        json!({"operation_id":id,"projection":"progress"}),
    )
    .await;
    assert_eq!(cancelling["operation_id"], id.to_string());
    assert_eq!(cancelling["running"], true);
    assert_eq!(cancelling["cancel_requested"], true);
    assert_eq!(cancelling["state"], "DOWNLOADING");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while agent.operations.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for (method, kind, state) in [
        ("GetBackupStatus", "backup", "COMPLETED"),
        ("GetRestoreStatus", "restore", "FAILED"),
        ("GetRestoreStatus", "restore", "ROLLED_BACK"),
        ("GetOperationStatus", "backup", "CANCELLED"),
        ("GetOperationStatus", "restore", "INTERRUPTED"),
        ("GetSealStatus", "seal", "SEALED"),
    ] {
        // Kind is immutable for an existing operation ID. Use a fresh record
        // for each alias/kind instead of attempting to rebind the cancelled ID.
        operation.operation_id = Uuid::new_v4();
        let id = operation.operation_id;
        operation.kind = kind.into();
        operation.state = state.into();
        operation.last_error = (state == "FAILED").then(|| "synthetic terminal failure".into());
        agent.db.upsert_operation(&operation).unwrap();
        let full = status(&agent, method, json!({"operation_id":id})).await;
        let projected = status(
            &agent,
            method,
            json!({"operation_id":id,"projection":"progress"}),
        )
        .await;
        assert_eq!(projected["state"], state);
        assert_eq!(projected["kind"], kind);
        assert_eq!(projected["running"], false);
        assert_eq!(projected["cancel_requested"], false);
        for (key, value) in projected.as_object().unwrap() {
            assert_eq!(Some(value), full.get(key));
        }
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn progress_projection_stays_small_and_matches_full_totals_with_large_results() {
    let (root, agent, mut operation) = fixture();
    let id = operation.operation_id;
    operation.detail_json = json!({
        "checkpoint": {"manifest": "synthetic-result-".repeat(350_000)},
        "retry": {"params": {"app_id": "synthetic-game"}},
    });
    assert!(serde_json::to_vec(&operation.detail_json).unwrap().len() > 4 * 1024 * 1024);
    agent.db.upsert_operation(&operation).unwrap();
    let mut progress = OperationProgress::new("FINALIZING", 7);
    progress.total_units = Some(9);
    progress.unit = Some("packs".into());
    progress.message = Some("Synthetic transfer progress".into());
    progress.detail_json = json!({
        "pipeline": {"published_bytes":2000, "total_bytes":3000},
        "total_packs":9, "completed_pack_bytes":1800, "total_transfer_bytes":2900,
        "bytes_transferred":777, "ready_to_launch_reached":true,
        "milestones":["RESTORE_STARTED", "READY_TO_LAUNCH"],
    });
    agent
        .db
        .set_operation_progress(id, Some(&progress))
        .unwrap();
    agent
        .db
        .set_operation_metrics(id, &OperationMetrics::default())
        .unwrap();
    for (operation_id, key, kind, size, state, transferred) in [
        (
            id,
            "pack-one",
            ContentObjectKind::Pack,
            7,
            SyncJournalState::Completed,
            7,
        ),
        (
            id,
            "pack-two",
            ContentObjectKind::Pack,
            11,
            SyncJournalState::InProgress,
            3,
        ),
        (
            id,
            "manifest",
            ContentObjectKind::Other,
            99,
            SyncJournalState::Completed,
            99,
        ),
        (
            Uuid::new_v4(),
            "foreign-pack",
            ContentObjectKind::Pack,
            999,
            SyncJournalState::Completed,
            999,
        ),
    ] {
        let mut entry = SyncJournalEntry::pending(operation_id, key, kind, SyncDirection::Download);
        entry.size = Some(size);
        entry.state = state;
        entry.bytes_transferred = transferred;
        agent.db.upsert_sync_journal_entry(&entry).unwrap();
    }
    let full = status(
        &agent,
        "GetOperationStatus",
        json!({"operation_id":id,"include_journal":true}),
    )
    .await;
    assert!(serde_json::to_vec(&full).unwrap().len() > 4 * 1024 * 1024);
    assert!(full["detail_json"]["checkpoint"].is_object());
    assert!(full["metrics"].is_object());
    assert_eq!(full["sync_journal"]["total_bytes"], 117);
    assert_eq!(full["pack_transfer"]["total_bytes"], 18);
    assert_eq!(full["pack_transfer"]["bytes_transferred"], 10);
    assert_eq!(full["pack_transfer"]["completed_items"], 1);
    for method in [
        "GetOperationStatus",
        "GetBackupStatus",
        "GetRestoreStatus",
        "GetSealStatus",
    ] {
        let projected = status(
            &agent,
            method,
            json!({"operation_id":id,"projection":"progress","include_journal":true}),
        )
        .await;
        assert!(serde_json::to_vec(&projected).unwrap().len() < 64 * 1024);
        for field in [
            "operation_id",
            "kind",
            "app_id",
            "state",
            "created_at",
            "updated_at",
            "last_error",
            "running",
            "cancel_requested",
            "progress",
            "pack_transfer",
        ] {
            assert_eq!(projected.get(field), full.get(field), "{method}: {field}");
        }
        assert_eq!(projected.as_object().unwrap().len(), 11);
        for omitted in [
            "detail_json",
            "result",
            "retry",
            "metrics",
            "sync_journal",
            "sync_journal_entries",
        ] {
            assert!(projected.get(omitted).is_none(), "{method}: {omitted}");
        }
        assert_eq!(projected["progress"]["detail_json"], progress.detail_json);
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
