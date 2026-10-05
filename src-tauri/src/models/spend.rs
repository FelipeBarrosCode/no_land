//! Local spend ledger and monthly budget.
//!
//! Cost is estimated from what the app observes: every time the background
//! poller sees an instance it accrues the elapsed time since the previous
//! observation at the rate that applied during that interval (full hourly
//! price while running, storage-only price while stopped). The provider's
//! own invoice is authoritative; this ledger exists so the user can see and
//! cap spending without leaving the app.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Ledger entries older than this many months are pruned.
const LEDGER_RETENTION_MONTHS: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSettings {
    /// Monthly spending cap in USD. `0` disables the budget.
    #[serde(default)]
    pub monthly_budget_usd: f64,
    /// Percent of the budget at which a warning is shown (1-100).
    #[serde(default = "default_warn_percent")]
    pub warn_at_percent: u8,
    /// Stop running instances (after the usual pre-stop backup) once the
    /// budget is reached.
    #[serde(default)]
    pub auto_stop_at_budget: bool,
}

fn default_warn_percent() -> u8 {
    80
}

impl Default for BudgetSettings {
    fn default() -> Self {
        Self {
            monthly_budget_usd: 0.0,
            warn_at_percent: default_warn_percent(),
            auto_stop_at_budget: false,
        }
    }
}

impl BudgetSettings {
    pub fn validated(mut self) -> Self {
        if !self.monthly_budget_usd.is_finite() || self.monthly_budget_usd < 0.0 {
            self.monthly_budget_usd = 0.0;
        }
        self.warn_at_percent = self.warn_at_percent.clamp(1, 100);
        self
    }

    pub fn enabled(&self) -> bool {
        self.monthly_budget_usd > 0.0
    }
}

/// Accrued cost for one instance within one calendar month (UTC).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendLedgerEntry {
    pub month: String,
    pub provider: String,
    pub instance_id: u64,
    pub label: String,
    pub gpu_name: String,
    pub running_seconds: f64,
    pub stopped_seconds: f64,
    pub accrued_usd: f64,
}

