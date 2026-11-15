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

#[tokio::test]
async fn completed_operation_cleanup_warning_remains_visible_without_relabeling_or_retry() {
    for kind in ["backup", "restore", "seal", "delete"] {
        let (root, agent, mut operation) = fixture();
        operation.operation_id = Uuid::new_v4();
        let id = operation.operation_id;
        operation.kind = kind.into();
        operation.detail_json["repository_admission"] = json!({
            "status":"CLEANUP_REQUIRED",
            "cleanup_required":true,
            "access":if kind == "restore" { "restore_read" } else { "exclusive" },
            "instance_id":Uuid::new_v4(),
            "reason":"release_failed",
        });
        agent.db.upsert_operation(&operation).unwrap();
        let terminal = if kind == "seal" {
            "SEALED"
        } else {
            "COMPLETED"
        };
        mark_operation_completed(&agent, id, terminal, json!({}), Instant::now());
        let full = status(&agent, "GetOperationStatus", json!({"operation_id":id})).await;
        let projected = status(
            &agent,
            "GetOperationStatus",
            json!({"operation_id":id,"projection":"progress"}),
        )
        .await;
        assert_eq!(full["state"], terminal);
        assert_eq!(projected["state"], terminal);
        assert_eq!(full["repository_admission"]["status"], "CLEANUP_REQUIRED");
        assert_eq!(
            projected["progress"]["detail_json"]["repository_admission"]["status"],
            "CLEANUP_REQUIRED"
        );
        assert!(full["last_error"].is_null());
        assert!(
            !is_retryable_state(terminal),
            "cleanup alone never makes deletion/transfer retryable"
        );
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn terminal_diagnostics_keep_readback_catalog_and_future_io_fields_and_retry_separates_attempts(
) {
    for terminal in ["completed", "failed", "cancelled"] {
        let (root, agent, operation) = fixture();
        let id = operation.operation_id;
        let mut progress = OperationProgress::new("applying", 7);
        progress.detail_json = json!({
            "pipeline": {"readback_bytes":123, "verified_bytes":456, "elapsed_ms":789,
                "io":{"write_us":10, "future_scheduler_counter":11, "commit":{"elapsed_us":12}}},
            "catalog":{"load_catalog_duration_ms":13},
            "remote_metadata":{"visibility_check_duration_ms":14},
            "session":{"token":"must-not-be-archived"},
        });
        agent
            .db
            .set_operation_progress(id, Some(&progress))
            .unwrap();
        match terminal {
            "completed" => {
                mark_operation_completed(&agent, id, "COMPLETED", json!({}), Instant::now())
            }
            "failed" => mark_operation_failed(&agent, id, "fixture failure".into(), Instant::now()),
            _ => mark_operation_cancelled(&agent, id),
        }
        let finished = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(
            finished.detail_json["pipeline"],
            progress.detail_json["pipeline"]
        );
        assert_eq!(
            finished.detail_json["catalog"],
            progress.detail_json["catalog"]
        );
        assert!(finished.detail_json.get("session").is_none());
        if terminal != "completed" {
            assert_eq!(
                agent
                    .db
                    .get_operation_progress(id)
                    .unwrap()
                    .unwrap()
                    .detail_json["pipeline"],
                progress.detail_json["pipeline"]
            );
        }
        let mut retry = finished;
        crate::operation_diagnostics::begin_retry(&mut retry);
        assert!(retry.detail_json.get("pipeline").is_none());
        assert_eq!(
            retry.detail_json["previous_attempt_diagnostics"]["pipeline"]["readback_bytes"],
            123
        );
        drop(agent);
        let rebooted = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        assert_eq!(
            rebooted.db.get_operation(id).unwrap().unwrap().detail_json["pipeline"]["io"]
                ["future_scheduler_counter"],
            11
        );
        drop(rebooted);
        std::fs::remove_dir_all(root).unwrap();
    }
}
