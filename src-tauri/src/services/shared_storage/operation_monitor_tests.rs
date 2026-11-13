use super::*;
use std::collections::VecDeque;
use std::future;

const FAST: PollPolicy = PollPolicy {
    interval: Duration::from_millis(1),
    idle_max: Duration::from_millis(4),
    fetch_timeout: Duration::from_millis(20),
    reconnect_initial: Duration::from_millis(1),
    reconnect_max: Duration::from_millis(4),
};

#[tokio::test]
async fn deletion_preflight_progress_is_visible_even_before_the_first_object_is_removed() {
    let sample = |count| {
        json!({"operation_id":"delete", "state":"RUNNING", "progress": {
        "phase":"validating", "completed_units":0, "total_units":119,
        "detail_json":{"deletion":{"validated_packs":count,"completed_objects":0}}}})
    };
    let mut replies = VecDeque::from([
        sample(1),
        sample(2),
        json!({"operation_id":"delete", "state":"COMPLETED"}),
    ]);
    let mut events = Vec::new();
    monitor_with_policy(
        "delete",
        Duration::from_secs(1),
        FAST,
        |_| future::ready(Ok::<_, ()>(replies.pop_front().unwrap())),
        |event| events.push(event.clone()),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[1].pointer("/progress/detail_json/deletion/validated_packs"),
        Some(&json!(2))
    );
}

