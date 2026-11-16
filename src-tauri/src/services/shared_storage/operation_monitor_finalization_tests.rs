use super::*;
use std::{collections::VecDeque, future};

fn sample(stage: &str, elapsed: u64) -> Value {
    json!({"operation_id":"backup", "state":"COMMITTING", "running":true, "updated_at":elapsed,
        "progress":{"phase":"COMMITTING", "completed_units":0, "total_units":0,
            "detail_json":{"streaming_pipeline":{"capture_complete":true,"prepared_bytes":0},
                "total_packs":0, "bytes_transferred":0,
                "finalization":{"version":1,"stage":stage,"started_at":format!("2026-11-15T22:55:{:02}Z", elapsed % 60),
                    "stages":{"publishing_manifest":{"elapsed_ms":elapsed,"outcome":"completed",
                        "storage_io":{"remote_stat_calls":elapsed},"metadata_reads":{"bytes":elapsed}}}}}}})
}

#[tokio::test]
async fn stage_changes_emit_but_sampled_timings_and_metrics_do_not() {
    let first = sample("publishing_manifest", 1);
    let next_poll = sample("publishing_manifest", 2);
    assert_eq!(meaningful_status(&first), meaningful_status(&next_poll));
    let next_stage = sample("publishing_catalog", 3);
    assert_ne!(meaningful_status(&first), meaningful_status(&next_stage));
    let mut failed = first.clone();
    failed["progress"]["detail_json"]["finalization"]["stages"]["publishing_manifest"]["outcome"] =
        json!("failed");
    assert_ne!(meaningful_status(&first), meaningful_status(&failed));
    let mut checkpoint = first.clone();
    checkpoint["state"] = json!("CHECKPOINTING");
    assert_ne!(meaningful_status(&first), meaningful_status(&checkpoint));
    let mut replies = VecDeque::from([
        first,
        next_poll,
        next_stage,
        json!({"operation_id":"backup","state":"COMPLETED","running":false}),
    ]);
    let mut events = Vec::new();
    let result = monitor_with_policy(
        "backup",
        Duration::from_secs(1),
        PollPolicy {
            interval: Duration::from_millis(1),
            idle_max: Duration::from_millis(2),
            ..POLL_POLICY
        },
        |_| future::ready(Ok::<_, ()>(replies.pop_front().unwrap())),
        |status| events.push(status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["state"], "COMMITTING");
    assert_eq!(
        events[1]["progress"]["detail_json"]["finalization"]["stage"],
        "publishing_catalog"
    );
    assert!(replies.is_empty(), "zero packs is not terminal completion");
    assert_eq!(result["state"], "COMPLETED");
}

#[test]
fn finalization_survives_same_operation_fallback_but_not_retry_or_other_operations() {
    let previous = sample("verifying_backup_history", 100);
    let expected = &previous["progress"]["detail_json"]["finalization"];
    let mut fallback = json!({"operation_id":"backup","state":"COMMITTING"});
    retain_transfer_progress(&mut fallback, Some(&previous));
    assert_eq!(
        &fallback["progress"]["detail_json"]["finalization"],
        expected
    );
    assert_eq!(
        fallback["progress"]["detail_json"]["streaming_pipeline"]["capture_complete"],
        true
    );
    let reconnect = connection_status(
        "backup",
        Some(&previous),
        "RECONNECTING",
        "Checking the same operation".into(),
    );
    assert_eq!(
        &reconnect["progress"]["detail_json"]["finalization"],
        expected
    );
    for state in ["QUEUED", "REQUESTED"] {
        let mut retry = json!({"operation_id":"backup","state":state});
        retain_transfer_progress(&mut retry, Some(&previous));
        assert!(retry
            .pointer("/progress/detail_json/finalization")
            .is_none());
    }
    let mut other = json!({"operation_id":"different","state":"COMMITTING"});
    retain_transfer_progress(&mut other, Some(&previous));
    assert!(other
        .pointer("/progress/detail_json/finalization")
        .is_none());
    assert!(
        connection_status("different", Some(&previous), "RECONNECTING", String::new())
            .pointer("/progress/detail_json/finalization")
            .is_none()
    );
    let mut cleared = json!({"operation_id":"backup","state":"CAPTURING_AND_UPLOADING",
        "progress":{"detail_json":{"finalization":null}}});
    retain_transfer_progress(&mut cleared, Some(&previous));
    assert!(cleared["progress"]["detail_json"]["finalization"].is_null());
    let mut capturing = json!({"operation_id":"backup","state":"CAPTURING_AND_UPLOADING",
        "progress":{"detail_json":{"streaming_pipeline":{"capture_complete":false}}}});
    retain_transfer_progress(&mut capturing, Some(&previous));
    assert!(capturing
        .pointer("/progress/detail_json/finalization")
        .is_none());
    let archived = json!({"version":1,"stage":"releasing_repository","stages":{}});
    let mut terminal = json!({"operation_id":"backup","state":"COMPLETED",
        "detail_json":{"finalization":archived}});
    retain_transfer_progress(&mut terminal, Some(&previous));
    assert_eq!(
        terminal["progress"]["detail_json"]["finalization"],
        archived
    );
}
