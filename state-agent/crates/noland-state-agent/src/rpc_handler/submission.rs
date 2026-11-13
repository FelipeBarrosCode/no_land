//! Idempotent submission IDs survive a lost initial SSH response, including an
//! operation completing before the client reconnects. A replay never queues work.
use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_state_core::{OperationRecord, Result, StateError};
use noland_state_db::StateDb;
use serde_json::{json, Value};
use uuid::Uuid;

// Held only through synchronous lookup/persistence/spawn, never through transfer
// or an await. It closes the check/create race between concurrent RPC clients.
pub(super) static CREATION: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

pub(super) enum Claim {
    Replay(OperationRecord),
    New {
        operation_id: Uuid,
        binding: Option<String>,
    },
}

pub(super) fn resolve(
    db: &StateDb,
    params: &Value,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    kind: &str,
    selection: &Value,
    retry: Option<Uuid>,
) -> Result<Claim> {
    super::retry::validate_client_storage_identity(params, session)?;
    let requested = match params.get("client_operation_id") {
        None => None,
        Some(Value::String(raw)) => Some(
            Uuid::parse_str(raw)
                .map_err(|_| StateError::Invalid("client_operation_id must be a UUID".into()))?,
        ),
        Some(_) => {
            return Err(StateError::Invalid(
                "client_operation_id must be a UUID".into(),
            ))
        }
    };
    if requested
        .zip(retry)
        .is_some_and(|(requested, retry)| requested != retry)
    {
        return Err(StateError::Invalid(
            "retry operation ID differs from its original submission".into(),
        ));
    }
    let operation_id = retry.or(requested).unwrap_or_else(Uuid::new_v4);
    // Short-lived credentials/session IDs deliberately do not affect a replay.
    // The repository, selection, operation kind, and vault key always do.
    let binding = blake3::keyed_hash(
        &master.0,
        &serde_json::to_vec(&json!({
            "domain": "noland-operation-submission-v1",
            "kind": kind,
            "repository": session.storage_identity(),
            "selection": selection,
        }))?,
    )
    .to_hex()
    .to_string();
    if let Some(existing) = db.get_operation(operation_id)? {
        if retry.is_some() {
            super::retry::validate_retry_selection(&existing, selection)?;
            if let Some(identity) =
                super::retry::matching_params(&existing).get("client_storage_identity")
            {
                super::retry::validate_client_storage_identity(
                    &json!({"client_storage_identity": identity}),
                    session,
                )?;
            }
        }
        let stored = existing
            .detail_json
            .get("submission_binding")
            .and_then(Value::as_str);
        if existing.kind != kind
            || (requested.is_some() || stored.is_some()) && stored != Some(binding.as_str())
        {
            return Err(StateError::Invalid(format!(
                "operation {operation_id} belongs to a different submission"
            )));
        }
        if retry.is_none() {
            if requested.is_none() || stored.is_none() {
                return Err(StateError::Invalid(format!(
                    "operation {operation_id} already exists"
                )));
            }
            return Ok(Claim::Replay(existing));
        }
        return Ok(Claim::New {
            operation_id,
            binding: stored.map(|_| binding),
        });
    }
    if retry.is_some() {
        return Err(StateError::NotFound(operation_id.to_string()));
    }
    Ok(Claim::New {
        operation_id,
        binding: requested.map(|_| binding),
    })
}

