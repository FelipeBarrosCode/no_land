use std::{collections::BTreeMap, time::Duration};

use base64::Engine;
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    models::launch_library::{LaunchLibraryItem, LaunchLibraryResponse},
    services::{
        app_context::AppContext,
        remote_exec::RemoteExec,
        shared_storage::{
            agent_handoff::{AgentAppRecord, AgentCatalogAppRecord},
            shared_storage_manager::SharedStorageManager,
        },
    },
};

#[derive(Debug, Clone)]
pub(crate) struct LaunchLibraryEntry {
    pub item: LaunchLibraryItem,
    pub canonical_executable: Option<String>,
    pub desktop_entry_id: Option<String>,
    pub steam_app_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchPlan {
    Steam(u32),
    Desktop(String),
    Executable(String),
}

pub(crate) async fn load_launch_library(
    context: &AppContext,
    remote: &RemoteExec,
    target_user: &str,
    instance_id: u64,
    launch_pc_available: bool,
) -> AppResult<(LaunchLibraryResponse, Vec<LaunchLibraryEntry>)> {
    let local = SharedStorageManager::list_agent_app_records(remote, target_user).await?;
    let catalog = match SharedStorageManager::list_agent_catalog_records(
        context,
        remote,
        target_user,
    )
    .await
    {
        Ok(catalog) => catalog,
        Err(error) => {
            warn!(instance_id, %error, "Shared-storage catalog is unavailable; returning installed launch apps only");
            Vec::new()
        }
    };
    let entries = merge_launch_library(local, catalog);
    let response = LaunchLibraryResponse {
        instance_id,
        launch_pc_available,
        items: entries.iter().map(|entry| entry.item.clone()).collect(),
    };
    Ok((response, entries))
}

pub(crate) async fn launch_remote_software(
    remote: &RemoteExec,
    target_user: &str,
    entry: &LaunchLibraryEntry,
) -> AppResult<()> {
    let plan = launch_plan(entry).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "{} cannot be launched automatically because no Steam ID, desktop entry, or executable was discovered. Launch it from the streamed desktop instead.",
            entry.item.display_name
        ))
    })?;
    let script = launch_script(&plan)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    let remote_command = format!(
        "printf %s {payload} | base64 -d | sudo -u {user} sh",
        payload = shell_quote(&encoded),
        user = shell_quote(target_user),
    );
    let output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh(&remote_command, Duration::from_secs(30)))
            .await
            .map_err(|error| AppError::Command(format!("software launch task failed: {error}")))??
    };
    if output.status_code != 0 {
        let details = [output.stderr.trim(), output.stdout.trim()]
            .into_iter()
            .find(|value| !value.is_empty())
            .unwrap_or("remote launcher returned no details");
        return Err(AppError::Command(format!(
            "Could not launch {}: {}",
            entry.item.display_name, details
        )));
    }
    Ok(())
}

