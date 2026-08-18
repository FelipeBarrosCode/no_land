//! Push an ephemeral rclone session and invoke the state-agent.

use std::time::Duration;

use noland_rclone_adapter::EphemeralRcloneSession;
use serde_json::json;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::app_state::SharedStorageObjectEntry;
use crate::services::app_context::AppContext;
use crate::services::remote_exec::RemoteExec;
use crate::services::shared_storage::agent_runtime::{call_agent_raw, ensure_state_agent};
use crate::services::shared_storage::provider_profiles::shared_profile_manager;
use crate::services::shared_storage::rclone_adapter::mint_ephemeral_session;
use crate::services::shared_storage::shared_storage_manager::SharedStorageManager;

impl SharedStorageManager {
    pub async fn prepare_agent_operation(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<(EphemeralRcloneSession, String)> {
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
        target_user: &str,
        app_id: &str,
        mode: &str,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        call_agent_raw(
            remote,
            "StartBackup",
            json!({
                "app_id": app_id,
                "mode": mode,
                "session": session,
                "master_key_hex": hex,
            }),
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
        target_user: &str,
        app_id: &str,
        bundle_id: &str,
        mode: &str,
    ) -> AppResult<serde_json::Value> {
        let (session, hex) = Self::prepare_agent_operation(context, remote, target_user).await?;
        call_agent_raw(
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
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, payload.as_bytes());
    let cmd = format!(
        "sudo mkdir -p {dir} && sudo chmod 700 {dir} && printf %s {b64} | base64 -d | sudo tee {dir}/session.json >/dev/null && sudo python3 -c \"import json; s=json.load(open('{dir}/session.json')); open('{dir}/rclone.conf','w').write(s.get('config_ini',''))\" && sudo chmod 600 {dir}/rclone.conf {dir}/session.json && sudo chown {user}:{user} {dir}/rclone.conf {dir}/session.json || true",
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
