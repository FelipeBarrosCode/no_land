//! Background spend accrual and budget enforcement.
//!
//! Every minute the tracker lists the account's instances, folds them into
//! the persisted [`SpendState`](crate::models::spend::SpendState) ledger and
//! emits `spend:updated`. When the monthly budget crosses its warning
//! threshold or is reached it emits `spend:alert` once per month (the
//! frontend decides whether to show an OS notification), and when auto-stop
//! is enabled it stops running instances through the normal lifecycle path,
//! which runs the pre-stop backup first.

use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

use crate::{
    models::{
        spend::{month_key, BudgetLevel, SpendObservation},
        vast::VastInstance,
    },
    services::{
        app_context::AppContext, instance_lifecycle::InstanceLifecycleService,
        vast_api::VastApiClient,
    },
};

const POLL_INTERVAL: Duration = Duration::from_secs(60);
pub const SPEND_UPDATED_EVENT: &str = "spend:updated";
pub const SPEND_ALERT_EVENT: &str = "spend:alert";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendAlert {
    /// `warning`, `exceeded`, `auto_stopped` or `auto_stop_failed`.
    pub kind: String,
    pub month_to_date_usd: f64,
    pub budget_usd: f64,
    pub instance_id: Option<u64>,
    pub message: String,
}

/// Whether a Vast status is billed at the full hourly price. Loading
/// instances are counted as running because Vast bills GPU time from start.
pub fn vast_status_is_billed_as_running(status: &str) -> bool {
    let status = status.trim().to_ascii_lowercase();
    if status.contains("inactive")
        || status.contains("stop")
        || status.contains("exit")
        || status.contains("offline")
    {
        return false;
    }
    ["running", "loading", "creating", "starting", "active", "ready"]
        .iter()
        .any(|state| status.contains(state))
}

pub fn observations_from_vast(instances: &[VastInstance]) -> Vec<SpendObservation> {
    instances
        .iter()
        .filter(|instance| !instance.status.to_ascii_lowercase().contains("destroy"))
        .map(|instance| SpendObservation {
            provider: "vast".to_string(),
            instance_id: instance.id,
            label: if instance.label.trim().is_empty() {
                format!("Instance {}", instance.id)
            } else {
                instance.label.clone()
            },
            gpu_name: instance.gpu_name.clone(),
            running: vast_status_is_billed_as_running(&instance.status),
            hourly_price: instance.hourly_price,
            storage_hourly_price: instance.storage_hourly_price,
        })
        .collect()
}

pub async fn run_spend_tracking(app: AppHandle, context: AppContext) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        tick(&app, &context).await;
    }
}

async fn tick(app: &AppHandle, context: &AppContext) {
    let api_key = context.state.read().await.credentials.vast_api_key.clone();
    if api_key.trim().is_empty() {
        return;
    }
    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );
    let instances = match vast.list_instances().await {
        Ok(instances) => instances,
        Err(error) => {
            // A failed listing must not be mistaken for "every instance was
            // destroyed", so skip this tick entirely.
            warn!("spend tracker could not list instances: {error}");
            return;
        }
    };
    let observations = observations_from_vast(&instances);
    record_and_enforce(app, context, observations).await;
}

