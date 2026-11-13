use super::*;

#[test]
fn projections_are_small_atomic_and_legacy_rows_upgrade_without_changing_results() {
    let db = StateDb::open_in_memory().unwrap();
    let mut op = OperationRecord {
        operation_id: Uuid::new_v4(),
        kind: "backup".into(),
        app_id: Some(AppId("game".into())),
        state: "QUEUED".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_error: None,
        detail_json: json!({"mode":"complete_application", "result":"x".repeat(5*1024*1024),
            "retry":{"params":{"app_id":"game","session":{"config_ini":"never expose"}}}}),
    };
    db.upsert_operation(&op).unwrap();
    assert_eq!(db.unfinished_operation_count().unwrap(), 1);
    assert_eq!(
        db.projected_operation(op.operation_id, false)
            .unwrap()
            .unwrap()
            .detail_json,
        json!({})
    );
    let expected = db
        .projected_operation(op.operation_id, true)
        .unwrap()
        .unwrap();
    assert!(expected.detail_json.to_string().len() < 512);
    assert!(!expected.detail_json.to_string().contains("never expose"));
    db.lock()
        .unwrap()
        .execute("DELETE FROM operation_projections", [])
        .unwrap();
    let legacy = db
        .projected_operation(op.operation_id, true)
        .unwrap()
        .unwrap();
    assert_eq!(expected.detail_json, legacy.detail_json);
    assert_eq!(
        db.recent_matching_operations(op.app_id.as_ref(), 10)
            .unwrap()
            .len(),
        1
    );
    op.state = "FAILED".into();
    op.detail_json["mode"] = json!("personal_state");
    db.upsert_operation(&op).unwrap();
    let updated = db
        .projected_operation(op.operation_id, true)
        .unwrap()
        .unwrap();
    assert_eq!(updated.state, "FAILED");
    assert_eq!(db.unfinished_operation_count().unwrap(), 0);
    assert_eq!(updated.detail_json["mode"], "personal_state");
    // Progress must not even deserialize a legacy invalid detail document.
    db.lock()
        .unwrap()
        .execute("UPDATE operations SET detail_json='invalid json'", [])
        .unwrap();
    assert_eq!(
        db.projected_operation(op.operation_id, false)
            .unwrap()
            .unwrap()
            .state,
        "FAILED"
    );
    // Older writers must invalidate cached identity rather than serve stale data.
    db.lock()
        .unwrap()
        .execute(
            "UPDATE operations SET detail_json=?1",
            [json!({"mode":"legacy-updated"}).to_string()],
        )
        .unwrap();
    assert_eq!(
        db.projected_operation(op.operation_id, true)
            .unwrap()
            .unwrap()
            .detail_json["mode"],
        "legacy-updated"
    );
}
