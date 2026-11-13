use super::super::operation_monitor::monitor_operation;
use super::*;
use std::cell::RefCell;
use std::future;

#[tokio::test]
async fn definitive_rejection_clears_its_handle_but_transport_ambiguity_keeps_it() {
    for rejected in [true, false] {
        let memory = SubmissionMemory::new();
        let fingerprint = request_fingerprint("seal", &params());
        let other = request_fingerprint("backup", &params());
        assert!(memory.remember(42, "other-id", other));
        assert!(memory.remember(43, "seal-id", fingerprint));
        let active = RefCell::new(None);
        let mut events = Vec::new();
        let result = submit_with_policy(
            "seal-id".into(),
            FAST,
            |id| {
                memory.remember(42, &id, fingerprint);
                *active.borrow_mut() = Some(id);
                future::ready(())
            },
            |_| {
                future::ready(Err(if rejected {
                    SubmissionAttemptError::Rejected("invalid request".into())
                } else {
                    SubmissionAttemptError::Unavailable
                }))
            },
            |event| events.push(event.clone()),
        )
        .await;
        let cleared = clear_rejected_submission(&result, &memory, 42, "seal-id", async {
            *active.borrow_mut() = None;
        })
        .await;
        assert_eq!(cleared, rejected);
        assert_eq!(active.borrow().is_some(), !rejected);
        assert_eq!(memory.matches(42, "seal-id", &fingerprint), !rejected);
        assert_eq!(memory.lookup(42, &other).as_deref(), Some("other-id"));
        assert_eq!(memory.lookup(43, &fingerprint).as_deref(), Some("seal-id"));
        assert_eq!(
            events
                .iter()
                .any(|event| event["state"] == "MONITORING_PAUSED"),
            !rejected
        );
    }
}

#[test]
fn seal_replay_identity_binds_mode_repository_and_vault_not_ephemeral_credentials() {
    let original = params();
    let fingerprint = request_fingerprint("seal", &original);
    let mut refreshed = original.clone();
    refreshed["session"]["config_ini"] = "new credentials".into();
    refreshed["session"]["operation_id"] = "new session".into();
    assert_eq!(request_fingerprint("seal", &refreshed), fingerprint);
    for field in ["mode", "master_key_hex"] {
        let mut changed = original.clone();
        changed[field] = "other".into();
        assert_ne!(request_fingerprint("seal", &changed), fingerprint);
    }
    let mut changed = original;
    changed["session"]["root"] = "/different".into();
    assert_ne!(request_fingerprint("seal", &changed), fingerprint);
}

const FAST: RetryPolicy = RetryPolicy {
    attempts: 3,
    timeout: Duration::from_millis(100),
    initial: Duration::from_millis(1),
    maximum: Duration::from_millis(4),
};

fn params() -> Value {
    json!({
        "app_id":"game", "bundle_id":"bundle", "mode":"personal_state",
        "performance_mode":"balanced", "regenerable_caches":"include", "master_key_hex":"ab12",
        "session":{"provider":"sftp", "backend_type":"sftp", "remote_name":"profile", "root":"/repo",
            "operation_id":"ephemeral-session", "expires_at_unix":1, "config_ini":"ephemeral"},
    })
}

