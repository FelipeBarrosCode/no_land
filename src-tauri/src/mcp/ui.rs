use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, Mutex, RwLock};

#[derive(Default)]
pub struct UiBridge {
    snapshot: RwLock<Value>,
    replies: Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
}

impl UiBridge {
    pub async fn snapshot(&self, config: &noland_mcp::policy::Config) -> Value {
        let mut value = self.snapshot.read().await.clone();
        if value.is_null() {
            return json!({"available":false,"message":"Main app UI has not published a snapshot"});
        }
        if let Some(dialogs) = value["dialogs"].as_array_mut() {
            for dialog in dialogs {
                if dialog["instanceId"]
                    .as_u64()
                    .is_some_and(|id| !config.allows_instance(id))
                {
                    dialog["text"] =
                        json!("This dialog belongs to an instance not granted to this agent");
                    dialog["actions"] = json!([]);
                }
            }
        }
        if let Some(actions) = value["actions"].as_array_mut() {
            for action in actions {
                if !config.dialogs
                    || (action["capability"] == "control"
                        && (!config.control
                            || !action["instanceId"]
                                .as_u64()
                                .is_some_and(|id| config.allows_instance(id))))
                {
                    action["allowed"] = json!(false);
                }
            }
        }
        value
    }
    pub async fn publish(&self, value: Value) -> Result<(), String> {
        if value.to_string().len() > 65536
            || !value["revision"].is_string()
            || !value["route"].is_string()
            || !value["dialogs"].is_array()
            || !value["actions"].is_array()
        {
            return Err("Invalid UI snapshot".into());
        }
        *self.snapshot.write().await = value;
        Ok(())
    }
    pub async fn complete(&self, id: &str, result: Result<Value, String>) {
        if let Some(reply) = self.replies.lock().await.remove(id) {
            let _ = reply.send(result);
        }
    }
    pub async fn perform(
        self: &Arc<Self>,
        app: &AppHandle,
        kind: &str,
        args: Value,
        config: &noland_mcp::policy::Config,
    ) -> Result<Value, String> {
        if !config.enabled || !config.dialogs {
            return Err("App UI delegation is not granted".into());
        }
        let snapshot = self.snapshot.read().await;
        let mut expected_instance = None;
        if snapshot.is_null() {
            return Err(
                "The main app UI is unavailable; bring Noland Connect to the foreground".into(),
            );
        }
        if kind != "navigate" {
            if args["revision"] != snapshot["revision"] {
                return Err("UI snapshot changed; request a fresh snapshot before acting".into());
            }
            let id = args["action_id"].as_str().ok_or("action_id required")?;
            let actions = snapshot["actions"].as_array().ok_or("UI unavailable")?;
            let action = actions
                .iter()
                .find(|a| a["id"].as_str() == Some(id) && a["allowed"] == true)
                .ok_or(
                    "This UI action is not delegated. Security/MCP approvals require a human.",
                )?;
            if action["capability"] == "control" {
                expected_instance = action["instanceId"].as_u64();
                if !config.control
                    || !action["instanceId"]
                        .as_u64()
                        .is_some_and(|id| config.allows_instance(id))
                {
                    return Err(
                        "This UI action requires machine control and a grant for its instance"
                            .into(),
                    );
                }
            } else if action["capability"] != "dialogs" {
                return Err("Unknown UI action capability".into());
            }
        }
        drop(snapshot);
        if let Some(expected) = expected_instance {
            if app
                .state::<crate::services::app_context::AppContext>()
                .state
                .read()
                .await
                .instance
                .instance_id
                != Some(expected)
            {
                return Err("The selected backend instance changed; refresh the UI and context before acting".into());
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.replies.lock().await.insert(id.clone(), sender);
        if let Err(error) = app.emit_to(
            "main",
            "mcp:ui-action",
            json!({"id":id,"kind":kind,"arguments":args,"expiresAt":chrono::Utc::now().timestamp_millis()+10_000}),
        ) {
            self.replies.lock().await.remove(&id);
            return Err(error.to_string());
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), receiver).await;
        self.replies.lock().await.remove(&id);
        result
            .map_err(|_| {
                "The UI did not acknowledge the action; refresh context before retrying".to_string()
            })?
            .map_err(|_| "UI action was interrupted".to_string())?
    }
}
