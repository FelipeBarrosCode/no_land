use chrono::Utc;
use tauri::{State, WebviewWindow};

use crate::{
    errors::{AppError, AppResult, FrontendError},
    models::spend::{BudgetSettings, SpendAlert, SpendSummary},
    services::app_context::AppContext,
};

fn require_main_window(label: &str) -> AppResult<()> {
    if label != "main" {
        return Err(AppError::InvalidInput(
            "Budget commands are available only in the main window.".into(),
        ));
    }
    Ok(())
}

fn validate_settings(settings: &BudgetSettings) -> AppResult<()> {
    if !settings.monthly_budget_usd.is_finite()
        || settings.monthly_budget_usd < 0.0
        || !(1..=100).contains(&settings.warn_at_percent)
        || (settings.auto_stop_at_budget && !settings.enabled())
    {
        return Err(AppError::InvalidInput(
            "Invalid monthly budget or warning threshold.".into(),
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn get_spend_summary(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<SpendSummary, FrontendError> {
    require_main_window(window.label())?;
    Ok(context.state.read().await.spend.summary(Utc::now()))
}

#[tauri::command]
pub async fn update_budget_settings(
    window: WebviewWindow,
    settings: BudgetSettings,
    context: State<'_, AppContext>,
) -> Result<SpendSummary, FrontendError> {
    require_main_window(window.label())?;
    validate_settings(&settings)?;
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

#[tauri::command]
pub async fn get_pending_spend_alerts(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<Vec<SpendAlert>, FrontendError> {
    require_main_window(window.label())?;
    Ok(context.state.read().await.spend.pending_alerts.clone())
}

#[tauri::command]
pub async fn acknowledge_spend_alert(
    window: WebviewWindow,
    id: String,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    require_main_window(window.label())?;
    uuid::Uuid::parse_str(&id)
        .map_err(|_| AppError::InvalidInput("Invalid alert identifier.".into()))?;
    context
        .update_state(|state| state.spend.acknowledge_alert(&id))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_commands_reject_secondary_and_unknown_windows() {
        assert!(require_main_window("main").is_ok());
        for label in ["moonlight-stream", "", "main-other"] {
            assert!(require_main_window(label).is_err());
        }
    }

    #[test]
    fn invalid_ipc_values_are_rejected_not_silently_disabling_budget() {
        for amount in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(validate_settings(&BudgetSettings {
                monthly_budget_usd: amount,
                ..Default::default()
            })
            .is_err());
        }
        for percent in [0, 101, 255] {
            assert!(validate_settings(&BudgetSettings {
                monthly_budget_usd: 10.0,
                warn_at_percent: percent,
                auto_stop_at_budget: false
            })
            .is_err());
        }
        assert!(validate_settings(&BudgetSettings {
            auto_stop_at_budget: true,
            ..Default::default()
        })
        .is_err());
        assert!(validate_settings(&BudgetSettings::default()).is_ok());
    }
}