/// Where accrual left off for an instance that is still in the account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendCursor {
    pub provider: String,
    pub instance_id: u64,
    pub last_observed_at: DateTime<Utc>,
    pub running: bool,
    pub hourly_price: f64,
    pub storage_hourly_price: f64,
    /// Start of the current uninterrupted running stretch.
    pub session_started_at: Option<DateTime<Utc>>,
    pub session_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetAlertState {
    /// Month (`YYYY-MM`) in which the warning threshold notification fired.
    pub warned_month: Option<String>,
    /// Month in which the budget-reached notification fired.
    pub exceeded_month: Option<String>,
    /// Month the auto-stop list below belongs to.
    pub auto_stop_month: Option<String>,
    /// Instances already stopped by the budget this month. If the user
    /// starts one again it is treated as a deliberate override.
    pub auto_stopped_instance_ids: Vec<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendState {
    #[serde(default)]
    pub settings: BudgetSettings,
    #[serde(default)]
    pub ledger: Vec<SpendLedgerEntry>,
    #[serde(default)]
    pub cursors: Vec<SpendCursor>,
    #[serde(default)]
    pub alerts: BudgetAlertState,
}

/// One instance as seen by a provider listing.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendObservation {
    pub provider: String,
    pub instance_id: u64,
    pub label: String,
    pub gpu_name: String,
    pub running: bool,
    pub hourly_price: f64,
    pub storage_hourly_price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetLevel {
    Disabled,
    Ok,
    Warning,
    Exceeded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSpend {
    pub provider: String,
    pub instance_id: u64,
    pub label: String,
    pub gpu_name: String,
    pub running: bool,
    pub current_hourly_usd: f64,
    pub session_usd: f64,
    pub session_started_at: Option<DateTime<Utc>>,
    pub month_to_date_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MonthlySpend {
    pub month: String,
    pub total_usd: f64,
    pub running_hours: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendSummary {
    pub month: String,
    pub month_to_date_usd: f64,
    pub current_burn_usd_per_hour: f64,
    /// Month-to-date plus the current burn rate for the rest of the month.
    pub projected_month_usd: f64,
    pub budget: BudgetSettings,
    /// `disabled`, `ok`, `warning` or `exceeded`.
    pub budget_status: String,
    pub budget_used_percent: Option<f64>,
    pub instances: Vec<InstanceSpend>,
    pub months: Vec<MonthlySpend>,
}

pub fn month_key(at: DateTime<Utc>) -> String {
    format!("{:04}-{:02}", at.year(), at.month())
}

fn start_of_next_month(at: DateTime<Utc>) -> DateTime<Utc> {
    let (year, month) = if at.month() == 12 {
        (at.year() + 1, 1)
    } else {
        (at.year(), at.month() + 1)
    };
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .expect("first of month is always a valid UTC time")
}

fn finite_non_negative(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

impl SpendState {
    /// Accrue cost for every observed instance up to `now`.
    ///
    /// The interval since the previous observation is billed at the rate
    /// recorded at that previous observation, because that is the state the
    /// instance was known to be in. Instances missing from `observations`
    /// are treated as destroyed and their cursor is dropped.
    pub fn record_observations(&mut self, observations: &[SpendObservation], now: DateTime<Utc>) {
        for observation in observations {
            let label = observation.label.clone();
            let gpu_name = observation.gpu_name.clone();
            let index = self.cursors.iter().position(|cursor| {
                cursor.instance_id == observation.instance_id
                    && cursor.provider == observation.provider
            });

            match index {
                Some(index) => {
                    let previous = self.cursors[index].clone();
                    if now > previous.last_observed_at {
                        let rate = if previous.running {
                            previous.hourly_price
                        } else {
                            previous.storage_hourly_price
                        };
                        let accrued = self.accrue_interval(
                            &previous.provider,
                            previous.instance_id,
                            &label,
                            &gpu_name,
                            previous.running,
                            rate,
                            previous.last_observed_at,
                            now,
                        );
                        let cursor = &mut self.cursors[index];
                        if previous.running {
                            cursor.session_usd += accrued;
                        }
                        cursor.last_observed_at = now;
                    }

                    let cursor = &mut self.cursors[index];
                    if observation.running && !cursor.running {
                        cursor.session_started_at = Some(now);
                        cursor.session_usd = 0.0;
                    } else if !observation.running {
                        cursor.session_started_at = None;
                        cursor.session_usd = 0.0;
                    }
                    cursor.running = observation.running;
                    cursor.hourly_price = finite_non_negative(observation.hourly_price);
                    cursor.storage_hourly_price =
                        finite_non_negative(observation.storage_hourly_price);
                }
                None => {
                    self.cursors.push(SpendCursor {
                        provider: observation.provider.clone(),
                        instance_id: observation.instance_id,
                        last_observed_at: now,
                        running: observation.running,
                        hourly_price: finite_non_negative(observation.hourly_price),
                        storage_hourly_price: finite_non_negative(
                            observation.storage_hourly_price,
                        ),
                        session_started_at: observation.running.then_some(now),
                        session_usd: 0.0,
                    });
                    // Make the instance visible in this month's ledger even
                    // before it has accrued anything.
                    self.entry_mut(
                        &observation.provider,
                        observation.instance_id,
                        &month_key(now),
                        &label,
                        &gpu_name,
                    );
                }
            }
        }

        self.cursors.retain(|cursor| {
            observations.iter().any(|observation| {
                observation.instance_id == cursor.instance_id
                    && observation.provider == cursor.provider
            })
        });
        self.prune_ledger(now);
    }

    #[allow(clippy::too_many_arguments)]
    fn accrue_interval(
        &mut self,
        provider: &str,
        instance_id: u64,
        label: &str,
        gpu_name: &str,
        running: bool,
        hourly_rate: f64,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> f64 {
        let mut total = 0.0;
        let mut segment_start = from;
        // Split at month boundaries so each month gets its own share.
        while segment_start < to {
            let segment_end = start_of_next_month(segment_start).min(to);
            let seconds = (segment_end - segment_start).num_milliseconds() as f64 / 1000.0;
            let cost = hourly_rate * seconds / 3600.0;
            let entry = self.entry_mut(
                provider,
                instance_id,
                &month_key(segment_start),
                label,
                gpu_name,
            );
            if running {
                entry.running_seconds += seconds;
            } else {
                entry.stopped_seconds += seconds;
            }
            entry.accrued_usd += cost;
            total += cost;
            segment_start = segment_end;
        }
        total
    }

    fn entry_mut(
        &mut self,
        provider: &str,
        instance_id: u64,
        month: &str,
        label: &str,
        gpu_name: &str,
    ) -> &mut SpendLedgerEntry {
        let index = match self.ledger.iter().position(|entry| {
            entry.instance_id == instance_id && entry.provider == provider && entry.month == month
        }) {
            Some(index) => index,
            None => {
                self.ledger.push(SpendLedgerEntry {
                    month: month.to_string(),
                    provider: provider.to_string(),
                    instance_id,
                    label: label.to_string(),
                    gpu_name: gpu_name.to_string(),
                    running_seconds: 0.0,
                    stopped_seconds: 0.0,
                    accrued_usd: 0.0,
                });
                self.ledger.len() - 1
            }
        };
        let entry = &mut self.ledger[index];
        if !label.is_empty() {
            entry.label = label.to_string();
        }
        if !gpu_name.is_empty() {
            entry.gpu_name = gpu_name.to_string();
        }
        entry
    }

    fn prune_ledger(&mut self, now: DateTime<Utc>) {
        let mut months = self
            .ledger
            .iter()
            .map(|entry| entry.month.clone())
            .collect::<Vec<_>>();
        months.sort();
        months.dedup();
        let current = month_key(now);
        if !months.contains(&current) {
            months.push(current);
        }
        if months.len() <= LEDGER_RETENTION_MONTHS {
            return;
        }
        let cutoff = months[months.len() - LEDGER_RETENTION_MONTHS].clone();
        self.ledger.retain(|entry| entry.month >= cutoff);
    }

    pub fn month_total(&self, month: &str) -> f64 {
        self.ledger
            .iter()
            .filter(|entry| entry.month == month)
            .map(|entry| entry.accrued_usd)
            .sum()
    }

    pub fn current_burn_per_hour(&self) -> f64 {
        self.cursors
            .iter()
            .map(|cursor| {
                if cursor.running {
                    cursor.hourly_price
                } else {
                    cursor.storage_hourly_price
                }
            })
            .sum()
    }

    pub fn budget_level(&self, now: DateTime<Utc>) -> BudgetLevel {
        if !self.settings.enabled() {
            return BudgetLevel::Disabled;
        }
        let spent = self.month_total(&month_key(now));
        let budget = self.settings.monthly_budget_usd;
        if spent >= budget {
            BudgetLevel::Exceeded
        } else if spent >= budget * f64::from(self.settings.warn_at_percent) / 100.0 {
            BudgetLevel::Warning
        } else {
            BudgetLevel::Ok
        }
    }

    pub fn summary(&self, now: DateTime<Utc>) -> SpendSummary {
        let month = month_key(now);
        let month_to_date_usd = self.month_total(&month);
        let burn = self.current_burn_per_hour();
        let hours_left = (start_of_next_month(now) - now).num_seconds().max(0) as f64 / 3600.0;
        let level = self.budget_level(now);

        let instances = self
            .cursors
            .iter()
            .map(|cursor| {
                let entry = self.ledger.iter().find(|entry| {
                    entry.instance_id == cursor.instance_id
                        && entry.provider == cursor.provider
                        && entry.month == month
                });
                InstanceSpend {
                    provider: cursor.provider.clone(),
                    instance_id: cursor.instance_id,
                    label: entry.map(|entry| entry.label.clone()).unwrap_or_default(),
                    gpu_name: entry.map(|entry| entry.gpu_name.clone()).unwrap_or_default(),
                    running: cursor.running,
                    current_hourly_usd: if cursor.running {
                        cursor.hourly_price
                    } else {
                        cursor.storage_hourly_price
                    },
                    session_usd: cursor.session_usd,
                    session_started_at: cursor.session_started_at,
                    month_to_date_usd: entry.map(|entry| entry.accrued_usd).unwrap_or_default(),
                }
            })
            .collect();

        let mut months = self
            .ledger
            .iter()
            .map(|entry| entry.month.clone())
            .collect::<Vec<_>>();
        months.sort();
        months.dedup();
        let months = months
            .into_iter()
            .rev()
            .map(|month| MonthlySpend {
                total_usd: self.month_total(&month),
                running_hours: self
                    .ledger
                    .iter()
                    .filter(|entry| entry.month == month)
                    .map(|entry| entry.running_seconds / 3600.0)
                    .sum(),
                month,
            })
            .collect();

        SpendSummary {
            month,
            month_to_date_usd,
            current_burn_usd_per_hour: burn,
            projected_month_usd: month_to_date_usd + burn * hours_left,
            budget: self.settings.clone(),
            budget_status: match level {
                BudgetLevel::Disabled => "disabled",
                BudgetLevel::Ok => "ok",
                BudgetLevel::Warning => "warning",
                BudgetLevel::Exceeded => "exceeded",
            }
            .to_string(),
            budget_used_percent: self
                .settings
                .enabled()
                .then(|| month_to_date_usd / self.settings.monthly_budget_usd * 100.0),
            instances,
            months,
        }
    }

    /// Running instances the budget should stop now. Instances already
    /// stopped by the budget this month and started again are skipped.
    pub fn instances_to_auto_stop(&mut self, now: DateTime<Utc>) -> Vec<SpendCursor> {
        let month = month_key(now);
        if self.alerts.auto_stop_month.as_deref() != Some(month.as_str()) {
            self.alerts.auto_stop_month = Some(month);
            self.alerts.auto_stopped_instance_ids.clear();
        }
        if !self.settings.auto_stop_at_budget || self.budget_level(now) != BudgetLevel::Exceeded {
            return Vec::new();
        }
        self.cursors
            .iter()
            .filter(|cursor| {
                cursor.running
                    && !self
                        .alerts
                        .auto_stopped_instance_ids
                        .contains(&cursor.instance_id)
            })
            .cloned()
            .collect()
    }

    pub fn mark_auto_stopped(&mut self, instance_id: u64) {
        if !self.alerts.auto_stopped_instance_ids.contains(&instance_id) {
            self.alerts.auto_stopped_instance_ids.push(instance_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn observation(id: u64, running: bool, hourly: f64, storage: f64) -> SpendObservation {
        SpendObservation {
            provider: "vast".to_string(),
            instance_id: id,
            label: format!("Instance {id}"),
            gpu_name: "RTX 4090".to_string(),
            running,
            hourly_price: hourly,
            storage_hourly_price: storage,
        }
    }

    #[test]
    fn running_time_accrues_at_full_hourly_price() {
        let mut state = SpendState::default();
        state.record_observations(&[observation(1, true, 0.60, 0.01)], at("2026-10-05T10:00:00Z"));
        state.record_observations(&[observation(1, true, 0.60, 0.01)], at("2026-10-05T12:30:00Z"));

        assert!((state.month_total("2026-10") - 1.5).abs() < 1e-9);
        let summary = state.summary(at("2026-10-05T12:30:00Z"));
        assert!((summary.instances[0].session_usd - 1.5).abs() < 1e-9);
        assert!((summary.current_burn_usd_per_hour - 0.60).abs() < 1e-9);
    }

    #[test]
    fn interval_is_billed_at_the_previous_state_rate() {
        let mut state = SpendState::default();
        state.record_observations(&[observation(1, false, 0.60, 0.02)], at("2026-10-05T10:00:00Z"));
        // Started at some point in the last hour; we only know it was
        // stopped at the previous observation.
        state.record_observations(&[observation(1, true, 0.60, 0.02)], at("2026-10-05T11:00:00Z"));
        assert!((state.month_total("2026-10") - 0.02).abs() < 1e-9);

        state.record_observations(&[observation(1, true, 0.60, 0.02)], at("2026-10-05T12:00:00Z"));
        assert!((state.month_total("2026-10") - 0.62).abs() < 1e-9);
        let entry = &state.ledger[0];
        assert!((entry.running_seconds - 3600.0).abs() < 1e-6);
        assert!((entry.stopped_seconds - 3600.0).abs() < 1e-6);
    }

    #[test]
    fn stopping_resets_the_session() {
        let mut state = SpendState::default();
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T10:00:00Z"));
        state.record_observations(&[observation(1, false, 1.0, 0.0)], at("2026-10-05T11:00:00Z"));
        let cursor = &state.cursors[0];
        assert_eq!(cursor.session_started_at, None);
        assert_eq!(cursor.session_usd, 0.0);
        assert!((state.month_total("2026-10") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn accrual_is_split_across_month_boundaries() {
        let mut state = SpendState::default();
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-09-30T23:00:00Z"));
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-01T02:00:00Z"));

        assert!((state.month_total("2026-09") - 1.0).abs() < 1e-9);
        assert!((state.month_total("2026-10") - 2.0).abs() < 1e-9);
    }

    #[test]
    fn destroyed_instances_drop_their_cursor_but_keep_history() {
        let mut state = SpendState::default();
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T10:00:00Z"));
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T11:00:00Z"));
        state.record_observations(&[], at("2026-10-05T12:00:00Z"));

        assert!(state.cursors.is_empty());
        assert!((state.month_total("2026-10") - 1.0).abs() < 1e-9);
        assert_eq!(state.current_burn_per_hour(), 0.0);
    }

    #[test]
    fn budget_levels_follow_warn_threshold() {
        let mut state = SpendState::default();
        assert_eq!(state.budget_level(at("2026-10-05T10:00:00Z")), BudgetLevel::Disabled);

        state.settings = BudgetSettings {
            monthly_budget_usd: 10.0,
            warn_at_percent: 80,
            auto_stop_at_budget: true,
        };
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T00:00:00Z"));
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T07:00:00Z"));
        assert_eq!(state.budget_level(at("2026-10-05T07:00:00Z")), BudgetLevel::Ok);
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T08:30:00Z"));
        assert_eq!(state.budget_level(at("2026-10-05T08:30:00Z")), BudgetLevel::Warning);
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-05T10:00:00Z"));
        assert_eq!(state.budget_level(at("2026-10-05T10:00:00Z")), BudgetLevel::Exceeded);
    }

    #[test]
    fn auto_stop_respects_manual_restart_override() {
        let mut state = SpendState {
            settings: BudgetSettings {
                monthly_budget_usd: 1.0,
                warn_at_percent: 80,
                auto_stop_at_budget: true,
            },
            ..SpendState::default()
        };
        state.record_observations(&[observation(7, true, 2.0, 0.0)], at("2026-10-05T10:00:00Z"));
        state.record_observations(&[observation(7, true, 2.0, 0.0)], at("2026-10-05T11:00:00Z"));

        let now = at("2026-10-05T11:00:00Z");
        let targets = state.instances_to_auto_stop(now);
        assert_eq!(targets.len(), 1);
        state.mark_auto_stopped(7);

        // User restarts it: it is running again but must not be stopped twice.
        assert!(state.instances_to_auto_stop(now).is_empty());

        // A new month resets the override list (and the spend).
        let next_month = at("2026-11-01T00:10:00Z");
        state.record_observations(&[observation(7, true, 2.0, 0.0)], next_month);
        assert_eq!(state.budget_level(next_month), BudgetLevel::Ok);
        assert!(state.instances_to_auto_stop(next_month).is_empty());
        assert!(state.alerts.auto_stopped_instance_ids.is_empty());
    }

    #[test]
    fn auto_stop_disabled_returns_nothing() {
        let mut state = SpendState {
            settings: BudgetSettings {
                monthly_budget_usd: 1.0,
                warn_at_percent: 80,
                auto_stop_at_budget: false,
            },
            ..SpendState::default()
        };
        state.record_observations(&[observation(7, true, 2.0, 0.0)], at("2026-10-05T10:00:00Z"));
        state.record_observations(&[observation(7, true, 2.0, 0.0)], at("2026-10-05T11:00:00Z"));
        assert!(state.instances_to_auto_stop(at("2026-10-05T11:00:00Z")).is_empty());
    }

    #[test]
    fn summary_projects_rest_of_month_at_current_burn() {
        let mut state = SpendState::default();
        state.record_observations(&[observation(1, true, 1.0, 0.0)], at("2026-10-31T22:00:00Z"));
        let summary = state.summary(at("2026-10-31T22:00:00Z"));
        assert!((summary.projected_month_usd - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ledger_keeps_twelve_months() {
        let mut state = SpendState::default();
        for month in 1..=12 {
            state.ledger.push(SpendLedgerEntry {
                month: format!("2025-{month:02}"),
                provider: "vast".into(),
                instance_id: 1,
                label: String::new(),
                gpu_name: String::new(),
                running_seconds: 0.0,
                stopped_seconds: 0.0,
                accrued_usd: 1.0,
            });
        }
        state.record_observations(&[], at("2026-01-15T00:00:00Z"));
        assert!(!state.ledger.iter().any(|entry| entry.month == "2025-01"));
        assert!(state.ledger.iter().any(|entry| entry.month == "2025-02"));
    }

    #[test]
    fn settings_validation_clamps_values() {
        let settings = BudgetSettings {
            monthly_budget_usd: -4.0,
            warn_at_percent: 0,
            auto_stop_at_budget: true,
        }
        .validated();
        assert_eq!(settings.monthly_budget_usd, 0.0);
        assert_eq!(settings.warn_at_percent, 1);
    }
}
