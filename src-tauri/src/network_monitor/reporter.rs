use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::services::{app_context::AppContext, feedback};
use noland_network_contracts::feedback::{MeasurementEvidence, RouteMode};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use super::status::FailSafeTransition;

fn take_batch(
    batches: &mut VecDeque<ReportedBatch>,
    evidence: &MeasurementEvidence,
) -> Option<ReportedBatch> {
    let index = batches
        .iter()
        .position(|batch| batch.id == evidence.batch_id)?;
    batches.remove(index)
}

const STATS_EVENT: &str = "network-monitor://stats";
const STATUS_EVENT: &str = "network-monitor://status";
const ERROR_EVENT: &str = "network-monitor://error";

#[derive(Clone)]
pub(crate) struct Reporter {
    app: AppHandle,
    session_id: Uuid,
    latest_stats: Arc<RwLock<Option<(Instant, Value)>>>,
    last_report: Arc<RwLock<Option<Instant>>>,
    instance_id: Option<u64>,
    feedback_batches: Arc<RwLock<VecDeque<ReportedBatch>>>,
    feedback_segment: Arc<RwLock<Uuid>>,
}

#[derive(Clone)]
struct ReportedBatch {
    id: Uuid,
    received_sequence: u64,
    measured_at: Instant,
    route: RouteMode,
}

impl Reporter {
    pub(crate) fn new(
        app: AppHandle,
        session_id: Uuid,
        latest_stats: Arc<RwLock<Option<(Instant, Value)>>>,
        instance_id: Option<u64>,
    ) -> Self {
        Self {
            app,
            session_id,
            latest_stats,
            last_report: Arc::new(RwLock::new(None)),
            instance_id,
            feedback_batches: Arc::new(RwLock::new(VecDeque::new())),
            feedback_segment: Arc::new(RwLock::new(Uuid::new_v4())),
        }
    }

    pub(crate) async fn new_connection(&self) {
        self.feedback_batches.write().await.clear();
        *self.feedback_segment.write().await = Uuid::new_v4();
    }

    pub(crate) async fn feedback_batch_id(&self) -> Option<Uuid> {
        let instance = self.instance_id?;
        let context = self.app.state::<AppContext>();
        context
            .feedback
            .can_measure(context.inner(), instance)
            .await
            .then(Uuid::new_v4)
    }

    pub(crate) async fn measurements_reported(
        &self,
        batch: Option<Uuid>,
        received: Option<(u64, Instant)>,
    ) {
        *self.last_report.write().await = Some(Instant::now());
        let (Some(id), Some((received_sequence, measured_at)), Some(instance)) =
            (batch, received, self.instance_id)
        else {
            return;
        };
        let context = self.app.state::<AppContext>();
        if !context
            .feedback
            .can_measure(context.inner(), instance)
            .await
        {
            return;
        }
        let route = feedback::route_for(&context.load_state().await, instance);
        let mut batches = self.feedback_batches.write().await;
        batches.retain(|batch| batch.measured_at.elapsed() <= Duration::from_secs(3));
        if batches.len() >= 128 {
            batches.pop_front();
        }
        batches.push_back(ReportedBatch {
            id,
            received_sequence,
            measured_at,
            route,
        });
    }

    async fn collect_feedback(&self, value: &Value) {
        let Some(instance) = self.instance_id else {
            return;
        };
        let Ok(evidence) =
            serde_json::from_value::<MeasurementEvidence>(value["measurementEvidence"].clone())
        else {
            return;
        };
        let batch = take_batch(&mut self.feedback_batches.write().await, &evidence);
        let Some(batch) = batch.filter(|batch| {
            Some(batch.received_sequence) == evidence.received_sequence
                && batch.measured_at.elapsed() <= Duration::from_secs(3)
        }) else {
            return;
        };
        let Some(jitter) = value["metrics"]["last60Seconds"]["jitterMs"].as_f64() else {
            return;
        };
        if value["metrics"]["last60Seconds"]["received"].as_u64()
            != Some(evidence.received_rtt_count)
        {
            return;
        }
        let context = self.app.state::<AppContext>();
        let current_route = feedback::route_for(&context.load_state().await, instance);
        let route = if current_route == batch.route {
            batch.route
        } else {
            RouteMode::Mixed
        };
        let segment = *self.feedback_segment.read().await;
        context
            .feedback
            .observe_jitter(
                context.inner(),
                instance,
                segment,
                &evidence,
                jitter,
                batch.measured_at,
                route,
            )
            .await;
    }

