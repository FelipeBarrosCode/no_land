use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use super::status::FailSafeTransition;

const STATS_EVENT: &str = "network-monitor://stats";
const STATUS_EVENT: &str = "network-monitor://status";
const ERROR_EVENT: &str = "network-monitor://error";

#[derive(Clone)]
pub(crate) struct Reporter {
    app: AppHandle,
    session_id: Uuid,
    latest_stats: Arc<RwLock<Option<Value>>>,
}

impl Reporter {
    pub(crate) fn new(
        app: AppHandle,
        session_id: Uuid,
        latest_stats: Arc<RwLock<Option<Value>>>,
    ) -> Self {
        Self {
            app,
            session_id,
            latest_stats,
        }
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
                *self.latest_stats.write().await = Some(value.clone());
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

        let title = if value
            .get("reasons")
            .and_then(Value::as_array)
            .is_some_and(|reasons| {
                reasons
                    .iter()
                    .any(|reason| reason.as_str() == Some("CONNECTION_LOST"))
            }) {
            "No Land — Connection lost"
        } else {
            "No Land — Connection unstable"
        };
        let body = if title.ends_with("Connection lost") {
            "The connection to your gaming PC appears to be lost."
        } else {
            "High latency variation or packet loss may affect streaming."
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
