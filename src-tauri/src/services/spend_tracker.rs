//! Background spend accrual and budget enforcement.
//!
//! Every minute the tracker lists the account's instances, folds them into
//! the persisted [`SpendState`](crate::models::spend::SpendState) ledger and
//! emits `spend:updated`. When the monthly budget crosses its warning
//! threshold or is reached it emits `spend:alert` once per month (the
//! frontend decides whether to show an OS notification), and when auto-stop
//! is enabled it stops running instances through the normal lifecycle path,
//! which runs the pre-stop backup first.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

use crate::{
    models::{
        app_state::PersistedAppState,
        spend::{month_key, BudgetLevel, BudgetSettings, SpendObservation},
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

pub(crate) fn replace_vast_api_key(state: &mut PersistedAppState, api_key: String) {
    if state.credentials.vast_api_key.trim() != api_key.trim() {
        // Without an authoritative account identifier, credential rotation is
        // a new authorization scope. Require an explicit budget opt-in again.
        let mut history = std::mem::take(&mut state.spend.prior_credential_ledger);
        history.append(&mut state.spend.ledger);
        state.spend = Default::default();
        state.spend.prior_credential_ledger = history;
    }
    state.credentials.vast_api_key = api_key;
}

pub use crate::models::spend::SpendAlert;

#[derive(Default)]
pub(crate) struct BudgetStopJobs {
    in_flight: HashSet<u64>,
    retry_after: HashMap<u64, Instant>,
}

struct StopReservation {
    jobs: Arc<Mutex<BudgetStopJobs>>,
    instance_id: u64,
}

impl StopReservation {
    fn acquire(jobs: &Arc<Mutex<BudgetStopJobs>>, instance_id: u64) -> Option<Self> {
        let mut state = jobs.lock().ok()?;
        if state.in_flight.contains(&instance_id)
            || state
                .retry_after
                .get(&instance_id)
                .is_some_and(|until| *until > Instant::now())
        {
            return None;
        }
        state.retry_after.remove(&instance_id);
        state.in_flight.insert(instance_id);
        Some(Self {
            jobs: jobs.clone(),
            instance_id,
        })
    }

    fn failed(&self) {
        if let Ok(mut state) = self.jobs.lock() {
            state
                .retry_after
                .insert(self.instance_id, Instant::now() + Duration::from_secs(300));
        }
    }
}

impl Drop for StopReservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.jobs.lock() {
            state.in_flight.remove(&self.instance_id);
        }
    }
}

// Never derive Debug/Serialize: the original credential stays memory-only.
pub(crate) struct BudgetStopPolicy {
    api_key: String,
    settings: BudgetSettings,
    month: String,
}

impl BudgetStopPolicy {
    pub(crate) fn authorized(&self, state: &PersistedAppState, instance_id: u64) -> bool {
        let now = Utc::now();
        state.credentials.vast_api_key.trim() == self.api_key.trim()
            && state.spend.settings == self.settings
            && month_key(now) == self.month
            && state.spend.settings.auto_stop_at_budget
            && state.spend.budget_level(now) == BudgetLevel::Exceeded
            && state.spend.cursors.iter().any(|cursor| {
                cursor.provider == "vast" && cursor.instance_id == instance_id && cursor.running
            })
            && !state
                .spend
                .alerts
                .auto_stopped_instance_ids
                .contains(&instance_id)
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }
    pub(crate) fn month(&self) -> &str {
        &self.month
    }
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
    [
        "running", "loading", "creating", "starting", "active", "ready",
    ]
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
        api_key.clone(),
    );
    let instances = match vast.list_instances().await {
        Ok(instances) => instances,
        Err(_error) => {
            // A failed listing must not be mistaken for "every instance was
            // destroyed", so skip this tick entirely.
            warn!("spend tracker could not list instances");
            return;
        }
    };
    let observations = observations_from_vast(&instances);
    record_and_enforce(app, context, observations, &api_key).await;
}

