use super::*;
use crate::deletion::{binding, ensure_idle, prepare_cloud, prepare_local};

pub(super) async fn prepare(
    agent: &Arc<StateAgent>,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    ensure_idle(agent)?;
    let _transfer = agent
        .operations
        .try_transfer_guard()
        .ok_or_else(|| StateError::Conflict("storage is busy".into()))?;
    let target = req_str(params, "target")?;
    let plan = match target.as_str() {
        "remote_pc" => prepare_local(agent, params)?,
        "cloud" => {
            let session = parse_session(params)?;
            let master = master_from_params(params, agent)?;
            let (config, _guard) =
                write_guarded_ephemeral_session(&agent.config.paths.run_root, &session)?;
            let provider = RcloneStorage::try_from_session(&session, &config)?.for_deletion();
            if let Some(paths) = params.get("selected_paths").and_then(|v| v.as_array()) {
                if paths.len() == 1 {
                    if let Some(id) = paths[0]
                        .as_str()
                        .and_then(|path| path.strip_prefix("/pending-deletions/"))
                    {
                        let id = Uuid::parse_str(id).map_err(|_| {
                            StateError::Invalid("invalid pending deletion ID".into())
                        })?;
                        let full = noland_storage::load_full_deletion(&provider, &master, id)
                            .await?
                            .ok_or_else(|| {
                                StateError::NotFound("pending deletion not found".into())
                            })?;
                        let guard = crate::repository::acquire(
                            agent,
                            &provider,
                            &master,
                            full.operation_id,
                        )
                        .await?;
                        let plan = crate::deletion::DeletionPlan::from_full(
                            full,
                            binding(&session, &master)?,
                        );
                        guard.release().await?;
                        let preview = plan.preview.clone();
                        *agent.deletion_plan.lock() = Some(plan);
                        return Ok(preview);
                    }
                }
            }
            let id = Uuid::new_v4();
            let guard = crate::repository::acquire(agent, &provider, &master, id).await?;
            let result = prepare_cloud(
                &provider,
                &master,
                params,
                binding(&session, &master)?,
                id,
                &crate::repository::owner(agent, &master)?,
            )
            .await;
            guard.release().await?;
            result?
        }
        _ => return Err(StateError::Invalid("unknown deletion target".into())),
    };
    ensure_idle(agent)?;
    let preview = plan.preview.clone();
    *agent.deletion_plan.lock() = Some(plan);
    Ok(preview)
}

pub(super) async fn start(
    agent: &Arc<StateAgent>,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    let id = req_uuid(params, "plan_id")?;
    let target = req_str(params, "target")?;
    if params.get("confirmed").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(StateError::Invalid(
            "explicit deletion confirmation is required".into(),
        ));
    }
    let cloud = if target == "cloud" {
        let session = parse_session(params)?;
        let master = master_from_params(params, agent)?;
        Some((session, master))
    } else if target == "remote_pc" {
        None
    } else {
        return Err(StateError::Invalid("unknown deletion target".into()));
    };
    let repository_binding = cloud
        .as_ref()
        .map(|(session, master)| binding(session, master))
        .transpose()?;
    let resumed = if target == "cloud"
        && agent.db.get_operation(id)?.is_some_and(|op| {
            op.kind == "delete"
                && matches!(op.state.as_str(), "FAILED" | "CANCELLED" | "INTERRUPTED")
        }) {
        let (session, master) = cloud.as_ref().expect("cloud session parsed");
        let (config, _guard) =
            write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
        let provider = RcloneStorage::try_from_session(session, &config)?.for_deletion();
        noland_storage::load_full_deletion(&provider, master, id)
            .await?
            .map(|full| {
                crate::deletion::DeletionPlan::from_full(
                    full,
                    repository_binding.clone().expect("cloud binding"),
                )
            })
    } else {
        None
    };
    let _submission = submission::CREATION.lock();
    if let Some(existing) = agent.db.get_operation(id)? {
        if existing.kind != "delete"
            || existing.detail_json["target"] != target
            || existing.detail_json["deletion_binding"].as_str() != repository_binding.as_deref()
        {
            return Err(StateError::Conflict(
                "deletion submission differs from its original preview".into(),
            ));
        }
        if resumed.is_none() || !is_retryable_state(&existing.state) {
            return serialize_operation(agent, existing, false);
        }
    }
    ensure_idle(agent)?;
    let mut pending = agent.deletion_plan.lock();
    if let Some(resumed) = resumed {
        *pending = Some(resumed);
    }
    let plan = pending.as_ref().filter(|p| p.id == id).ok_or_else(|| {
        StateError::Invalid("deletion preview is no longer available; preview again".into())
    })?;
    plan.validate(&target, repository_binding.as_deref())?;
    let mut detail = plan.preview.clone();
    detail["deletion_binding"] = json!(repository_binding);
    let retry = agent.db.get_operation(id)?.is_some();
    upsert_queued_operation(agent, id, "delete", None, "QUEUED", detail, retry)?;
    let plan = pending.take().expect("validated deletion preview");
    let task_agent = Arc::clone(agent);
    let cancelled_agent = Arc::clone(agent);
    let terminated_agent = Arc::clone(agent);
    let started = Instant::now();
    agent.operations.spawn_cancellable_on_lane(
        id,
        OperationLane::Transfer,
        async move {
            let result = async {
                plan.validate(&target, repository_binding.as_deref())?;
                if let Some((session, master)) = cloud {
                    let (config, _guard) = write_guarded_ephemeral_session(
                        &task_agent.config.paths.run_root,
                        &session,
                    )?;
                    let provider = RcloneStorage::try_from_session(&session, &config)?.for_deletion();
                    plan.run(&task_agent, Some(&provider), Some(&master)).await
                } else {
                    plan.run(&task_agent, None, None).await
                }
            }
            .await;
            match result {
                Ok(()) => mark_operation_completed(
                    &task_agent,
                    id,
                    "COMPLETED",
                    json!({"deletion_complete":true}),
                    started,
                ),
                Err(error) => mark_operation_failed(&task_agent, id, error.to_string(), started),
            }
        },
        move || mark_operation_cancelled(&cancelled_agent, id),
        move |error| mark_operation_failed(&terminated_agent, id, error, started),
    );
    Ok(json!({"operation_id":id,"state":"QUEUED"}))
}