    pub(crate) async fn forward_agent_text(&self, text: &str) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                self.error(format!("agent sent invalid JSON: {error}"));
                return;
            }
        };

        match value.get("type").and_then(Value::as_str) {
            Some("stats_update") => {
                if value["sessionId"].as_str() != Some(self.session_id.to_string().as_str()) {
                    return;
                }
                // Agent heartbeats can repeat old statistics. Freshness follows
                // actual client measurement reports, including measured timeouts.
                if let Some(measured_at) = *self.last_report.read().await {
                    *self.latest_stats.write().await = Some((measured_at, value.clone()));
                }
                // Never derive feedback from the client fail-safe or last_report alone.
                // Only an agent echo of an actual sent batch can reach the accumulator.
                self.collect_feedback(&value).await;
                self.emit(STATS_EVENT, value);
            }
            Some("status_changed") => {
                self.notify_if_alert_eligible(&value);
                self.emit(STATUS_EVENT, value);
            }
            Some("error") => self.emit(ERROR_EVENT, value),
            Some(message_type) => {
                self.error(format!(
                    "agent sent unsupported message type {message_type:?}"
                ));
            }
            None => self.error("agent message is missing a string type"),
        }
    }

    pub(crate) fn fail_safe(&self, transition: FailSafeTransition) {
        let value = match transition {
            FailSafeTransition::ConnectionLost => json!({
                "type": "status_changed",
                "sessionId": self.session_id,
                "previous": "GOOD",
                "current": "BAD",
                "reasons": ["CONNECTION_LOST"],
                "keyMetrics": {
                    "medianRttMs": null,
                    "p95RttMs": null,
                    "jitterMs": 0.0,
                    "lossPercent": 100.0,
                    "spikePercent": 0.0,
                    "longestLossBurst": 0
                },
                "alertEligible": true,
                "source": "client_fail_safe"
            }),
            FailSafeTransition::Recovered => json!({
                "type": "status_changed",
                "sessionId": self.session_id,
                "previous": "BAD",
                "current": "GOOD",
                "reasons": [],
                "keyMetrics": {
                    "medianRttMs": null,
                    "p95RttMs": null,
                    "jitterMs": 0.0,
                    "lossPercent": 0.0,
                    "spikePercent": 0.0,
                    "longestLossBurst": 0
                },
                "alertEligible": false,
                "source": "client_fail_safe"
            }),
        };
        self.notify_if_alert_eligible(&value);
        self.emit(STATUS_EVENT, value);
    }

    fn notify_if_alert_eligible(&self, value: &Value) {
        let is_bad = value.get("current").and_then(Value::as_str) == Some("BAD");
        let alert_eligible = value
            .get("alertEligible")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !is_bad || !alert_eligible {
            return;
        }

        let connection_lost =
            value
                .get("reasons")
                .and_then(Value::as_array)
                .is_some_and(|reasons| {
                    reasons
                        .iter()
                        .any(|reason| reason.as_str() == Some("CONNECTION_LOST"))
                });
        let (title, body) = if connection_lost {
            (
                "No Land — Connection lost",
                "The connection to your gaming PC appears to be lost.",
            )
        } else {
            (
                "No Land — Connection unstable",
                "High latency variation or packet loss may affect streaming.",
            )
        };

        if let Err(error) = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .sound("Glass")
            .show()
        {
            warn!(%error, "failed to show native network warning");
        }
    }

    pub(crate) fn error(&self, message: impl Into<String>) {
        self.emit(
            ERROR_EVENT,
            json!({
                "type": "error",
                "sessionId": self.session_id,
                "message": message.into(),
                "source": "network_monitor"
            }),
        );
    }

    fn emit(&self, event: &str, payload: Value) {
        if let Err(error) = self.app.emit(event, payload) {
            warn!(event, %error, "failed to emit network monitor event");
        }
    }
}

#[cfg(test)]
mod feedback_watermark_tests {
    use super::*;
    #[test]
    fn an_old_heartbeat_cannot_borrow_a_new_report_send_time() {
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        let mut batches = VecDeque::from([ReportedBatch {
            id: new,
            received_sequence: 12,
            measured_at: Instant::now(),
            route: RouteMode::Direct,
        }]);
        let mut evidence = MeasurementEvidence {
            batch_id: old,
            received_sequence: Some(12),
            received_rtt_count: 20,
            continuous_window_ms: 60_000,
            latest_received_age_ms: Some(0),
            metric_version: noland_network_contracts::feedback::METRIC_VERSION.into(),
        };
        assert!(take_batch(&mut batches, &evidence).is_none());
        assert_eq!(batches.len(), 1);
        evidence.batch_id = new;
        assert!(take_batch(&mut batches, &evidence).is_some());
        assert!(take_batch(&mut batches, &evidence).is_none());
    }
}