#[tokio::test]
async fn unchanged_samples_ignore_timestamps_and_reconnect_is_still_visible() {
    let sample = |at: u64| {
        json!({"operation_id":"op", "state":"UPLOADING", "updated_at":at,
        "progress":{"phase":"uploading", "completed_units":1, "updated_at":at}})
    };
    let mut replies = VecDeque::from([
        Ok(sample(1)),
        Ok(sample(2)),
        Ok(sample(3)),
        Err(()),
        Ok(sample(4)),
        Ok(json!({"operation_id":"op", "state":"COMPLETED"})),
    ]);
    let mut events = vec![];
    monitor_with_policy(
        "op",
        Duration::from_secs(1),
        FAST,
        |_| future::ready(replies.pop_front().unwrap()),
        |status| events.push(status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["UPLOADING", "RECONNECTING", "UPLOADING", "COMPLETED"]
    );
    assert_eq!(idle_delay(POLL_POLICY, 0), Duration::from_secs(1));
    assert_eq!(idle_delay(POLL_POLICY, 1), Duration::from_secs(2));
    assert_eq!(idle_delay(POLL_POLICY, u32::MAX), Duration::from_secs(8));
}

#[tokio::test]
async fn a_hung_poll_is_dropped_before_reconnecting_to_the_same_id() {
    use std::cell::Cell;
    struct PendingPoll<'a>(&'a Cell<u32>);
    impl Drop for PendingPoll<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let dropped = Cell::new(0);
    let calls = Cell::new(0);
    let mut events = vec![];
    monitor_with_policy(
        "op",
        Duration::from_secs(1),
        PollPolicy {
            fetch_timeout: Duration::from_millis(2),
            ..FAST
        },
        |id| {
            let attempt = calls.get();
            calls.set(attempt + 1);
            let dropped = &dropped;
            async move {
                assert_eq!(id, "op");
                if attempt == 0 {
                    let _pending = PendingPoll(dropped);
                    future::pending::<()>().await;
                }
                assert_eq!(
                    dropped.get(),
                    1,
                    "the previous transport future must be dropped"
                );
                Ok::<_, ()>(json!({"operation_id":"op", "state":"COMPLETED"}))
            }
        },
        |status| events.push(status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(events[0]["state"], "RECONNECTING");
}

#[test]
fn commit_keeps_transfer_metrics_without_counting_reuse_as_network_bytes() {
    let uploaded = json!({"operation_id":"op", "state":"UPLOADING",
        "progress":{"detail_json":{"pipeline":{"published_bytes":1000,"total_bytes":1000},
            "bytes_transferred":40,"total_packs":2}},
        "pack_transfer":{"completed_bytes":600,"total_bytes":600,"bytes_transferred":40}});
    let mut committing = json!({"operation_id":"op", "state":"COMMITTING",
        "progress":{"phase":"commit","completed_units":0}});
    retain_transfer_progress(&mut committing, Some(&uploaded));
    assert_eq!(
        committing["progress"]["detail_json"]["pipeline"]["published_bytes"],
        1000
    );
    assert_eq!(
        committing["progress"]["detail_json"]["bytes_transferred"],
        40
    );
    assert_eq!(committing["pack_transfer"]["bytes_transferred"], 40);
    assert_eq!(committing["progress"]["phase"], "commit");
    let mut retried = json!({"operation_id":"op", "state":"QUEUED"});
    retain_transfer_progress(&mut retried, Some(&committing));
    assert!(retried.get("pack_transfer").is_none());
}

#[tokio::test]
async fn lost_connections_recover_and_complete_the_same_operation() {
    let mut replies = VecDeque::from([
        Ok(
            json!({"operation_id":"same-id", "state":"UPLOADING", "running":true,
            "progress":{"completed_units":7,"total_units":10}}),
        ),
        Err("SSH reset"),
        Err("SSH offline"),
        Ok(json!({"operation_id":"same-id", "state":"COMPLETED"})),
    ]);
    let mut ids = vec![];
    let mut events = vec![];
    let result = monitor_with_policy(
        "same-id",
        Duration::from_secs(1),
        FAST,
        |id| {
            ids.push(id);
            future::ready(replies.pop_front().unwrap())
        },
        |status| events.push(status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(result["state"], "COMPLETED");
    assert_eq!(ids, vec!["same-id"; 4]);
    assert_eq!(
        events
            .iter()
            .map(|e| e["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["UPLOADING", "RECONNECTING", "RECONNECTING", "COMPLETED"]
    );
    assert_eq!(events[1]["progress"]["completed_units"], 7);
    assert_eq!(events[1]["running"], true);
    assert!(events[1]["progress"]["message"]
        .as_str()
        .unwrap()
        .contains("same-id"));
}

#[tokio::test]
async fn authoritative_failures_and_cancellation_are_never_retried() {
    for state in ["FAILED", "CANCELLED", "INTERRUPTED"] {
        let mut calls = 0;
        let error = monitor_with_policy(
            "op",
            Duration::from_secs(1),
            FAST,
            |_| {
                calls += 1;
                future::ready(Ok::<_, ()>(json!({
                    "operation_id":"op", "state":state, "last_error":"authoritative failure",
                    "progress":{"detail_json":{"ready_to_launch_reached":true}}
                })))
            },
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(!error.keep_active_handle());
        assert!(
            matches!(error, MonitorFailure::Terminal { state: actual, message } if actual == state && message.contains("op"))
        );
    }
}

#[tokio::test]
async fn wrong_operation_completion_cannot_end_monitoring() {
    let mut replies = VecDeque::from([
        json!({"operation_id":"other", "state":"COMPLETED"}),
        json!({"state":"FAILED"}),
        json!({"operation_id":"op", "state":"COMPLETED"}),
    ]);
    let mut events = vec![];
    let result = monitor_with_policy(
        "op",
        Duration::from_secs(1),
        FAST,
        |_| future::ready(Ok::<_, ()>(replies.pop_front().unwrap())),
        |status| events.push(status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(result["operation_id"], "op");
    assert_eq!(events[0]["state"], "RECONNECTING");
    assert_eq!(events[1]["state"], "RECONNECTING");
}

#[tokio::test]
async fn timeout_retains_resumable_handle_and_never_claims_remote_failure() {
    let mut events = vec![];
    let failure = monitor_with_policy(
        "resumable-op",
        Duration::from_millis(5),
        FAST,
        |_| future::pending::<Result<Value, ()>>(),
        |status| events.push(status.clone()),
    )
    .await
    .unwrap_err();
    assert!(failure.keep_active_handle());
    assert!(
        matches!(failure, MonitorFailure::TimedOut { message } if message.contains("resumable-op") && message.contains("may still be running"))
    );
    assert_eq!(events.last().unwrap()["state"], "MONITORING_PAUSED");
    assert_eq!(events.last().unwrap()["running"], true);
}

#[tokio::test]
async fn ready_to_launch_is_not_completion_and_recovery_resets_backoff() {
    let mut replies = VecDeque::from([
        Err(()),
        Err(()),
        Ok(json!({"operation_id":"op", "state":"APPLYING",
            "progress":{"detail_json":{"ready_to_launch_reached":true}}})),
        Err(()),
        Ok(json!({"operation_id":"op", "state":"COMPLETED"})),
    ]);
    let mut events = vec![];
    monitor_with_policy(
        "op",
        Duration::from_secs(1),
        FAST,
        |_| future::ready(replies.pop_front().unwrap()),
        |status| events.push(status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 5);
    assert!(events[3]["progress"]["message"]
        .as_str()
        .unwrap()
        .contains("attempt 1"));
    assert_eq!(reconnect_delay(POLL_POLICY, 1), Duration::from_secs(1));
    assert_eq!(
        reconnect_delay(POLL_POLICY, u32::MAX),
        Duration::from_secs(15)
    );
}

fn operation(
    id: &str,
    kind: &str,
    state: &str,
    app: &str,
    mode: &str,
    storage: &Value,
    bundle: Option<&str>,
) -> Value {
    json!({
        "operation_id":id,"kind":kind,"state":state,"app_id":app,
        "running":state != "INTERRUPTED", "cancel_requested":false,
        "detail_json":{"app_id":app,"mode":mode,"bundle_id":bundle,
            "retry":{"params":{"app_id":app,"mode":mode,"bundle_id":bundle,
                "client_storage_identity":storage}}}
    })
}

#[test]
fn reattach_requires_kind_mode_app_bundle_provider_and_cache_policy() {
    let storage = json!({"provider":"google_drive","backend_type":"drive","remote_name":"profile-1","root":"repo"});
    let request = OperationMatch {
        kind: "backup",
        app_id: "app",
        bundle_id: None,
        mode: "personal_state",
        storage_identity: &storage,
        regenerable_caches: "include",
    };
    let good = operation(
        "correct",
        "backup",
        "UPLOADING",
        "app",
        "personal_state",
        &storage,
        None,
    );
    assert_eq!(
        select_existing_operation(&[good.clone()], &request, None),
        Some(ExistingOperation::Running("correct".into()))
    );
    for path in [
        "kind",
        "app_id",
        "mode",
        "client_storage_identity",
        "regenerable_caches",
    ] {
        let mut bad = good.clone();
        if path == "kind" {
            bad["kind"] = "restore".into();
        } else {
            bad["detail_json"]["retry"]["params"][path] = "different".into();
        }
        assert_eq!(
            select_existing_operation(&[bad], &request, None),
            None,
            "{path} must match"
        );
    }
    let mut excluded = good.clone();
    excluded["detail_json"]["regenerable_caches"] = "exclude".into();
    assert_eq!(
        select_existing_operation(&[excluded.clone()], &request, None),
        None
    );
    let exclude_request = OperationMatch {
        regenerable_caches: "exclude",
        ..request
    };
    assert_eq!(
        select_existing_operation(&[excluded], &exclude_request, None),
        Some(ExistingOperation::Running("correct".into()))
    );
    assert_eq!(
        select_existing_operation(&[good], &exclude_request, None),
        None
    );
    let restore = OperationMatch {
        kind: "restore",
        bundle_id: Some("bundle-1"),
        ..request
    };
    let wrong_bundle = operation(
        "wrong",
        "restore",
        "DOWNLOADING",
        "app",
        "personal_state",
        &storage,
        Some("bundle-2"),
    );
    assert_eq!(
        select_existing_operation(&[wrong_bundle], &restore, None),
        None
    );
}

#[test]
fn running_precedes_interrupted_and_active_id_precedes_a_newer_duplicate() {
    let storage = json!({"provider":"local","root":"repo"});
    let request = OperationMatch {
        kind: "backup",
        app_id: "*",
        bundle_id: None,
        mode: "complete",
        storage_identity: &storage,
        regenerable_caches: "include",
    };
    let interrupted = operation(
        "old",
        "backup",
        "INTERRUPTED",
        "_all",
        "complete_application",
        &storage,
        None,
    );
    let running = operation(
        "new",
        "backup",
        "UPLOADING",
        "*",
        "complete_application",
        &storage,
        None,
    );
    let active = operation(
        "active",
        "backup",
        "UPLOADING",
        "*",
        "complete_application",
        &storage,
        None,
    );
    assert_eq!(
        select_existing_operation(&[interrupted.clone()], &request, None),
        Some(ExistingOperation::Interrupted("old".into()))
    );
    assert_eq!(
        select_existing_operation(
            &[interrupted, running.clone(), active.clone()],
            &request,
            Some("active")
        ),
        Some(ExistingOperation::Running("active".into()))
    );
    assert_eq!(
        select_existing_operation(&[running, active], &request, None),
        Some(ExistingOperation::Running("new".into()))
    );
}

#[test]
fn legacy_unbound_running_operations_block_duplicate_submission() {
    let storage = json!({"provider":"local","root":"repo"});
    let request = OperationMatch {
        kind: "backup",
        app_id: "app",
        bundle_id: None,
        mode: "personal_state",
        storage_identity: &storage,
        regenerable_caches: "include",
    };
    let mut legacy = operation(
        "legacy",
        "backup",
        "UPLOADING",
        "app",
        "personal_state",
        &storage,
        None,
    );
    legacy["detail_json"]["retry"]["params"]
        .as_object_mut()
        .unwrap()
        .remove("client_storage_identity");
    assert_eq!(
        select_existing_operation(&[legacy.clone()], &request, None),
        Some(ExistingOperation::UnboundRunning("legacy".into()))
    );
    legacy["running"] = false.into();
    legacy["state"] = "INTERRUPTED".into();
    assert_eq!(select_existing_operation(&[legacy], &request, None), None);
}

#[test]
fn a_timed_out_operation_that_finished_is_observed_before_new_submission() {
    let storage = json!({"provider":"local","root":"repo"});
    let request = OperationMatch {
        kind: "backup",
        app_id: "app",
        bundle_id: None,
        mode: "personal_state",
        storage_identity: &storage,
        regenerable_caches: "include",
    };
    for state in ["COMPLETED", "FAILED", "CANCELLED"] {
        let finished = operation(
            "original",
            "backup",
            state,
            "app",
            "personal_state",
            &storage,
            None,
        );
        assert_eq!(
            select_existing_operation(&[finished.clone()], &request, Some("original")),
            Some(ExistingOperation::Tracked("original".into()))
        );
        assert_eq!(
            select_existing_operation(&[finished], &request, Some("other-id")),
            None
        );
    }
    let mut cancelling = operation(
        "original",
        "backup",
        "UPLOADING",
        "app",
        "personal_state",
        &storage,
        None,
    );
    cancelling["cancel_requested"] = true.into();
    assert_eq!(
        select_existing_operation(&[cancelling], &request, None),
        Some(ExistingOperation::Running("original".into()))
    );
}
