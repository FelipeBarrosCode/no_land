//! Exact cross-client command seam. No credentials or HTTP clients are exposed to the webview.
use serde_json::{json, Value};
use tauri::State;

use crate::{
    errors::FrontendError, models::feedback::EnrollmentRequest, services::app_context::AppContext,
};

#[tauri::command]
pub async fn provider_creation_clear_unconfirmed(
    context: State<'_, AppContext>,
    request_id: String,
    confirmed_absent: bool,
) -> Result<Value, FrontendError> {
    crate::services::provider_creation::ProviderCreationService::clear_unconfirmed(
        context.inner(),
        &request_id,
        confirmed_absent,
    )
    .await?;
    Ok(json!({"cleared": true}))
}

#[tauri::command]
pub async fn feedback_program_status(
    context: State<'_, AppContext>,
) -> Result<Value, FrontendError> {
    Ok(context.feedback.status(context.inner()).await)
}
#[tauri::command]
pub async fn feedback_program_notices(
    context: State<'_, AppContext>,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .notices(context.inner())
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_program_enroll(
    context: State<'_, AppContext>,
    request: EnrollmentRequest,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .enroll(context.inner(), request)
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_program_consent(
    context: State<'_, AppContext>,
    enabled: bool,
    optional_context_enabled: bool,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .consent(context.inner(), enabled, optional_context_enabled)
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_program_delete(
    context: State<'_, AppContext>,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .delete(context.inner())
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_machine_summaries(
    context: State<'_, AppContext>,
    machine_ids: Vec<String>,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .machine_summaries(context.inner(), machine_ids)
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_machine_reviews(
    context: State<'_, AppContext>,
    machine_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .machine_reviews(context.inner(), machine_id, cursor, limit)
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_review_report(
    context: State<'_, AppContext>,
    review_id: String,
    reason: String,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .moderate(context.inner(), review_id, Some(reason))
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_review_block_author(
    context: State<'_, AppContext>,
    review_id: String,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .moderate(context.inner(), review_id, None)
        .await
        .map_err(Into::into)
}
#[tauri::command]
pub async fn feedback_review_eligibility(
    context: State<'_, AppContext>,
    provider_instance_id: String,
) -> Result<Value, FrontendError> {
    Ok(
        json!({"eligible": context.feedback.review_eligible(context.inner(), &provider_instance_id).await}),
    )
}
#[tauri::command]
pub async fn feedback_stage_review(
    context: State<'_, AppContext>,
    provider_instance_id: String,
    score: Option<u8>,
    comment: Option<String>,
) -> Result<Value, FrontendError> {
    context
        .feedback
        .stage_review(context.inner(), provider_instance_id, score, comment)
        .await
        .map_err(Into::into)
}