fn merge_launch_library(
    local: Vec<AgentAppRecord>,
    catalog: Vec<AgentCatalogAppRecord>,
) -> Vec<LaunchLibraryEntry> {
    let mut merged = BTreeMap::<String, LaunchLibraryEntry>::new();
    for app in local {
        let method = launch_method(
            &app.app_id,
            app.steam_app_id,
            app.desktop_entry_id.as_deref(),
            app.canonical_executable.as_deref(),
        );
        let display_name = non_empty_or(&app.display_name, &app.app_id);
        merged.insert(
            app.app_id.clone(),
            LaunchLibraryEntry {
                item: LaunchLibraryItem {
                    app_id: app.app_id,
                    display_name: display_name.clone(),
                    aliases: unique_aliases(app.aliases, &display_name),
                    installed: true,
                    in_shared_storage: false,
                    latest_bundle_id: None,
                    source_labels: vec!["Installed".to_string()],
                    launchable: method.is_some(),
                    launch_method: method.unwrap_or_default(),
                    restore_required: false,
                    artwork_key: display_name.clone(),
                },
                canonical_executable: app.canonical_executable,
                desktop_entry_id: app.desktop_entry_id,
                steam_app_id: app.steam_app_id,
            },
        );
    }

    for app in catalog {
        if let Some(existing) = merged.get_mut(&app.app_id) {
            existing.item.in_shared_storage = true;
            existing.item.latest_bundle_id = app.latest_bundle_id;
            existing
                .item
                .source_labels
                .push("Shared storage".to_string());
            if !app.display_name.trim().is_empty()
                && app.display_name != existing.item.display_name
                && !existing.item.aliases.contains(&app.display_name)
            {
                existing.item.aliases.push(app.display_name.clone());
            }
            for alias in app.aliases {
                let alias = alias.trim().to_string();
                if !alias.is_empty()
                    && alias != existing.item.display_name
                    && !existing.item.aliases.contains(&alias)
                {
                    existing.item.aliases.push(alias);
                }
            }
            if existing.canonical_executable.is_none() {
                existing.canonical_executable = app.canonical_executable;
            }
            if existing.desktop_entry_id.is_none() {
                existing.desktop_entry_id = app.desktop_entry_id;
            }
            if existing.steam_app_id.is_none() {
                existing.steam_app_id = app.steam_app_id;
            }
            let method = launch_method(
                &existing.item.app_id,
                existing.steam_app_id,
                existing.desktop_entry_id.as_deref(),
                existing.canonical_executable.as_deref(),
            );
            existing.item.launchable = method.is_some();
            existing.item.launch_method = method.unwrap_or_default();
            continue;
        }

        let display_name = non_empty_or(&app.display_name, &app.app_id);
        let method = launch_method(
            &app.app_id,
            app.steam_app_id,
            app.desktop_entry_id.as_deref(),
            app.canonical_executable.as_deref(),
        );
        let restore_required = app.latest_bundle_id.is_some();
        merged.insert(
            app.app_id.clone(),
            LaunchLibraryEntry {
                item: LaunchLibraryItem {
                    app_id: app.app_id,
                    display_name: display_name.clone(),
                    aliases: unique_aliases(app.aliases, &display_name),
                    installed: false,
                    in_shared_storage: true,
                    latest_bundle_id: app.latest_bundle_id,
                    source_labels: vec!["Shared storage".to_string()],
                    launchable: restore_required && method.is_some(),
                    launch_method: method.unwrap_or_default(),
                    restore_required,
                    artwork_key: display_name.clone(),
                },
                canonical_executable: app.canonical_executable,
                desktop_entry_id: app.desktop_entry_id,
                steam_app_id: app.steam_app_id,
            },
        );
    }

    let mut entries = merged.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.item
            .display_name
            .to_ascii_lowercase()
            .cmp(&right.item.display_name.to_ascii_lowercase())
            .then_with(|| left.item.app_id.cmp(&right.item.app_id))
    });
    entries
}

fn launch_plan(entry: &LaunchLibraryEntry) -> Option<LaunchPlan> {
    if let Some(steam_id) = entry
        .steam_app_id
        .or_else(|| parse_steam_app_id(&entry.item.app_id))
    {
        return Some(LaunchPlan::Steam(steam_id));
    }
    if let Some(desktop_id) = entry
        .desktop_entry_id
        .as_deref()
        .filter(|value| valid_desktop_id(value))
        .map(ToOwned::to_owned)
        .or_else(|| parse_desktop_id(&entry.item.app_id))
    {
        return Some(LaunchPlan::Desktop(desktop_id));
    }
    entry
        .canonical_executable
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.contains(['\n', '\r', '\0']))
        .map(|value| LaunchPlan::Executable(value.to_string()))
}

fn launch_method(
    app_id: &str,
    steam_app_id: Option<u32>,
    desktop_entry_id: Option<&str>,
    canonical_executable: Option<&str>,
) -> Option<String> {
    let entry = LaunchLibraryEntry {
        item: LaunchLibraryItem {
            app_id: app_id.to_string(),
            display_name: String::new(),
            aliases: Vec::new(),
            installed: false,
            in_shared_storage: false,
            latest_bundle_id: None,
            source_labels: Vec::new(),
            launchable: false,
            launch_method: String::new(),
            restore_required: false,
            artwork_key: String::new(),
        },
        canonical_executable: canonical_executable.map(ToOwned::to_owned),
        desktop_entry_id: desktop_entry_id.map(ToOwned::to_owned),
        steam_app_id,
    };
    launch_plan(&entry).map(|plan| {
        match plan {
            LaunchPlan::Steam(_) => "steam",
            LaunchPlan::Desktop(_) => "desktop",
            LaunchPlan::Executable(_) => "executable",
        }
        .to_string()
    })
}

