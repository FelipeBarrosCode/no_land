//! Embedded localhost MCP. Native commands below are main-window-only and are
//! intentionally NOT part of the MCP tool catalog (no self-granting permissions).
mod adapter;
mod catalog;
mod remote;
mod ui;

use crate::{
    errors::{AppError, FrontendError},
    services::{app_context::AppContext, secure_store},
};
use noland_mcp::{
    core::Core,
    policy::{self, Config},
    server::{self, Listener},
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tokio::sync::{Mutex, RwLock};

const TOKEN_SERVICE: &str = "noland-connect.mcp";

pub struct McpManager {
    pub core: Arc<Core>,
    ui: Arc<ui::UiBridge>,
    listener: Mutex<Option<Listener>>,
    error: RwLock<Option<String>>,
    configuration_lock: Mutex<()>,
    token_account: String,
    token_known: AtomicBool,
}

fn main_window(window: &WebviewWindow) -> Result<(), FrontendError> {
    if window.label() != "main" {
        return Err(AppError::InvalidInput(
            "MCP permissions can only be changed from the main app window".into(),
        )
        .into());
    }
    Ok(())
}

async fn token(account: &str) -> Result<String, String> {
    let account = account.to_owned();
    tokio::task::spawn_blocking(move || {
        if let Some(value) =
            secure_store::get(TOKEN_SERVICE, &account).map_err(|e| e.to_string())?
        {
            return Ok(value);
        }
        let value = policy::generate_token();
        secure_store::put(TOKEN_SERVICE, &account, &value).map_err(|e| e.to_string())?;
        Ok(value)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn initialize(app: &AppHandle, context: AppContext) {
    let config = context
        .state
        .try_read()
        .map(|s| s.mcp.clone())
        .unwrap_or_default()
        .for_local_agents();
    let ui = Arc::new(ui::UiBridge::default());
    let directory = context
        .state_store
        .path()
        .parent()
        .expect("state directory")
        .join("mcp");
    let backend = Arc::new(adapter::DesktopBackend {
        app: app.clone(),
        context: context.clone(),
        ui: ui.clone(),
        directory: directory.clone(),
        secrets_cache: std::sync::Mutex::new(Vec::new()),
    });
    let account = format!(
        "localhost-{}",
        policy::fingerprint(&context.state_store.path().to_string_lossy())
    );
    let core = Core::new(
        backend,
        config.clone(),
        &policy::generate_token(),
        directory,
    );
    app.manage(McpManager {
        core: core.clone(),
        ui,
        listener: Mutex::new(None),
        error: RwLock::new(None),
        configuration_lock: Mutex::new(()),
        token_account: account,
        token_known: AtomicBool::new(false),
    });
    if config.enabled {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let manager = app.state::<McpManager>();
            let _lock = manager.configuration_lock.lock().await;
            if manager.core.config.read().await.enabled {
                if let Err(error) = manager.start().await {
                    *manager.error.write().await = Some(error);
                }
            }
        });
    }
}

impl McpManager {
    async fn start(&self) -> Result<(), String> {
        self.core.config.read().await.validate()?;
        let value = token(&self.token_account).await?;
        *self.core.token_hash.write().await = policy::digest(value.as_bytes());
        self.token_known.store(true, Ordering::SeqCst);
        let listener = server::start(self.core.clone()).await?;
        *self.listener.lock().await = Some(listener);
        *self.error.write().await = None;
        Ok(())
    }
    pub async fn stop(&self) {
        self.core.revoke().await;
        if let Some(listener) = self.listener.lock().await.take() {
            listener.stop().await;
        }
    }
    pub async fn status(&self) -> Value {
        let config = self.core.config.read().await.clone();
        let hash = self.core.token_hash.read().await;
        let fingerprint = self.token_known.load(Ordering::SeqCst).then(|| {
            format!(
                "SHA256:{}",
                hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
            )
        });
        drop(hash);
        json!({"config":config,"endpoint":format!("http://127.0.0.1:{}/mcp",config.port),
            "listening":self.listener.lock().await.as_ref().is_some_and(Listener::is_running),"error":self.error.read().await.clone(),
            "fingerprint":fingerprint,"clients":self.core.clients.read().await.values().cloned().collect::<Vec<_>>(),
            "activeJobs":self.core.active_jobs().await,"approvals":self.core.approvals().await,"recentActivity":self.core.recent_events(0).await.into_iter().rev().take(30).collect::<Vec<_>>()})
    }
}

#[tauri::command]
pub async fn mcp_status(app: AppHandle, window: WebviewWindow) -> Result<Value, FrontendError> {
    main_window(&window)?;
    Ok(app.state::<McpManager>().status().await)
}

#[tauri::command]
pub async fn mcp_disable(app: AppHandle, window: WebviewWindow) -> Result<Value, FrontendError> {
    main_window(&window)?;
    let manager = app.state::<McpManager>();
    let _lock = manager.configuration_lock.lock().await;
    // Emergency disable must work even if a transfer directory disappeared or
    // a previously allowed VM no longer exists.
    manager.stop().await;
    manager.core.config.write().await.enabled = false;
    app.state::<AppContext>()
        .update_state(|s| s.mcp.enabled = false)
        .await?;
    *manager.error.write().await = None;
    manager.core.event("server_disabled", json!({})).await;
    Ok(manager.status().await)
}

#[tauri::command]
pub async fn mcp_configure(
    app: AppHandle,
    window: WebviewWindow,
    mut config: Config,
) -> Result<Value, FrontendError> {
    main_window(&window)?;
    config = config.for_local_agents();
    config.validate().map_err(AppError::InvalidInput)?;
    let manager = app.state::<McpManager>();
    let _lock = manager.configuration_lock.lock().await;
    let context = app.state::<AppContext>();
    let state = context.load_state().await;
    if config.instances.iter().any(|id| {
        state.instance.instance_id != Some(*id)
            && !state
                .provisioned_servers
                .iter()
                .any(|s| s.instance_id == *id)
    }) {
        return Err(
            AppError::InvalidInput("Grant access only to registered instances".into()).into(),
        );
    }
    config.instances.sort_unstable();
    config.instances.dedup();
    config.local_roots = config
        .local_roots
        .iter()
        .map(|r| std::fs::canonicalize(r).map(|p| p.to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::InvalidInput(e.to_string()))?;
    // Resolve keychain access before publishing an enabled configuration.
    if config.enabled {
        token(&manager.token_account)
            .await
            .map_err(AppError::SecureStorage)?;
    }
    manager.stop().await;
    if let Err(error) = context
        .update_state(|state| state.mcp = config.clone())
        .await
    {
        *manager.error.write().await =
            Some("MCP stopped because settings could not be saved".into());
        return Err(error.into());
    }
    *manager.core.config.write().await = config.clone();
    if config.enabled {
        if let Err(error) = manager.start().await {
            *manager.error.write().await = Some(error);
        }
    } else {
        *manager.error.write().await = None;
    }
    manager
        .core
        .event("configuration_changed", json!({"enabled":config.enabled}))
        .await;
    Ok(manager.status().await)
}

#[tauri::command]
pub async fn mcp_access_token(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<String, FrontendError> {
    main_window(&window)?;
    let manager = app.state::<McpManager>();
    let _lock = manager.configuration_lock.lock().await;
    let value = token(&manager.token_account)
        .await
        .map_err(AppError::SecureStorage)?;
    *manager.core.token_hash.write().await = policy::digest(value.as_bytes());
    manager.token_known.store(true, Ordering::SeqCst);
    Ok(value)
}

#[tauri::command]
pub async fn mcp_rotate_token(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Value, FrontendError> {
    main_window(&window)?;
    let manager = app.state::<McpManager>();
    let _lock = manager.configuration_lock.lock().await;
    let value = policy::generate_token();
    let stored = value.clone();
    let account = manager.token_account.clone();
    tokio::task::spawn_blocking(move || secure_store::put(TOKEN_SERVICE, &account, &stored))
        .await
        .map_err(|e| AppError::Command(e.to_string()))??;
    manager.stop().await;
    *manager.core.token_hash.write().await = policy::digest(value.as_bytes());
    manager.token_known.store(true, Ordering::SeqCst);
    if manager.core.config.read().await.enabled {
        if let Err(error) = manager.start().await {
            *manager.error.write().await = Some(error);
        }
    }
    manager.core.event("token_rotated", json!({})).await;
    Ok(manager.status().await)
}

#[tauri::command]
pub async fn mcp_disconnect_agents(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Value, FrontendError> {
    main_window(&window)?;
    let manager = app.state::<McpManager>();
    let _lock = manager.configuration_lock.lock().await;
    manager.stop().await;
    if manager.core.config.read().await.enabled {
        if let Err(error) = manager.start().await {
            *manager.error.write().await = Some(error);
        }
    }
    manager.core.event("agents_disconnected", json!({})).await;
    Ok(manager.status().await)
}

#[tauri::command]
pub async fn mcp_revoke_token(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Value, FrontendError> {
    main_window(&window)?;
    let manager = app.state::<McpManager>();
    let _lock = manager.configuration_lock.lock().await;
    manager.stop().await;
    manager.core.config.write().await.enabled = false;
    *manager.core.token_hash.write().await = policy::digest(policy::generate_token().as_bytes());
    manager.token_known.store(false, Ordering::SeqCst);
    let account = manager.token_account.clone();
    tokio::task::spawn_blocking(move || secure_store::delete(TOKEN_SERVICE, &account))
        .await
        .map_err(|e| AppError::Command(e.to_string()))??;
    app.state::<AppContext>()
        .update_state(|s| s.mcp.enabled = false)
        .await?;
    *manager.error.write().await = None;
    manager.core.event("token_revoked", json!({})).await;
    Ok(manager.status().await)
}

#[tauri::command]
pub async fn mcp_decide_approval(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    approve: bool,
) -> Result<(), FrontendError> {
    main_window(&window)?;
    app.state::<McpManager>()
        .core
        .decide(&id, approve)
        .await
        .map_err(|e| AppError::InvalidInput(e).into())
}

#[tauri::command]
pub async fn mcp_publish_ui(
    app: AppHandle,
    window: WebviewWindow,
    snapshot: Value,
) -> Result<(), FrontendError> {
    main_window(&window)?;
    let manager = app.state::<McpManager>();
    let snapshot = manager.core.backend.sanitize(snapshot).await;
    manager
        .ui
        .publish(snapshot)
        .await
        .map_err(AppError::InvalidInput)?;
    Ok(())
}

#[tauri::command]
pub async fn mcp_complete_ui(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    error: Option<String>,
) -> Result<(), FrontendError> {
    main_window(&window)?;
    app.state::<McpManager>().ui.complete(&id,match error {Some(error)=>Err(error),None=>Ok(json!({"status":"acknowledged","message":"UI action handled; refresh state to verify the resulting operation"}))}).await;
    Ok(())
}