/// Fold observations into the ledger, then raise alerts and auto-stop.
async fn record_and_enforce(
    app: &AppHandle,
    context: &AppContext,
    observations: Vec<SpendObservation>,
    api_key: &str,
) {
    let now = Utc::now();
    let month = month_key(now);
    let mut stop_targets = Vec::new();
    let mut accepted = false;

    let updated = context
        .update_state(|state| {
            // Ignore an old account's HTTP response after credentials changed.
            if state.credentials.vast_api_key.trim() != api_key.trim() { return; }
            accepted = true;
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
                    spend.queue_alert(SpendAlert {
                        id: String::new(),
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
                    spend.queue_alert(SpendAlert {
                        id: String::new(),
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
        })
        .await;

    let state = match updated {
        Ok(state) => state,
        Err(error) => {
            warn!("spend tracker could not persist ledger: {error}");
            return;
        }
    };

    if !accepted {
        return;
    }

    if let Err(error) = app.emit_to("main", SPEND_UPDATED_EVENT, state.spend.summary(now)) {
        warn!("failed to emit spend update: {error}");
    }
    for alert in &state.spend.pending_alerts {
        emit_alert(app, alert.clone());
    }

    let spent = state.spend.month_total(&month);
    let budget = state.spend.settings.monthly_budget_usd;
    for target in stop_targets {
        let Some(reservation) =
            StopReservation::acquire(&context.spend_stop_jobs, target.instance_id)
        else {
            continue;
        };
        let policy = BudgetStopPolicy {
            api_key: api_key.to_string(),
            settings: state.spend.settings.clone(),
            month: month.clone(),
        };
        let app = app.clone();
        let context = context.clone();
        tauri::async_runtime::spawn(async move {
            let _reservation = reservation;
            let Ok(_slot) = context.spend_stop_slots.clone().acquire_owned().await else {
                return;
            };
            info!(
                instance_id = target.instance_id,
                "monthly budget reached; stopping instance"
            );
            let alert = match InstanceLifecycleService::pause_instance_for_budget(&context, target.instance_id, &policy)
                .await
            {
                Ok(false) => return, // Policy/account changed while waiting or backing up.
                Ok(true) => SpendAlert {
                    id: String::new(),
                    kind: "auto_stopped".into(),
                    month_to_date_usd: spent,
                    budget_usd: budget,
                    instance_id: Some(target.instance_id),
                    message: format!(
                        "Instance {} was stopped because your ${budget:.2} monthly budget was reached. Start it again to override.",
                        target.instance_id
                    ),
                },
                Err(_error) => {
                    _reservation.failed();
                    warn!(
                        instance_id = target.instance_id,
                        "budget auto-stop failed; retrying after cooldown"
                    );
                    SpendAlert {
                        id: String::new(),
                        kind: "auto_stop_failed".into(),
                        month_to_date_usd: spent,
                        budget_usd: budget,
                        instance_id: Some(target.instance_id),
                        message: format!(
                            "Your monthly budget was reached but instance {} could not be stopped. Check the instance and backup status; No Land will retry.",
                            target.instance_id
                        ),
                    }
                }
            };
            // Never forward raw provider/SSH errors to lock-screen notifications.
            let result = context
                .update_state(|state| {
                    if state.credentials.vast_api_key.trim() != policy.api_key().trim() {
                        return;
                    }
                    if alert.kind == "auto_stopped" && month_key(Utc::now()) == policy.month {
                        state.spend.mark_auto_stopped(target.instance_id);
                    }
                    state.spend.queue_alert(alert);
                })
                .await;
            if let Ok(state) = result {
                for alert in state.spend.pending_alerts {
                    emit_alert(&app, alert);
                }
            } else {
                warn!(
                    instance_id = target.instance_id,
                    "could not persist budget stop result"
                );
            }
        });
    }
}

fn emit_alert(app: &AppHandle, alert: SpendAlert) {
    if let Err(error) = app.emit_to("main", SPEND_ALERT_EVENT, alert) {
        warn!("failed to emit spend alert: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reservations_are_released_on_cancellation_without_marking_success() {
        let jobs = Arc::new(Mutex::new(BudgetStopJobs::default()));
        let first = StopReservation::acquire(&jobs, 7).unwrap();
        assert!(StopReservation::acquire(&jobs, 7).is_none());
        drop(first);
        assert!(StopReservation::acquire(&jobs, 7).is_some());
    }

    #[test]
    fn transient_failures_retry_after_cooldown_not_next_month() {
        let jobs = Arc::new(Mutex::new(BudgetStopJobs::default()));
        let attempt = StopReservation::acquire(&jobs, 7).unwrap();
        attempt.failed();
        drop(attempt);
        assert!(StopReservation::acquire(&jobs, 7).is_none());
        jobs.lock()
            .unwrap()
            .retry_after
            .insert(7, Instant::now() - Duration::from_secs(1));
        assert!(StopReservation::acquire(&jobs, 7).is_some());
    }

    fn authorized_policy() -> (PersistedAppState, BudgetStopPolicy) {
        let now = Utc::now();
        let mut state = PersistedAppState::default();
        state.credentials.vast_api_key = "original-credential".into();
        state.spend.settings = BudgetSettings {
            monthly_budget_usd: 1.0,
            warn_at_percent: 80,
            auto_stop_at_budget: true,
        };
        let observation = SpendObservation {
            provider: "vast".into(),
            instance_id: 7,
            label: String::new(),
            gpu_name: String::new(),
            running: true,
            hourly_price: 100.0,
            storage_hourly_price: 0.01,
        };
        // Staying in the same month makes the test safe on UTC month boundaries.
        state.spend.record_observations(&[observation], now);
        state.spend.ledger[0].accrued_usd = 2.0;
        state.spend.instances_to_auto_stop(now);
        let policy = BudgetStopPolicy {
            api_key: state.credentials.vast_api_key.clone(),
            settings: state.spend.settings.clone(),
            month: month_key(now),
        };
        (state, policy)
    }

    #[test]
    fn stop_policy_rechecks_credentials_settings_month_and_target() {
        let (state, policy) = authorized_policy();
        assert!(policy.authorized(&state, 7));
        assert!(!policy.authorized(&state, 8));
        let mut changed = state.clone();
        changed.credentials.vast_api_key = "another-account".into();
        assert!(!policy.authorized(&changed, 7));
        changed = state.clone();
        changed.spend.settings.auto_stop_at_budget = false;
        assert!(!policy.authorized(&changed, 7));
        changed = state.clone();
        changed.spend.settings.monthly_budget_usd = 1000.0;
        assert!(!policy.authorized(&changed, 7));
        changed = state.clone();
        changed.spend.mark_auto_stopped(7);
        assert!(!policy.authorized(&changed, 7));
        let expired = BudgetStopPolicy {
            month: "2000-01".into(),
            ..policy
        };
        assert!(!expired.authorized(&state, 7));
    }

    #[test]
    fn changing_credentials_clears_spend_and_requires_budget_opt_in_again() {
        let (mut state, _) = authorized_policy();
        let before = state.spend.clone();
        replace_vast_api_key(&mut state, " original-credential ".into());
        assert_eq!(state.spend, before);
        replace_vast_api_key(&mut state, "another-account".into());
        assert!(state.spend.cursors.is_empty());
        assert!(state.spend.ledger.is_empty());
        assert!(state.spend.pending_alerts.is_empty());
        assert_eq!(state.spend.prior_credential_ledger, before.ledger);
        assert!(!state.spend.settings.auto_stop_at_budget);
    }

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
