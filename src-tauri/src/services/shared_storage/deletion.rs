use super::{
    agent_handoff::wait_for_agent_operation,
    agent_runtime::{call_agent_raw, ensure_state_agent},
    operation_submission::{submit_with_id, SubmissionAttemptError, SubmissionFailure},
    shared_storage_manager::SharedStorageManager,
};
use crate::errors::{AppError, AppResult};
use crate::services::{
    app_context::{ActiveAgentOperation, AppContext},
    remote_exec::RemoteExec,
};
use serde_json::{json, Value};
use std::time::Duration;

impl SharedStorageManager {
    pub async fn deletion_request(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        target: &str,
        selected_paths: Option<&[String]>,
        plan_id: Option<&str>,
    ) -> AppResult<Value> {
        if !matches!(target, "remote_pc" | "cloud") {
            return Err(AppError::InvalidInput("Unknown deletion target".into()));
        }
        let mut params = json!({"target":target});
        if target == "cloud" {
            let (session, key) =
                Self::prepare_agent_operation(context, remote, target_user).await?;
            params["session"] = json!(session);
            params["master_key_hex"] = json!(key);
        } else {
            ensure_state_agent(remote, target_user).await?;
        }
        let Some(id) = plan_id else {
            params["selected_paths"] = json!(selected_paths.unwrap_or_default());
            return call_agent_raw(remote, "PrepareDeletion", params).await;
        };
        uuid::Uuid::parse_str(id)
            .map_err(|_| AppError::InvalidInput("Invalid deletion preview".into()))?;
        params["plan_id"] = json!(id);
        params["confirmed"] = json!(true);
        // A lost reply can only replay this exact preview/operation identity.
        let submission = submit_with_id(
            id.into(),
            |operation_id| async move {
                context
                    .retain_agent_operation(ActiveAgentOperation {
                        instance_id,
                        operation_id,
                        kind: "delete".into(),
                    })
                    .await;
            },
            |_| {
                let params = params.clone();
                async move {
                    call_agent_raw(remote, "StartDeletion", params)
                        .await
                        .map_err(|error| match error {
                            AppError::Provisioning(message)
                                if message.starts_with("state-agent StartDeletion:") =>
                            {
                                SubmissionAttemptError::Rejected(message)
                            }
                            _ => SubmissionAttemptError::Unavailable,
                        })
                }
            },
            |_| {},
        )
        .await;
        if matches!(submission, Err(SubmissionFailure::Rejected(_))) {
            context.clear_agent_operation(instance_id, id).await;
        }
        let submission = submission.map_err(|error| match error {
            SubmissionFailure::Rejected(message) => AppError::State(message),
            SubmissionFailure::Incompatible(message) | SubmissionFailure::Unconfirmed(message) => {
                AppError::Timeout(message)
            }
        })?;
        wait_for_agent_operation(
            context,
            remote,
            instance_id,
            id,
            "delete",
            None,
            None,
            Duration::from_secs(2 * 60 * 60),
            submission.status,
        )
        .await
    }
}