/// Fold observations into the ledger, then raise alerts and auto-stop.
pub async fn record_and_enforce(
    app: &AppHandle,
    context: &AppContext,
    observations: Vec<SpendObservation>,
) {
    let now = Utc::now();
    let month = month_key(now);
    let mut alerts = Vec::new();
    let mut stop_targets = Vec::new();

    let updated = context
        .update_state(|state| {
            let spend = &mut state.spend;
            spend.record_observations(&observations, now);
            let spent = spend.month_total(&month);
            let budget = spend.settings.monthly_budget_usd;
            match spend.budget_level(now) {
                BudgetLevel::Exceeded
                    if spend.alerts.exceeded_month.as_deref() != Some(month.as_str()) =>
                {
                    spend.alerts.exceeded_month = Some(month.clone());
                    spend.alerts.warned_month = Some(month.clone());
                    alerts.push(SpendAlert {
                        kind: "exceeded".into(),
                        month_to_date_usd: spent,
                        budget_usd: budget,
                        instance_id: None,
                        message: format!(
                            "You have spent ${spent:.2} of your ${budget:.2} monthly budget."
                        ),
                    });
                }
                BudgetLevel::Warning
                    if spend.alerts.warned_month.as_deref() != Some(month.as_str()) =>
                {
                    spend.alerts.warned_month = Some(month.clone());
                    alerts.push(SpendAlert {
                        kind: "warning".into(),
                        month_to_date_usd: spent,
                        budget_usd: budget,
                        instance_id: None,
                        message: format!(
                            "You have used {:.0}% of your ${budget:.2} monthly budget (${spent:.2}).",
                            spent / budget * 100.0
                        ),
                    });
                }
                _ => {}
            }
            stop_targets = spend.instances_to_auto_stop(now);
            // Mark before stopping so a slow or failing stop is not retried
            // every minute; the user is told if it fails.
            for target in &stop_targets {
                spend.mark_auto_stopped(target.instance_id);
            }
        })
        .await;

    let state = match updated {
        Ok(state) => state,
        Err(error) => {
            warn!("spend tracker could not persist ledger: {error}");
            return;
        }
    };

    if let Err(error) = app.emit(SPEND_UPDATED_EVENT, state.spend.summary(now)) {
        warn!("failed to emit spend update: {error}");
    }
    for alert in alerts {
        emit_alert(app, alert);
    }

    let spent = state.spend.month_total(&month);
    let budget = state.spend.settings.monthly_budget_usd;
    for target in stop_targets {
        let app = app.clone();
        let context = context.clone();
        tauri::async_runtime::spawn(async move {
            info!(
                instance_id = target.instance_id,
                "monthly budget reached; stopping instance"
            );
            let alert = match InstanceLifecycleService::pause_instance(&context, target.instance_id)
                .await
            {
                Ok(()) => SpendAlert {
                    kind: "auto_stopped".into(),
                    month_to_date_usd: spent,
                    budget_usd: budget,
                    instance_id: Some(target.instance_id),
                    message: format!(
                        "Instance {} was stopped because your ${budget:.2} monthly budget was reached. Start it again to override.",
                        target.instance_id
                    ),
                },
                Err(error) => {
                    warn!(
                        instance_id = target.instance_id,
                        "budget auto-stop failed: {error}"
                    );
                    SpendAlert {
                        kind: "auto_stop_failed".into(),
                        month_to_date_usd: spent,
                        budget_usd: budget,
                        instance_id: Some(target.instance_id),
                        message: format!(
                            "Your monthly budget was reached but instance {} could not be stopped: {error}",
                            target.instance_id
                        ),
                    }
                }
            };
            emit_alert(&app, alert);
        });
    }
}

fn emit_alert(app: &AppHandle, alert: SpendAlert) {
    if let Err(error) = app.emit(SPEND_ALERT_EVENT, alert) {
        warn!("failed to emit spend alert: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vast_statuses_map_to_billing_state() {
        for status in ["running", "loading", "Running", "creating"] {
            assert!(vast_status_is_billed_as_running(status), "{status}");
        }
        for status in ["exited", "stopped", "inactive", "offline", ""] {
            assert!(!vast_status_is_billed_as_running(status), "{status}");
        }
    }

    #[test]
    fn destroyed_instances_are_not_observed() {
        let instance = |id: u64, status: &str| {
            VastInstance::from_value(&serde_json::json!({
                "id": id,
                "actual_status": status,
                "dph_total": 0.5,
                "storage_total_cost": 0.01,
                "gpu_name": "RTX 4090",
            }))
            .unwrap()
        };
        let observations =
            observations_from_vast(&[instance(1, "running"), instance(2, "destroying")]);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].label, "Instance 1");
        assert!(observations[0].running);
        assert!((observations[0].hourly_price - 0.5).abs() < 1e-9);
    }
}