fn launch_script(plan: &LaunchPlan) -> AppResult<String> {
    let launch = match plan {
        LaunchPlan::Steam(app_id) => format!(
            "if ! command -v steam >/dev/null 2>&1; then echo 'Steam is not installed on this instance.' >&2; exit 127; fi\nnohup steam -applaunch {app_id} >\"/tmp/noland-launch-steam-{app_id}.log\" 2>&1 &"
        ),
        LaunchPlan::Desktop(desktop_id) => {
            if !valid_desktop_id(desktop_id) {
                return Err(AppError::InvalidInput(
                    "The discovered desktop application ID is invalid.".to_string(),
                ));
            }
            format!(
                "if ! command -v gtk-launch >/dev/null 2>&1; then echo 'gtk-launch is not installed on this instance.' >&2; exit 127; fi\ngtk-launch {} >/tmp/noland-launch-desktop.log 2>&1",
                shell_quote(desktop_id)
            )
        }
        LaunchPlan::Executable(executable) => format!(
            "if [ ! -x {exe} ]; then echo 'The discovered executable is missing or is not executable: {display}' >&2; exit 126; fi\nnohup {exe} >/tmp/noland-launch-executable.log 2>&1 &",
            exe = shell_quote(executable),
            display = executable.replace('\'', ""),
        ),
    };
    Ok(format!(
        "export DISPLAY=\"${{DISPLAY:-:0}}\"\nexport XDG_RUNTIME_DIR=\"${{XDG_RUNTIME_DIR:-/run/user/$(id -u)}}\"\nexport DBUS_SESSION_BUS_ADDRESS=\"${{DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}}\"\n{launch}\n"
    ))
}

fn parse_steam_app_id(app_id: &str) -> Option<u32> {
    app_id.strip_prefix("steam:")?.parse().ok()
}

fn parse_desktop_id(app_id: &str) -> Option<String> {
    let value = app_id.strip_prefix("desktop:")?;
    valid_desktop_id(value).then(|| value.to_string())
}

fn valid_desktop_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn unique_aliases(aliases: Vec<String>, display_name: &str) -> Vec<String> {
    let mut unique = Vec::new();
    for alias in aliases {
        let alias = alias.trim().to_string();
        if !alias.is_empty()
            && alias != display_name
            && !unique.iter().any(|existing| existing == &alias)
        {
            unique.push(alias);
        }
    }
    unique
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::{
        launch_plan, merge_launch_library, AgentAppRecord, AgentCatalogAppRecord, LaunchPlan,
    };

    #[test]
    fn merges_installed_and_cloud_entries_by_app_id() {
        let entries = merge_launch_library(
            vec![AgentAppRecord {
                app_id: "steam:480".to_string(),
                display_name: "Spacewar".to_string(),
                canonical_executable: None,
                desktop_entry_id: None,
                steam_app_id: Some(480),
                launcher: Some("steam".to_string()),
                aliases: vec!["Space War".to_string()],
                icon_path: None,
            }],
            vec![AgentCatalogAppRecord {
                app_id: "steam:480".to_string(),
                display_name: "Spacewar Cloud".to_string(),
                aliases: Vec::new(),
                canonical_executable: None,
                desktop_entry_id: None,
                steam_app_id: Some(480),
                launcher: Some("steam".to_string()),
                icon_path: None,
                latest_bundle_id: Some("bundle-1".to_string()),
            }],
        );

        assert_eq!(entries.len(), 1);
        let item = &entries[0].item;
        assert!(item.installed);
        assert!(item.in_shared_storage);
        assert!(!item.restore_required);
        assert_eq!(item.launch_method, "steam");
        assert_eq!(item.latest_bundle_id.as_deref(), Some("bundle-1"));
        assert!(item.aliases.contains(&"Spacewar Cloud".to_string()));
    }

    #[test]
    fn parses_supported_launch_plans_and_rejects_unsafe_desktop_ids() {
        let steam = merge_launch_library(
            vec![AgentAppRecord {
                app_id: "steam:570".to_string(),
                display_name: "Dota 2".to_string(),
                canonical_executable: None,
                desktop_entry_id: None,
                steam_app_id: None,
                launcher: None,
                aliases: Vec::new(),
                icon_path: None,
            }],
            Vec::new(),
        );
        assert_eq!(launch_plan(&steam[0]), Some(LaunchPlan::Steam(570)));

        let unsafe_desktop = merge_launch_library(
            vec![AgentAppRecord {
                app_id: "unknown".to_string(),
                display_name: "Unsafe".to_string(),
                canonical_executable: None,
                desktop_entry_id: Some("bad;command".to_string()),
                steam_app_id: None,
                launcher: None,
                aliases: Vec::new(),
                icon_path: None,
            }],
            Vec::new(),
        );
        assert_eq!(launch_plan(&unsafe_desktop[0]), None);
    }
}
