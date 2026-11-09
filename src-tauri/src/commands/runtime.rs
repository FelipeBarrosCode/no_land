use crate::{
    errors::{AppError, FrontendError},
    services::{
        app_context::AppContext,
        runtime_manager::{self, RuntimeStatus},
    },
};
use tauri::State;

#[tauri::command]
pub async fn get_instance_runtime_status(
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<RuntimeStatus, FrontendError> {
    let remote = super::build_remote_exec_for_instance(context.inner(), instance_id).await?;
    Ok(runtime_manager::status(&remote, &context.config.audio_target_user).await?)
}

#[tauri::command]
pub async fn instance_runtime_action(
    instance_id: u64,
    action: String,
    file: Option<String>,
    app_id: Option<String>,
    artifact_sha256: Option<String>,
    context: State<'_, AppContext>,
) -> Result<serde_json::Value, FrontendError> {
    let _action = crate::services::instance_lifecycle::InstanceLifecycleService::begin_action(
        context.inner(),
        instance_id,
        "runtime action",
    )
    .await?;
    let remote = super::build_remote_exec_for_instance(context.inner(), instance_id).await?;
    let user = &context.config.audio_target_user;
    // Retained as a wire-compatible argument for older preview clients. Standard
    // installation now happens during provisioning, with no signed manifest.
    let _ = artifact_sha256;
    if action == "enable" {
        runtime_manager::enable(&remote, user).await?;
        return Ok(runtime_manager::run(&remote, user, &["create-shortcut"]).await?);
    }
    let args = match action.as_str() {
        "plan" => vec!["plan"],
        "install" => vec!["ensure", "--approve"],
        "cancel" => vec!["cancel"],
        "doctor" => vec!["doctor", "--json"],
        "rollback" => vec!["rollback"],
        "register" => vec![
            "register",
            "--file",
            file.as_deref()
                .ok_or_else(|| AppError::InvalidInput("Select a remote application path".into()))?,
        ],
        "launch" => vec![
            "launch",
            "--app-id",
            app_id
                .as_deref()
                .ok_or_else(|| AppError::InvalidInput("Select a registered app".into()))?,
        ],
        "shortcut" => vec![
            "create-shortcut",
            "--app-id",
            app_id
                .as_deref()
                .ok_or_else(|| AppError::InvalidInput("Select a registered app".into()))?,
        ],
        _ => return Err(AppError::InvalidInput("Unsupported runtime operation".into()).into()),
    };
    Ok(runtime_manager::run(&remote, user, &args).await?)
}