pub(super) fn bind_details(
    mut detail: Value,
    operation_id: Uuid,
    binding: Option<String>,
) -> Value {
    if let Some(binding) = binding {
        detail["submission_binding"] = json!(binding);
        detail["client_operation_id"] = json!(operation_id);
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use noland_state_core::AppId;

    fn session() -> EphemeralRcloneSession {
        EphemeralRcloneSession {
            operation_id: "credentials-one".into(),
            provider: "local".into(),
            backend_type: "local".into(),
            remote_name: "store".into(),
            root: "/repository".into(),
            config_ini: "[store]\ntype = local\n".into(),
            expires_at_unix: 0,
            upload_concurrency: None,
        }
    }

    fn persist(db: &StateDb, claim: Claim, state: &str, selection: &Value) -> Uuid {
        let Claim::New {
            operation_id,
            binding,
        } = claim
        else {
            panic!("new submission expected")
        };
        db.upsert_operation(&OperationRecord {
            operation_id,
            kind: "restore".into(),
            app_id: Some(AppId::desktop("game")),
            state: state.into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: bind_details(selection.clone(), operation_id, binding),
        })
        .unwrap();
        operation_id
    }

    #[test]
    fn a_lost_submission_response_replays_terminal_state_without_resetting_it() {
        let db = StateDb::open_in_memory().unwrap();
        let master = MasterKey::generate();
        let session = session();
        let id = Uuid::new_v4();
        let params = json!({"client_operation_id": id});
        let selection =
            json!({"app_id":"game", "bundle_id":"bundle", "mode":"complete_application"});
        for state in [
            "QUEUED",
            "DOWNLOADING",
            "COMPLETED",
            "FAILED",
            "CANCELLED",
            "INTERRUPTED",
        ] {
            if db.get_operation(id).unwrap().is_none() {
                persist(
                    &db,
                    resolve(&db, &params, &session, &master, "restore", &selection, None).unwrap(),
                    state,
                    &selection,
                );
            } else {
                let mut operation = db.get_operation(id).unwrap().unwrap();
                operation.state = state.into();
                db.upsert_operation(&operation).unwrap();
            }
            let Claim::Replay(operation) =
                resolve(&db, &params, &session, &master, "restore", &selection, None).unwrap()
            else {
                panic!("a repeated ID must never start another restore")
            };
            assert_eq!(operation.operation_id, id);
            assert_eq!(operation.state, state);
            assert_eq!(db.get_operation(id).unwrap().unwrap().state, state);
        }
    }

    #[test]
    fn replay_is_bound_to_selection_repository_and_vault_but_not_expiring_credentials() {
        let db = StateDb::open_in_memory().unwrap();
        let master = MasterKey::generate();
        let mut session = session();
        let params = json!({"client_operation_id": Uuid::new_v4()});
        let selection = json!({"app_id":"game", "bundle_id":"one", "mode":"complete_application", "regenerable_caches":"include"});
        persist(
            &db,
            resolve(&db, &params, &session, &master, "restore", &selection, None).unwrap(),
            "COMPLETED",
            &selection,
        );
        session.operation_id = "new-credentials".into();
        session.expires_at_unix = 123456;
        assert!(matches!(
            resolve(&db, &params, &session, &master, "restore", &selection, None).unwrap(),
            Claim::Replay(_)
        ));
        for (field, value) in [
            ("app_id", "another"),
            ("bundle_id", "two"),
            ("mode", "personal_state"),
            ("regenerable_caches", "exclude"),
        ] {
            let mut changed = selection.clone();
            changed[field] = json!(value);
            assert!(resolve(&db, &params, &session, &master, "restore", &changed, None).is_err());
        }
        assert!(resolve(
            &db,
            &params,
            &session,
            &MasterKey::generate(),
            "restore",
            &selection,
            None
        )
        .is_err());
        assert!(resolve(&db, &params, &session, &master, "backup", &selection, None).is_err());
        session.root = "/another-repository".into();
        assert!(resolve(&db, &params, &session, &master, "restore", &selection, None).is_err());
    }

    #[test]
    fn concurrent_submission_claims_queue_once() {
        let db = std::sync::Arc::new(StateDb::open_in_memory().unwrap());
        let master = MasterKey::generate();
        let params = json!({"client_operation_id": Uuid::new_v4()});
        let created = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let _guard = CREATION.lock();
                    let selection = json!({"app_id":"game"});
                    let claim = resolve(
                        &db,
                        &params,
                        &session(),
                        &master,
                        "restore",
                        &selection,
                        None,
                    )
                    .unwrap();
                    if matches!(claim, Claim::New { .. }) {
                        persist(&db, claim, "QUEUED", &selection);
                        created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(created.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn invalid_submission_ids_and_mismatched_retry_ids_are_rejected() {
        let db = StateDb::open_in_memory().unwrap();
        let master = MasterKey::generate();
        for value in [Value::Null, json!(true), json!("invalid")] {
            assert!(resolve(
                &db,
                &json!({"client_operation_id":value}),
                &session(),
                &master,
                "backup",
                &json!({}),
                None
            )
            .is_err());
        }
        assert!(resolve(
            &db,
            &json!({"client_operation_id":Uuid::new_v4()}),
            &session(),
            &master,
            "backup",
            &json!({}),
            Some(Uuid::new_v4())
        )
        .is_err());
    }

    #[tokio::test]
    async fn rpc_replays_a_completed_restore_without_starting_any_worker() {
        use crate::{rpc_handler::AgentRpc, AgentConfig, StateAgent};
        use noland_rpc::{RpcHandler, RpcRequest};
        let root = std::env::temp_dir().join(format!("noland-submission-{}", Uuid::new_v4()));
        let agent =
            std::sync::Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
        let master = MasterKey::generate();
        let mut session = session();
        session.root = root.join("remote").to_string_lossy().into_owned();
        let id = Uuid::new_v4();
        let app = AppId::desktop("game");
        let bundle = Uuid::new_v4();
        let selection = json!({"app_id": app, "bundle_id": bundle, "mode": "complete_application"});
        let mut params = selection.clone();
        params["client_operation_id"] = json!(id);
        params["session"] = serde_json::to_value(&session).unwrap();
        params["master_key_hex"] = json!(master
            .0
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>());
        persist(
            &agent.db,
            resolve(
                &agent.db, &params, &session, &master, "restore", &selection, None,
            )
            .unwrap(),
            "COMPLETED",
            &selection,
        );
        drop(agent);
        let agent =
            std::sync::Arc::new(StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap());
        let response = AgentRpc(agent.clone())
            .handle(&RpcRequest {
                id: "submission-test".into(),
                method: "StartRestore".into(),
                params,
            })
            .await
            .unwrap();
        assert_eq!(response["operation_id"], id.to_string());
        assert_eq!(response["state"], "COMPLETED");
        assert!(!agent.operations.is_running(id));
        assert!(!root.join("remote").exists());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }
}
