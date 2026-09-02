//! Push an ephemeral rclone session and invoke the state-agent.

use std::time::Duration;

use noland_rclone_adapter::EphemeralRcloneSession;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::app_state::{BackupPerformanceMode, SharedStorageObjectEntry};
use crate::models::events::SharedStorageProgressEvent;
use crate::services::app_context::{ActiveAgentOperation, AppContext};
use crate::services::remote_exec::RemoteExec;
use crate::services::shared_storage::agent_runtime::{call_agent_raw, ensure_state_agent};
use crate::services::shared_storage::provider_profiles::shared_profile_manager;
use crate::services::shared_storage::rclone_adapter::mint_ephemeral_session;
use crate::services::shared_storage::shared_storage_manager::SharedStorageManager;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct AgentAppRecord {
    #[serde(alias = "appId")]
    pub app_id: String,
    #[serde(alias = "displayName")]
    pub display_name: String,
    #[serde(default, alias = "canonicalExecutable")]
    pub canonical_executable: Option<String>,
    #[serde(default, alias = "desktopEntryId")]
    pub desktop_entry_id: Option<String>,
    #[serde(default, alias = "steamAppId")]
    pub steam_app_id: Option<u32>,
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, alias = "iconPath")]
    pub icon_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentCatalogAppRecord {
    #[serde(alias = "appId")]
    pub app_id: String,
    #[serde(alias = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, alias = "canonicalExecutable")]
    pub canonical_executable: Option<String>,
    #[serde(default, alias = "desktopEntryId")]
    pub desktop_entry_id: Option<String>,
    #[serde(default, alias = "steamAppId")]
    pub steam_app_id: Option<u32>,
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default, alias = "iconPath")]
    pub icon_path: Option<String>,
    #[serde(default, alias = "latestBundleId")]
    pub latest_bundle_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentCatalogDocument {
    #[serde(default)]
    apps: Vec<AgentCatalogAppRecord>,
}

