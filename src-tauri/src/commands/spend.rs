use chrono::Utc;
use tauri::State;

use crate::{
    errors::FrontendError,
    models::spend::{BudgetSettings, SpendSummary},
    services::app_context::AppContext,
};

#[tauri::command]
pub async fn get_spend_summary(
    context: State<'_, AppContext>,
) -> Result<SpendSummary, FrontendError> {
    Ok(context.state.read().await.spend.summary(Utc::now()))
}

#[tauri::command]
pub async fn update_budget_settings(
    settings: BudgetSettings,
    context: State<'_, AppContext>,
) -> Result<SpendSummary, FrontendError> {
    let settings = settings.validated();
    let state = context
        .update_state(|state| {
            let previous = state.spend.settings.clone();
            // Raising the budget re-arms this month's alerts at the new level.
            if settings.monthly_budget_usd > previous.monthly_budget_usd
                || settings.warn_at_percent > previous.warn_at_percent
            {
                state.spend.alerts.warned_month = None;
                state.spend.alerts.exceeded_month = None;
            }
            state.spend.settings = settings;
        })
        .await?;
    Ok(state.spend.summary(Utc::now()))
}