#[tokio::test]
async fn interleaved_ambiguous_admissions_resume_exact_ids_on_each_instance() {
    let memory = SubmissionMemory::new();
    let first = params();
    let mut second = first.clone();
    second["app_id"] = "another-game".into();
    let requests = [
        (7, "earlier", first.clone()),
        (7, "later", second),
        (8, "elsewhere", first),
    ];
    let mut executed = std::collections::BTreeSet::new();
    for (instance, original_id, request) in &requests {
        let fingerprint = request_fingerprint("backup", request);
        let outcome = submit_with_policy(
            (*original_id).into(),
            RetryPolicy {
                attempts: 1,
                ..FAST
            },
            |id| {
                assert!(memory.remember(*instance, &id, fingerprint));
                future::ready(())
            },
            |id| {
                assert!(executed.insert((*instance, id)));
                future::ready(Err(SubmissionAttemptError::Unavailable)) // admitted, reply lost
            },
            |_| {},
        )
        .await;
        assert!(matches!(outcome, Err(SubmissionFailure::Unconfirmed(_))));
    }
    // Resume the earlier request after a newer same-kind request on the same
    // instance and an identical request on another instance have been retained.
    for index in [0, 2, 1] {
        let (instance, original_id, request) = &requests[index];
        let fingerprint = request_fingerprint("backup", request);
        let resumed_id = memory
            .lookup(*instance, &fingerprint)
            .expect("retained replay authority");
        assert_eq!(resumed_id, *original_id);
        let mut replay_calls = 0;
        let receipt = submit_with_policy(
            resumed_id,
            FAST,
            |id| {
                assert!(memory.remember(*instance, &id, fingerprint));
                future::ready(())
            },
            |id| {
                replay_calls += 1;
                assert!(
                    executed.contains(&(*instance, id.clone())),
                    "no fresh execution ID is allowed"
                );
                future::ready(Ok(json!({"operation_id":id,"state":"COMPLETED"})))
            },
            |_| {},
        )
        .await
        .unwrap();
        monitor_operation(
            &receipt.operation_id,
            Duration::from_secs(1),
            receipt.status,
            |_| future::ready(Err::<Value, _>("status unavailable")),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(replay_calls, 1);
        memory.forget(*instance, &receipt.operation_id);
    }
    assert_eq!(executed.len(), 3);
}

#[test]
fn earlier_late_completion_is_selected_and_cleanup_cannot_erase_newer_handles() {
    use super::super::operation_monitor::{
        select_existing_operation, ExistingOperation, OperationMatch,
    };
    let memory = SubmissionMemory::new();
    let first = params();
    let mut second = first.clone();
    second["app_id"] = "another-game".into();
    let first_fp = request_fingerprint("backup", &first);
    let second_fp = request_fingerprint("backup", &second);
    assert!(memory.remember(7, "earlier", first_fp));
    assert!(memory.remember(7, "later", second_fp));
    assert!(memory.remember(8, "earlier", first_fp));
    let identity =
        json!({"provider":"sftp","backend_type":"sftp","remote_name":"profile","root":"/repo"});
    let record = |id: &str, app: &str, state: &str, running: bool| {
        json!({
            "operation_id":id,"kind":"backup","app_id":app,"state":state,"running":running,
            "detail_json":{"mode":"personal_state","client_storage_identity":identity}
        })
    };
    let operations = [
        record("later", "another-game", "UPLOADING", true),
        record("earlier", "game", "COMPLETED", false),
    ];
    let request = OperationMatch {
        kind: "backup",
        app_id: "game",
        bundle_id: None,
        mode: "personal_state",
        storage_identity: &identity,
        regenerable_caches: "include",
    };
    let preferred = memory.lookup(7, &first_fp);
    assert_eq!(
        select_existing_operation(&operations, &request, preferred.as_deref()),
        Some(ExistingOperation::Tracked("earlier".into()))
    );
    assert_eq!(
        select_existing_operation(&operations, &request, Some("later")),
        None,
        "the latest same-kind handle would hide the completion"
    );
    memory.forget(7, "earlier");
    assert_eq!(memory.lookup(7, &second_fp).as_deref(), Some("later"));
    assert_eq!(memory.lookup(8, &first_fp).as_deref(), Some("earlier"));
    assert!(memory.remember(7, "new-request", first_fp));
    memory.forget(7, "earlier"); // a late duplicate monitor result
    assert_eq!(memory.lookup(7, &first_fp).as_deref(), Some("new-request"));
    let mut unbound = record("new-request", "game", "UPLOADING", true);
    unbound["detail_json"]
        .as_object_mut()
        .unwrap()
        .remove("client_storage_identity");
    assert_eq!(
        select_existing_operation(&[unbound], &request, Some("new-request")),
        Some(ExistingOperation::UnboundRunning("new-request".into()))
    );
}

#[test]
fn retained_ids_are_immutable_and_never_silently_evicted() {
    let memory = SubmissionMemory::new();
    let original = request_fingerprint("seal", &params());
    let mut changed = params();
    changed["master_key_hex"] = "different-vault".into();
    let changed = request_fingerprint("seal", &changed);
    assert!(memory.remember(7, "seal", original));
    assert!(memory.remember(7, "seal-twin", original));
    assert!(!memory.remember(7, "seal", changed));
    assert!(memory.has_conflicting_binding(7, "seal", &changed));
    assert_eq!(memory.lookup(7, &original).as_deref(), Some("seal"));
    assert_eq!(memory.lookup(7, &changed), None);
    assert!(memory.remember(8, "seal", changed));
    for index in 0..256u64 {
        let fingerprint = request_fingerprint("backup", &json!({"app_id":format!("app-{index}")}));
        assert!(memory.remember(index % 4, &format!("op-{index}"), fingerprint));
    }
    for index in 0..256u64 {
        let fingerprint = request_fingerprint("backup", &json!({"app_id":format!("app-{index}")}));
        assert_eq!(
            memory.lookup(index % 4, &fingerprint),
            Some(format!("op-{index}"))
        );
    }
    memory.forget(7, "seal");
    assert_eq!(memory.lookup(7, &original).as_deref(), Some("seal-twin"));
    assert_eq!(memory.lookup(8, &changed).as_deref(), Some("seal"));
    assert_eq!(
        SubmissionMemory::new().lookup(8, &changed),
        None,
        "restart must not recreate replay authority"
    );
}

#[tokio::test]
async fn unaccepted_attempt_then_lost_fast_completion_reply_replays_exact_id_once() {
    let active = RefCell::new(None);
    let completed = RefCell::new(None);
    let mut starts = Vec::new();
    let mut executions = 0;
    let request = params();
    let receipt = submit_with_policy(
        "client-uuid".into(),
        FAST,
        |id| {
            *active.borrow_mut() = Some(id);
            future::ready(())
        },
        |id| {
            assert_eq!(active.borrow().as_deref(), Some(id.as_str()));
            let mut sent = request.clone();
            sent["client_operation_id"] = id.clone().into();
            starts.push(sent);
            future::ready(match starts.len() {
                1 => Err(SubmissionAttemptError::Unavailable), // Never reached the server.
                2 => {
                    executions += 1;
                    *completed.borrow_mut() = Some(json!({"operation_id":id,"state":"COMPLETED"}));
                    Err(SubmissionAttemptError::Unavailable) // Completed, but reply was lost.
                }
                _ => Ok(completed.borrow().clone().unwrap()), // Idempotent replay.
            })
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(executions, 1);
    assert_eq!(starts.len(), 3);
    assert!(starts.iter().all(|sent| sent == &starts[0]));
    assert_eq!(receipt.operation_id, "client-uuid");
    let mut lookups = 0;
    let result = monitor_operation(
        &receipt.operation_id,
        Duration::from_secs(1),
        receipt.status,
        |_| {
            lookups += 1;
            future::ready(Err::<Value, _>("status lookup unavailable"))
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result["state"], "COMPLETED");
    assert_eq!(
        lookups, 0,
        "an authoritative replay result survives lookup outages"
    );
}

#[tokio::test]
async fn exhausted_admission_retains_matching_fingerprint_for_same_id_ui_resume() {
    let memory = SubmissionMemory::new();
    let request = params();
    let fingerprint = request_fingerprint("restore", &request);
    let mut calls = Vec::new();
    let mut events = Vec::new();
    let result = submit_with_policy(
        "original-id".into(),
        FAST,
        |id| {
            memory.remember(7, &id, fingerprint);
            future::ready(())
        },
        |id| {
            calls.push(id);
            future::ready(Err(SubmissionAttemptError::Unavailable))
        },
        |status| events.push(status.clone()),
    )
    .await;
    assert!(
        matches!(result, Err(SubmissionFailure::Unconfirmed(message)) if message.contains("original-id"))
    );
    assert_eq!(calls, vec!["original-id"; 3]);
    assert_eq!(events.last().unwrap()["state"], "MONITORING_PAUSED");
    assert!(events.iter().all(|event| event["state"] != "FAILED"));

    let mut refreshed = request.clone();
    refreshed["session"]["operation_id"] = "fresh-session".into();
    refreshed["session"]["expires_at_unix"] = 100.into();
    refreshed["session"]["config_ini"] = "fresh-credentials".into();
    let fingerprint = request_fingerprint("restore", &refreshed);
    // Even when the UI's status lookup fails, this local match authorizes only
    // the retained ID, never a newly generated UUID or changed request.
    let lookup: Result<Value, ()> = Err(());
    assert!(lookup.is_err() && memory.matches(7, "original-id", &fingerprint));
    let resumed = submit_with_policy(
        "original-id".into(),
        FAST,
        |id| {
            memory.remember(7, &id, fingerprint);
            future::ready(())
        },
        |id| future::ready(Ok(json!({"operation_id":id, "state":"QUEUED"}))),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(resumed.operation_id, "original-id");
}

#[test]
fn local_replay_authority_covers_selection_provider_vault_instance_and_id() {
    let request = params();
    let memory = SubmissionMemory::new();
    let fingerprint = request_fingerprint("restore", &request);
    memory.remember(7, "original", fingerprint);
    for field in [
        "app_id",
        "bundle_id",
        "mode",
        "performance_mode",
        "regenerable_caches",
        "master_key_hex",
    ] {
        let mut changed = request.clone();
        changed[field] = "different".into();
        assert!(
            !memory.matches(7, "original", &request_fingerprint("restore", &changed)),
            "{field}"
        );
    }
    for field in ["provider", "backend_type", "remote_name", "root"] {
        let mut changed = request.clone();
        changed["session"][field] = "different".into();
        assert!(
            !memory.matches(7, "original", &request_fingerprint("restore", &changed)),
            "{field}"
        );
    }
    assert!(!memory.matches(8, "original", &fingerprint));
    assert!(!memory.matches(7, "another", &fingerprint));
    assert!(!memory.matches(7, "original", &request_fingerprint("backup", &request)));
    memory.forget(7, "another");
    assert!(memory.matches(7, "original", &fingerprint));
    memory.forget(7, "original");
    assert!(!memory.matches(7, "original", &fingerprint));
}

#[tokio::test]
async fn authoritative_rejection_is_not_a_transport_retry() {
    let mut attempts = 0;
    let result = submit_with_policy(
        "original".into(),
        FAST,
        |_| future::ready(()),
        |_| {
            attempts += 1;
            future::ready(Err(SubmissionAttemptError::Rejected(
                "different submission".into(),
            )))
        },
        |_| {},
    )
    .await;
    assert_eq!(attempts, 1);
    assert!(
        matches!(result, Err(SubmissionFailure::Rejected(message)) if message.contains("original"))
    );
}

#[tokio::test]
async fn incompatible_server_id_never_replaces_retained_id() {
    let active = RefCell::new(None);
    let memory = SubmissionMemory::new();
    let fingerprint = request_fingerprint("backup", &params());
    let result = submit_with_policy(
        "original".into(),
        FAST,
        |id| {
            assert!(memory.remember(7, &id, fingerprint));
            *active.borrow_mut() = Some(id);
            future::ready(())
        },
        |_| future::ready(Ok(json!({"operation_id":"different-server-id"}))),
        |_| {},
    )
    .await;
    assert!(
        matches!(&result, Err(SubmissionFailure::Incompatible(message)) if message.contains("original"))
    );
    assert!(
        !clear_rejected_submission(&result, &memory, 7, "original", async {
            *active.borrow_mut() = None;
        })
        .await
    );
    assert_eq!(memory.lookup(7, &fingerprint).as_deref(), Some("original"));
    assert_eq!(active.borrow().as_deref(), Some("original"));
}

#[tokio::test]
async fn a_hung_submission_has_a_bounded_deadline_and_keeps_its_handle() {
    let active = RefCell::new(None);
    let result = submit_with_policy(
        "original".into(),
        RetryPolicy {
            timeout: Duration::from_millis(5),
            ..FAST
        },
        |id| {
            *active.borrow_mut() = Some(id);
            future::ready(())
        },
        |_| future::pending::<Result<Value, SubmissionAttemptError>>(),
        |_| {},
    )
    .await;
    assert!(matches!(result, Err(SubmissionFailure::Unconfirmed(_))));
    assert_eq!(active.borrow().as_deref(), Some("original"));
}

#[tokio::test]
async fn terminal_replay_results_never_trigger_an_execution_retry() {
    for state in ["COMPLETED", "FAILED", "CANCELLED", "INTERRUPTED"] {
        let mut attempts = 0;
        let receipt = submit_with_policy(
            "original".into(),
            FAST,
            |_| future::ready(()),
            |id| {
                attempts += 1;
                future::ready(Ok(json!({"operation_id":id, "state":state})))
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(receipt.status.unwrap()["state"], state);
    }
}