impl SharedStorageManager {
    pub async fn prepare_agent_operation(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<(EphemeralRcloneSession, String)> {
        Self::ensure_rclone_installed(remote).await?;
        ensure_state_agent(remote, target_user).await?;
        let session_op = Uuid::new_v4().to_string();
        let session = Self::mint_session(context, &session_op).await?;
        push_session(remote, target_user, &session).await?;
        let key_hex = Self::master_key_hex(context).await?;
        Ok((session, key_hex))
    }

    pub async fn start_agent_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        app_id: &str,
        mode: &str,
        performance_mode: BackupPerformanceMode,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        if let Some(operation_id) =
            find_interrupted_operation(remote, "backup", Some(app_id), None).await?
        {
            let queued = call_agent_raw(
                remote,
                "RetryOperation",
                json!({
                    "operation_id": operation_id,
                    "session": session,
                    "master_key_hex": hex,
                }),
            )
            .await?;
            let operation_id = queued
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(operation_id.as_str());
            return wait_for_agent_operation(
                context,
                remote,
                instance_id,
                operation_id,
                "backup",
                Duration::from_secs(2 * 60 * 60),
            )
            .await;
        }
        let queued = call_agent_raw(
            remote,
            "StartBackup",
            json!({
                "app_id": app_id,
                "mode": mode,
                "performance_mode": performance_mode,
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await?;
        let operation_id = queued
            .get("operation_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AppError::State("state-agent backup returned no operation_id".into()))?;
        wait_for_agent_operation(
            context,
            remote,
            instance_id,
            operation_id,
            "backup",
            Duration::from_secs(2 * 60 * 60),
        )
        .await
    }

    pub async fn start_agent_seal(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
        mode: &str,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        call_agent_raw(
            remote,
            "StartSeal",
            json!({
                "mode": mode,
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await
    }

    pub async fn list_agent_catalog(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        call_agent_raw(
            remote,
            "ListCloudCatalog",
            json!({
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await
    }

    pub async fn start_agent_restore(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        app_id: &str,
        bundle_id: &str,
        mode: &str,
    ) -> AppResult<serde_json::Value> {
        let (session, hex) = Self::prepare_agent_operation(context, remote, target_user).await?;
        if let Some(operation_id) =
            find_interrupted_operation(remote, "restore", Some(app_id), Some(bundle_id)).await?
        {
            let queued = call_agent_raw(
                remote,
                "RetryOperation",
                json!({
                    "operation_id": operation_id,
                    "session": session,
                    "master_key_hex": hex,
                }),
            )
            .await?;
            let operation_id = queued
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(operation_id.as_str());
            return wait_for_agent_operation(
                context,
                remote,
                instance_id,
                operation_id,
                "restore",
                Duration::from_secs(2 * 60 * 60),
            )
            .await;
        }
        let queued = call_agent_raw(
            remote,
            "StartRestore",
            json!({
                "app_id": app_id,
                "bundle_id": bundle_id,
                "mode": mode,
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await?;
        let operation_id = queued
            .get("operation_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                AppError::State("state-agent restore returned no operation_id".into())
            })?;
        wait_for_agent_operation(
            context,
            remote,
            instance_id,
            operation_id,
            "restore",
            Duration::from_secs(2 * 60 * 60),
        )
        .await
    }

    pub async fn list_agent_apps(
        _context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<SharedStorageObjectEntry>> {
        ensure_state_agent(remote, target_user).await?;
        let apps = call_agent_raw(remote, "ListApps", json!({})).await?;
        Ok(apps_to_entries(&apps))
    }

    pub async fn catalog_to_entries(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<SharedStorageObjectEntry>> {
        let catalog = Self::list_agent_catalog(context, remote, target_user, None).await?;
        Ok(catalog_to_entries(&catalog))
    }

    pub(crate) async fn list_agent_app_records(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<AgentAppRecord>> {
        ensure_state_agent(remote, target_user).await?;
        let apps = call_agent_raw(remote, "ListApps", json!({})).await?;
        parse_agent_apps(&apps)
    }

    pub(crate) async fn list_agent_catalog_records(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<AgentCatalogAppRecord>> {
        let catalog = Self::list_agent_catalog(context, remote, target_user, None).await?;
        parse_agent_catalog(&catalog)
    }

    async fn mint_session(
        context: &AppContext,
        operation_id: &str,
    ) -> AppResult<EphemeralRcloneSession> {
        let active = Self::resolve_active_profile(context).await?;
        mint_ephemeral_session(
            &active.profile.provider,
            &active.credentials,
            &active.provider_fields,
            active.profile.bucket.as_deref(),
            active.profile.prefix.as_deref(),
            &active.remote_name,
            operation_id,
        )
    }

    async fn master_key_hex(context: &AppContext) -> AppResult<String> {
        let active = Self::resolve_active_profile(context).await?;
        let key = shared_profile_manager()
            .retrieve_repository_key(context, &active.profile)
            .await?;
        Ok(hex::encode(key))
    }
}

async fn find_interrupted_operation(
    remote: &RemoteExec,
    kind: &str,
    app_id: Option<&str>,
    bundle_id: Option<&str>,
) -> AppResult<Option<String>> {
    let recent = call_agent_raw(remote, "ListRecentOperations", json!({"limit": 100})).await?;
    let operations = recent
        .get("operations")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for operation in operations {
        if operation.get("kind").and_then(serde_json::Value::as_str) != Some(kind)
            || operation.get("state").and_then(serde_json::Value::as_str) != Some("INTERRUPTED")
        {
            continue;
        }
        let detail = operation.get("detail_json");
        let persisted_app = operation
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                detail
                    .and_then(|value| value.get("app_id"))
                    .and_then(serde_json::Value::as_str)
            });
        let app_matches = match app_id {
            Some("*") | Some("_all") => persisted_app.is_none(),
            Some(expected) => persisted_app == Some(expected),
            None => true,
        };
        let bundle_matches = bundle_id.is_none_or(|expected| {
            detail
                .and_then(|value| value.get("bundle_id"))
                .and_then(serde_json::Value::as_str)
                == Some(expected)
        });
        if app_matches && bundle_matches {
            return Ok(operation
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string));
        }
    }
    Ok(None)
}

async fn wait_for_agent_operation(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    operation_id: &str,
    kind: &str,
    timeout: Duration,
) -> AppResult<serde_json::Value> {
    {
        let mut active = context.active_agent_operation.write().await;
        *active = Some(ActiveAgentOperation {
            instance_id,
            operation_id: operation_id.to_string(),
            kind: kind.to_string(),
        });
    }
    let result = tokio::time::timeout(timeout, async {
        loop {
            let status = call_agent_raw(
                remote,
                "GetOperationStatus",
                json!({"operation_id": operation_id}),
            )
            .await?;
            emit_operation_progress(context, instance_id, kind, operation_id, &status);
            let ready_to_launch = status
                .get("progress")
                .and_then(|progress| progress.get("detail_json"))
                .and_then(|detail| detail.get("ready_to_launch_reached"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if ready_to_launch {
                return Ok(status);
            }
            match status
                .get("state")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("UNKNOWN")
            {
                "COMPLETED" | "SEALED" | "ROLLED_BACK" => return Ok(status),
                "FAILED" => {
                    let error = status
                        .get("last_error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("state-agent operation failed");
                    return Err(AppError::Provisioning(error.to_string()));
                }
                "CANCELLED" => {
                    return Err(AppError::State(format!(
                        "state-agent operation {operation_id} was cancelled"
                    )));
                }
                _ => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    })
    .await
    .map_err(|_| {
        AppError::Command(format!(
            "timed out waiting for state-agent operation {operation_id}"
        ))
    });
    {
        let mut active = context.active_agent_operation.write().await;
        if active
            .as_ref()
            .is_some_and(|operation| operation.operation_id == operation_id)
        {
            *active = None;
        }
    }
    result?
}

fn emit_operation_progress(
    context: &AppContext,
    instance_id: u64,
    kind: &str,
    operation_id: &str,
    status: &serde_json::Value,
) {
    let progress = status.get("progress");
    let phase = progress
        .and_then(|value| value.get("phase"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let message = progress
        .and_then(|value| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            status
                .get("state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    let completed_units = progress
        .and_then(|value| value.get("completed_units"))
        .and_then(serde_json::Value::as_u64);
    let total_units = progress
        .and_then(|value| value.get("total_units"))
        .and_then(serde_json::Value::as_u64);
    let unit = progress
        .and_then(|value| value.get("unit"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let fraction = match (completed_units, total_units) {
        (Some(completed), Some(total)) if total > 0 => {
            Some((completed as f64 / total as f64).clamp(0.0, 1.0))
        }
        _ => None,
    };
    let ready_to_launch = phase.as_deref() == Some("READY_TO_LAUNCH")
        || progress
            .and_then(|value| value.get("detail_json"))
            .and_then(|value| value.get("ready_to_launch_reached"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        || progress
            .and_then(|value| value.get("detail_json"))
            .and_then(|value| value.get("milestones"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|milestones| {
                milestones
                    .iter()
                    .any(|value| value.as_str() == Some("READY_TO_LAUNCH"))
            });
    let state = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_string();
    let cancel_requested = status
        .get("cancel_requested")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let running = status
        .get("running")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    context.emit_shared_storage_progress(SharedStorageProgressEvent {
        operation_id: operation_id.to_string(),
        instance_id,
        kind: kind.to_string(),
        state,
        phase,
        message,
        completed_units,
        total_units,
        unit,
        fraction,
        ready_to_launch,
        cancel_requested,
        cancellable: running && !cancel_requested,
    });
}

fn parse_agent_apps(value: &serde_json::Value) -> AppResult<Vec<AgentAppRecord>> {
    serde_json::from_value::<Vec<AgentAppRecord>>(value.clone())
        .map(|apps| apps.into_iter().map(AgentAppRecord::normalize).collect())
        .map_err(|error| {
            AppError::State(format!("state-agent ListApps payload is invalid: {error}"))
        })
}

fn parse_agent_catalog(value: &serde_json::Value) -> AppResult<Vec<AgentCatalogAppRecord>> {
    serde_json::from_value::<AgentCatalogDocument>(value.clone())
        .map(|catalog| {
            catalog
                .apps
                .into_iter()
                .map(AgentCatalogAppRecord::normalize)
                .collect()
        })
        .map_err(|error| {
            AppError::State(format!(
                "state-agent ListCloudCatalog payload is invalid: {error}"
            ))
        })
}

impl AgentAppRecord {
    fn normalize(mut self) -> Self {
        self.display_name = self.display_name.trim().to_string();
        self.canonical_executable = normalized_optional_string(self.canonical_executable.take());
        self.desktop_entry_id = normalized_optional_string(self.desktop_entry_id.take());
        self.launcher = normalized_launcher(self.launcher.take());
        self.steam_app_id = self
            .steam_app_id
            .or_else(|| infer_steam_app_id(&self.app_id, self.launcher.as_deref()));
        if self.launcher.is_none() && steam_like_app_id(&self.app_id, self.steam_app_id) {
            self.launcher = Some("steam".to_string());
        }
        self.aliases = normalized_aliases(self.aliases, &self.display_name);
        self.icon_path = normalized_optional_string(self.icon_path.take());
        self
    }
}

impl AgentCatalogAppRecord {
    fn normalize(mut self) -> Self {
        self.display_name = self.display_name.trim().to_string();
        self.canonical_executable = normalized_optional_string(self.canonical_executable.take());
        self.desktop_entry_id = normalized_optional_string(self.desktop_entry_id.take());
        self.launcher = normalized_launcher(self.launcher.take());
        self.steam_app_id = self
            .steam_app_id
            .or_else(|| infer_steam_app_id(&self.app_id, self.launcher.as_deref()));
        if self.launcher.is_none() && steam_like_app_id(&self.app_id, self.steam_app_id) {
            self.launcher = Some("steam".to_string());
        }
        self.aliases = normalized_aliases(self.aliases, &self.display_name);
        self.icon_path = normalized_optional_string(self.icon_path.take());
        self.latest_bundle_id = normalized_optional_string(self.latest_bundle_id.take());
        self
    }
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn normalized_launcher(value: Option<String>) -> Option<String> {
    normalized_optional_string(value).map(|value| value.to_ascii_lowercase())
}

fn normalized_aliases(aliases: Vec<String>, display_name: &str) -> Vec<String> {
    let mut normalized = Vec::new();
    for alias in aliases {
        let trimmed = alias.trim();
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case(display_name)
            || normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn infer_steam_app_id(app_id: &str, launcher: Option<&str>) -> Option<u32> {
    let app_id = app_id.trim();
    if let Some(value) = app_id.strip_prefix("steam:") {
        return value.parse().ok();
    }
    launcher
        .filter(|value| value.trim().eq_ignore_ascii_case("steam"))
        .and_then(|_| app_id.parse().ok())
}

fn steam_like_app_id(app_id: &str, steam_app_id: Option<u32>) -> bool {
    steam_app_id.is_some() || app_id.trim().starts_with("steam:")
}

fn looks_like_image_utility(id: &str, name: &str) -> bool {
    let hay = format!("{} {}", id, name).to_ascii_lowercase();
    const KEEP: &[&str] = &[
        "steam",
        "lutris",
        "heroic",
        "bottles",
        "playonlinux",
        "q4wine",
        "winetricks",
        "wine",
        "proton",
        "minecraft",
        "epic",
        "gog",
        "itch",
    ];
    if KEEP.iter().any(|m| hay.contains(m)) {
        return false;
    }
    const DROP: &[&str] = &[
        "kate",
        "dolphin",
        "konsole",
        "kwrite",
        "spectacle",
        "okular",
        "gwenview",
        "kmail",
        "korganizer",
        "kaddressbook",
        "knotes",
        "kcalc",
        "kfind",
        "klipper",
        "konqueror",
        "sweeper",
        "akregator",
        "ark",
        "juk",
        "dragon",
        "bluetooth",
        "muon",
        "discover",
        "systemsettings",
        "system settings",
        "htop",
        "vim",
        "systemd",
        "sunshine",
        "nvidia",
        "byobu",
        "texinfo",
        "idle",
        "imagemagick",
        "kde connect",
        "kwallet",
        "partition",
        "info center",
        "help",
        "emoji",
        "qsynth",
        "sieve",
        "ktnef",
        "contact print",
        "contact theme",
        "software sources",
        "additional drivers",
        "terminal",
        "add bluetooth",
    ];
    DROP.iter().any(|m| hay.contains(m))
}

fn apps_to_entries(apps: &serde_json::Value) -> Vec<SharedStorageObjectEntry> {
    let mut out = vec![SharedStorageObjectEntry {
        path: "/apps".into(),
        name: "Applications".into(),
        parent_path: "/".into(),
        is_dir: true,
    }];
    let list = apps.as_array().cloned().unwrap_or_default();
    for app in list {
        let id = app
            .get("app_id")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("appId").and_then(|v| v.as_str()))
            .unwrap_or("unknown");
        let name = app
            .get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(id);
        if looks_like_image_utility(id, name) {
            continue;
        }
        out.push(SharedStorageObjectEntry {
            path: format!("/apps/{id}"),
            name: name.into(),
            parent_path: "/apps".into(),
            is_dir: false,
        });
    }
    out.insert(
        1,
        SharedStorageObjectEntry {
            path: "/apps/*".into(),
            name: "All discovered applications".into(),
            parent_path: "/apps".into(),
            is_dir: false,
        },
    );
    out
}

fn catalog_to_entries(catalog: &serde_json::Value) -> Vec<SharedStorageObjectEntry> {
    let mut out = vec![SharedStorageObjectEntry {
        path: "/catalog".into(),
        name: "Shared Storage apps".into(),
        parent_path: "/".into(),
        is_dir: true,
    }];
    let apps = catalog
        .get("apps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for app in apps {
        let id = app
            .get("app_id")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("appId").and_then(|v| v.as_str()))
            .unwrap_or("unknown");
        let name = app
            .get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(id);
        let app_path = format!("/catalog/{id}");
        out.push(SharedStorageObjectEntry {
            path: app_path.clone(),
            name: name.into(),
            parent_path: "/catalog".into(),
            is_dir: true,
        });
        if let Some(bundles) = app.get("bundles").and_then(|v| v.as_array()) {
            for bundle in bundles {
                let bundle_id = bundle
                    .get("bundle_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| bundle.get("bundleId").and_then(|v| v.as_str()))
                    .unwrap_or("latest");
                out.push(SharedStorageObjectEntry {
                    path: format!("{app_path}/{bundle_id}"),
                    name: format!("bundle {bundle_id}"),
                    parent_path: app_path.clone(),
                    is_dir: false,
                });
            }
        } else if let Some(latest) = app
            .get("latest_bundle_id")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("latestBundleId").and_then(|v| v.as_str()))
        {
            out.push(SharedStorageObjectEntry {
                path: format!("{app_path}/{latest}"),
                name: "latest bundle".into(),
                parent_path: app_path,
                is_dir: false,
            });
        }
    }
    out
}

async fn push_session(
    remote: &RemoteExec,
    target_user: &str,
    session: &EphemeralRcloneSession,
) -> AppResult<()> {
    let dir = format!("/run/noland/storage/{}", session.operation_id);
    let payload = serde_json::to_string(session)
        .map_err(|e| AppError::State(format!("serialize session: {e}")))?;
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        payload.as_bytes(),
    );
    let cmd = format!(
        "sudo mkdir -p {dir} && sudo chmod 700 {dir} && printf %s {b64} | base64 -d | sudo tee {dir}/session.json >/dev/null && sudo python3 -c \"import json; s=json.load(open('{dir}/session.json')); open('{dir}/rclone.conf','w').write(s.get('config_ini',''))\" && sudo chmod 600 {dir}/rclone.conf {dir}/session.json && sudo chown {user}: {dir} {dir}/rclone.conf {dir}/session.json || true",
        dir = dir,
        b64 = shell_escape(&encoded),
        user = shell_escape(target_user),
    );
    let output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??
    };
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed to push ephemeral rclone session: {} {}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }
    Ok(())
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::{parse_agent_apps, parse_agent_catalog};
    use serde_json::json;

    #[test]
    fn parses_agent_apps_with_launch_metadata() {
        let apps = parse_agent_apps(&json!([{
            "app_id": "steam:480",
            "display_name": "Spacewar",
            "canonical_executable": "/games/spacewar",
            "desktop_entry_id": null,
            "steam_app_id": 480,
            "launcher": "steam",
            "aliases": ["Space War"],
            "icon_path": "/icons/spacewar.png"
        }]))
        .expect("valid app payload");

        assert_eq!(apps[0].app_id, "steam:480");
        assert_eq!(apps[0].steam_app_id, Some(480));
        assert_eq!(
            apps[0].canonical_executable.as_deref(),
            Some("/games/spacewar")
        );
    }

    #[test]
    fn parses_catalog_camel_case_compatibly() {
        let apps = parse_agent_catalog(&json!({
            "apps": [{
                "appId": "desktop:org.example.Game",
                "displayName": "Example Game",
                "latestBundleId": "f30a42a8-3dc9-4aea-a71c-f57f4b66bbef",
                "bundles": []
            }]
        }))
        .expect("valid catalog payload");

        assert_eq!(apps[0].app_id, "desktop:org.example.Game");
        assert_eq!(
            apps[0].latest_bundle_id.as_deref(),
            Some("f30a42a8-3dc9-4aea-a71c-f57f4b66bbef")
        );
    }

    #[test]
    fn infers_missing_steam_metadata_from_sparse_agent_payloads() {
        let apps = parse_agent_apps(&json!([{
            "app_id": "steam:3241660",
            "display_name": "R.E.P.O.",
            "launcher": "  ",
            "aliases": ["REPO", " repo ", "R.E.P.O."]
        }]))
        .expect("valid app payload");

        assert_eq!(apps[0].steam_app_id, Some(3_241_660));
        assert_eq!(apps[0].launcher.as_deref(), Some("steam"));
        assert_eq!(apps[0].aliases, vec!["REPO".to_string()]);
    }

    #[test]
    fn infers_numeric_steam_app_ids_from_catalog_when_launcher_is_steam() {
        let apps = parse_agent_catalog(&json!({
            "apps": [{
                "appId": "3241660",
                "displayName": "R.E.P.O.",
                "launcher": "Steam",
                "latestBundleId": "f30a42a8-3dc9-4aea-a71c-f57f4b66bbef"
            }]
        }))
        .expect("valid catalog payload");

        assert_eq!(apps[0].steam_app_id, Some(3_241_660));
        assert_eq!(apps[0].launcher.as_deref(), Some("steam"));
    }
}
