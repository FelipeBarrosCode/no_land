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
    pub launcher: Option<String>,
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
    let plan = if let Some(plan) = launch_plan(entry) {
        Some(plan)
    } else if needs_steam_title_lookup(entry) {
        match resolve_steam_app_id_by_title(entry).await {
            Ok(Some(app_id)) => Some(LaunchPlan::Steam(app_id)),
            Ok(None) => {
                warn!(
                    title = %entry.item.display_name,
                    "Steam title lookup returned no exact game match"
                );
                None
            }
            Err(error) => {
                warn!(
                    title = %entry.item.display_name,
                    %error,
                    "Steam title lookup failed; continuing with executable fallback"
                );
                None
            }
        }
    } else {
        None
    };

    if let Some(plan) = plan {
        if let LaunchPlan::Steam(app_id) = &plan {
            ensure_remote_steam_appmanifest(remote, target_user, entry, *app_id).await?;
        }
        let output = run_launch_plan(remote, target_user, &plan).await?;
        if output.status_code == 0 {
            return Ok(());
        }

        let details = [output.stderr.trim(), output.stdout.trim()]
            .into_iter()
            .find(|value| !value.is_empty())
            .unwrap_or("remote launcher returned no details")
            .to_string();
        if matches!(plan, LaunchPlan::Steam(_)) {
            return Err(AppError::Command(format!(
                "Could not launch {} through Steam: {}",
                entry.item.display_name, details
            )));
        }
        if let Some(fallback_executable) =
            search_remote_executable(remote, target_user, entry).await?
        {
            let fallback_plan = LaunchPlan::Executable(fallback_executable);
            let fallback_output = run_launch_plan(remote, target_user, &fallback_plan).await?;
            if fallback_output.status_code == 0 {
                return Ok(());
            }
            let fallback_details = [fallback_output.stderr.trim(), fallback_output.stdout.trim()]
                .into_iter()
                .find(|value| !value.is_empty())
                .unwrap_or("fallback executable launcher returned no details");
            return Err(AppError::Command(format!(
                "Could not launch {} with its primary launcher ({details}) or fallback executable search ({fallback_details}).",
                entry.item.display_name,
            )));
        }

        return Err(AppError::Command(format!(
            "Could not launch {}: {}",
            entry.item.display_name, details
        )));
    }

    if let Some(fallback_executable) = search_remote_executable(remote, target_user, entry).await? {
        let fallback_output = run_launch_plan(
            remote,
            target_user,
            &LaunchPlan::Executable(fallback_executable),
        )
        .await?;
        if fallback_output.status_code == 0 {
            return Ok(());
        }
        let details = [fallback_output.stderr.trim(), fallback_output.stdout.trim()]
            .into_iter()
            .find(|value| !value.is_empty())
            .unwrap_or("fallback executable launcher returned no details");
        return Err(AppError::Command(format!(
            "Could not launch {} from its discovered executable: {}",
            entry.item.display_name, details
        )));
    }

    Err(AppError::InvalidInput(format!(
        "{} cannot be launched automatically because no Steam ID, desktop entry, or executable was discovered. Launch it from the streamed desktop instead.",
        entry.item.display_name
    )))
}

fn merge_launch_library(
    local: Vec<AgentAppRecord>,
    catalog: Vec<AgentCatalogAppRecord>,
) -> Vec<LaunchLibraryEntry> {
    let mut merged = BTreeMap::<String, LaunchLibraryEntry>::new();
    for app in local {
        let display_name = non_empty_or(&app.display_name, &app.app_id);
        let mut entry = LaunchLibraryEntry {
            item: LaunchLibraryItem {
                app_id: app.app_id,
                display_name: display_name.clone(),
                aliases: unique_aliases(app.aliases, &display_name),
                installed: true,
                in_shared_storage: false,
                latest_bundle_id: None,
                source_labels: vec!["Installed".to_string()],
                launchable: false,
                launch_method: String::new(),
                restore_required: false,
                artwork_key: display_name.clone(),
            },
            canonical_executable: app.canonical_executable,
            desktop_entry_id: app.desktop_entry_id,
            steam_app_id: app.steam_app_id,
            launcher: app.launcher,
        };
        refresh_launchability(&mut entry);
        merged.insert(entry.item.app_id.clone(), entry);
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
            if existing.launcher.is_none() {
                existing.launcher = app.launcher;
            }
            refresh_launchability(existing);
            continue;
        }

        let display_name = non_empty_or(&app.display_name, &app.app_id);
        let restore_required = app.latest_bundle_id.is_some();
        let mut entry = LaunchLibraryEntry {
            item: LaunchLibraryItem {
                app_id: app.app_id,
                display_name: display_name.clone(),
                aliases: unique_aliases(app.aliases, &display_name),
                installed: false,
                in_shared_storage: true,
                latest_bundle_id: app.latest_bundle_id,
                source_labels: vec!["Shared storage".to_string()],
                launchable: false,
                launch_method: String::new(),
                restore_required,
                artwork_key: display_name.clone(),
            },
            canonical_executable: app.canonical_executable,
            desktop_entry_id: app.desktop_entry_id,
            steam_app_id: app.steam_app_id,
            launcher: app.launcher,
        };
        refresh_launchability(&mut entry);
        entry.item.launchable = restore_required && entry.item.launchable;
        merged.insert(entry.item.app_id.clone(), entry);
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

fn refresh_launchability(entry: &mut LaunchLibraryEntry) {
    repair_entry_launch_metadata(entry);
    let method = launch_method_for_entry(entry);
    entry.item.launchable = if entry.item.installed {
        method.is_some()
    } else {
        entry.item.restore_required && method.is_some()
    };
    entry.item.launch_method = method.unwrap_or_default();
}

fn repair_entry_launch_metadata(entry: &mut LaunchLibraryEntry) {
    entry.canonical_executable = normalized_non_empty(entry.canonical_executable.take());
    entry.desktop_entry_id = normalized_non_empty(entry.desktop_entry_id.take());
    entry.launcher = normalized_launcher(entry.launcher.take());
    entry.steam_app_id = entry
        .steam_app_id
        .or_else(|| parse_steam_app_id(&entry.item.app_id, entry.launcher.as_deref()));
    if entry.launcher.is_none() && steamish_entry(entry) {
        entry.launcher = Some("steam".to_string());
    }
}

pub(crate) async fn repair_entry_before_launch(entry: &mut LaunchLibraryEntry) -> AppResult<()> {
    refresh_launchability(entry);
    if entry.item.launchable || !needs_steam_title_lookup(entry) {
        return Ok(());
    }
    if let Some(app_id) = resolve_steam_app_id_by_title(entry).await? {
        entry.steam_app_id = Some(app_id);
        if entry.launcher.is_none() {
            entry.launcher = Some("steam".to_string());
        }
        refresh_launchability(entry);
    }
    Ok(())
}

fn launch_plan(entry: &LaunchLibraryEntry) -> Option<LaunchPlan> {
    if let Some(steam_id) = entry
        .steam_app_id
        .or_else(|| parse_steam_app_id(&entry.item.app_id, entry.launcher.as_deref()))
    {
        return Some(LaunchPlan::Steam(steam_id));
    }
    if let Some(desktop_id) = entry
        .desktop_entry_id
        .as_deref()
        .filter(|value| valid_desktop_id(value))
        .filter(|value| should_use_desktop_entry(entry, value))
        .map(ToOwned::to_owned)
        .or_else(|| {
            parse_desktop_id(&entry.item.app_id)
                .filter(|value| should_use_desktop_entry(entry, value))
        })
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

fn launch_method_for_entry(entry: &LaunchLibraryEntry) -> Option<String> {
    launch_plan(entry)
        .map(|plan| {
            match plan {
                LaunchPlan::Steam(_) => "steam",
                LaunchPlan::Desktop(_) => "desktop",
                LaunchPlan::Executable(_) => "executable",
            }
            .to_string()
        })
        .or_else(|| needs_steam_title_lookup(entry).then(|| "steam_lookup".to_string()))
}

async fn run_launch_plan(
    remote: &RemoteExec,
    target_user: &str,
    plan: &LaunchPlan,
) -> AppResult<crate::services::remote_exec::ExecOutput> {
    let script = launch_script(plan)?;
    run_remote_user_script(
        remote,
        target_user,
        &script,
        Duration::from_secs(30),
        "software launch task failed",
    )
    .await
}

async fn run_remote_user_script(
    remote: &RemoteExec,
    target_user: &str,
    script: &str,
    timeout: Duration,
    task_label: &str,
) -> AppResult<crate::services::remote_exec::ExecOutput> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    let remote_command = format!(
        "printf %s {payload} | base64 -d | sudo -i -u {user} sh",
        payload = shell_quote(&encoded),
        user = shell_quote(target_user),
    );
    let remote = remote.clone();
    tokio::task::spawn_blocking(move || remote.ssh(&remote_command, timeout))
        .await
        .map_err(|error| AppError::Command(format!("{task_label}: {error}")))?
}

async fn search_remote_executable(
    remote: &RemoteExec,
    target_user: &str,
    entry: &LaunchLibraryEntry,
) -> AppResult<Option<String>> {
    let candidates = executable_search_terms(entry);
    if candidates.is_empty() {
        return Ok(None);
    }

    let payload = serde_json::to_string(&candidates).map_err(|error| {
        AppError::Serialization(format!("search term serialization failed: {error}"))
    })?;
    let script = format!(
        r#"python3 - <<'PY'
import json
import os
from pathlib import Path

terms = json.loads({terms})
roots = [
    Path('/home/{user}'),
    Path('/home/{user}/Games'),
    Path('/home/{user}/.local/share'),
    Path('/home/{user}/.steam'),
    Path('/mnt'),
    Path('/media'),
    Path('/opt'),
    Path('/usr/local'),
    Path('/usr/games'),
    Path('/games'),
    Path('/srv'),
]
allowed_suffixes = {{'.appimage', '.AppImage', '.sh', '.run', '.exe', '.jar', '.bin', '.x86_64'}}
results = []
seen = set()
for root in roots:
    if not root.exists():
        continue
    for current, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in {{'.cache', '.local/share/Trash', 'node_modules', '.git', '__pycache__', 'tmp', 'Temp'}}]
        for name in files:
            path = Path(current) / name
            if path.is_symlink() and not path.exists():
                continue
            suffix = path.suffix
            executable_like = os.access(path, os.X_OK) or suffix in allowed_suffixes
            if not executable_like:
                continue
            lowered = name.lower()
            stem = Path(name).stem.lower()
            compact = ''.join(ch for ch in stem if ch.isalnum())
            score = None
            for idx, term in enumerate(terms):
                t = term.lower()
                t_compact = ''.join(ch for ch in t if ch.isalnum())
                if lowered == t:
                    score = (0, idx, len(name))
                    break
                if stem == t:
                    score = (1, idx, len(name))
                    break
                if lowered.startswith(t + '.') or lowered.startswith(t + '_') or lowered.startswith(t + '-'):
                    score = (2, idx, len(name))
                    break
                if t and t in lowered:
                    score = (3, idx, len(name))
                    break
                if t_compact and t_compact == compact:
                    score = (4, idx, len(name))
                    break
                if t_compact and t_compact in compact:
                    score = (5, idx, len(name))
                    break
            if score is None:
                continue
            normalized = str(path)
            if normalized in seen:
                continue
            seen.add(normalized)
            results.append((score, normalized))
results.sort(key=lambda item: item[0])
print(results[0][1] if results else '')
PY"#,
        terms = shell_quote(&payload),
        user = target_user.replace('"', ""),
    );
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || remote.ssh(&script, Duration::from_secs(90)))
        .await
        .map_err(|error| {
            AppError::Command(format!("remote executable search failed: {error}"))
        })??;
    if output.status_code != 0 {
        return Err(AppError::Command(format!(
            "remote executable search failed: {}",
            [output.stderr.trim(), output.stdout.trim()]
                .into_iter()
                .find(|value| !value.is_empty())
                .unwrap_or("unknown search failure")
        )));
    }
    let found = output.stdout.trim();
    if found.is_empty() {
        Ok(None)
    } else {
        Ok(Some(found.to_string()))
    }
}

fn executable_search_terms(entry: &LaunchLibraryEntry) -> Vec<String> {
    let mut terms = Vec::new();
    push_unique_term(&mut terms, &entry.item.display_name);
    for alias in &entry.item.aliases {
        push_unique_term(&mut terms, alias);
    }
    if let Some(path) = entry
        .canonical_executable
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(name) = std::path::Path::new(path)
            .file_name()
            .and_then(|value| value.to_str())
        {
            push_unique_term(&mut terms, name);
        }
        if let Some(stem) = std::path::Path::new(path)
            .file_stem()
            .and_then(|value| value.to_str())
        {
            push_unique_term(&mut terms, stem);
        }
    }
    if let Some(app_tail) = entry.item.app_id.rsplit(':').next() {
        if !app_tail.chars().all(|ch| ch.is_ascii_digit()) {
            push_unique_term(&mut terms, app_tail);
        }
    }
    if let Some(desktop_id) = entry.desktop_entry_id.as_deref() {
        push_unique_term(&mut terms, desktop_id);
        if let Some(last) = desktop_id.rsplit('.').next() {
            push_unique_term(&mut terms, last);
        }
    }
    if let Some(launcher) = entry.launcher.as_deref() {
        push_unique_term(&mut terms, launcher);
    }
    terms
}

fn push_unique_term(terms: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    let candidates = [
        trimmed.to_string(),
        sanitize_executable_search_term(trimmed),
        punctuated_executable_search_term(trimmed),
        compact_executable_search_term(trimmed),
    ];
    for candidate in candidates {
        let normalized = candidate.trim();
        if normalized.len() < 2 || ignored_executable_search_term(normalized) {
            continue;
        }
        if !terms
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(normalized))
        {
            terms.push(normalized.to_string());
        }
    }
}

fn sanitize_executable_search_term(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn punctuated_executable_search_term(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_executable_search_term(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn ignored_executable_search_term(value: &str) -> bool {
    let compact = compact_executable_search_term(value);
    compact.len() < 2
        || compact.chars().all(|ch| ch.is_ascii_digit())
        || matches!(
            compact.to_ascii_lowercase().as_str(),
            "steam" | "heroic" | "lutris" | "wine" | "proton" | "native" | "desktop"
        )
        || looks_like_generic_launcher_descriptor(value)
}

fn should_use_desktop_entry(entry: &LaunchLibraryEntry, desktop_id: &str) -> bool {
    !looks_like_generic_launcher_descriptor(desktop_id)
        || entry_looks_like_same_launcher(entry, desktop_id)
}

fn entry_looks_like_same_launcher(entry: &LaunchLibraryEntry, desktop_id: &str) -> bool {
    let launcher_hints = launcher_descriptor_hints(desktop_id);
    if launcher_hints.is_empty() {
        return false;
    }
    [Some(entry.item.display_name.as_str())]
        .into_iter()
        .chain(entry.item.aliases.iter().map(|value| Some(value.as_str())))
        .flatten()
        .any(|value| {
            let normalized = sanitize_executable_search_term(value).to_ascii_lowercase();
            launcher_hints.iter().any(|hint| normalized.contains(hint))
        })
}

fn looks_like_generic_launcher_descriptor(value: &str) -> bool {
    let tokens = value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }
    let generic_tokens = [
        "net", "com", "org", "io", "app", "desktop", "launcher", "client", "games", "game",
        "steam", "lutris", "heroic", "wine", "proton", "bottles", "native", "flatpak", "snap",
        "appimage", "hgl",
    ];
    let has_launcher_name = tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "steam" | "lutris" | "heroic" | "wine" | "proton" | "bottles"
        )
    });
    has_launcher_name
        && tokens
            .iter()
            .all(|token| generic_tokens.contains(&token.as_str()))
}

fn launcher_descriptor_hints(value: &str) -> Vec<&'static str> {
    let normalized = sanitize_executable_search_term(value).to_ascii_lowercase();
    ["steam", "lutris", "heroic", "wine", "proton", "bottles"]
        .into_iter()
        .filter(|hint| normalized.contains(hint))
        .collect()
}

const GUI_SESSION_ENVIRONMENT_PRELUDE: &str = r#"export DISPLAY="${DISPLAY:-:0}"
if [ -z "${XAUTHORITY:-}" ]; then
  for candidate in /etc/X11/.Xauthority-noland "$HOME/.Xauthority"; do
    if [ -s "$candidate" ] && [ -r "$candidate" ]; then export XAUTHORITY="$candidate"; break; fi
  done
fi
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
"#;

const STEAM_LAUNCH_TEMPLATE: &str = r#"export PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin:/usr/local/games:/usr/games:/snap/bin:${PATH:-}"
if command -v steam >/dev/null 2>&1; then
  steam -applaunch __STEAM_APP_ID__
elif command -v flatpak >/dev/null 2>&1 && flatpak info --user com.valvesoftware.Steam >/dev/null 2>&1; then
  flatpak run --user com.valvesoftware.Steam -applaunch __STEAM_APP_ID__
elif command -v flatpak >/dev/null 2>&1 && flatpak info --system com.valvesoftware.Steam >/dev/null 2>&1; then
  flatpak run --system com.valvesoftware.Steam -applaunch __STEAM_APP_ID__
else
  echo 'Steam is not installed on this instance.' >&2
  exit 127
fi
"#;

fn launch_script(plan: &LaunchPlan) -> AppResult<String> {
    let launch = match plan {
        LaunchPlan::Steam(app_id) => {
            STEAM_LAUNCH_TEMPLATE.replace("__STEAM_APP_ID__", &app_id.to_string())
        }
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
            "executable={exe}\ncase \"$executable\" in\n  */*)\n    if [ ! -f \"$executable\" ]; then printf '%s\\n' \"The discovered executable is missing: $executable\" >&2; exit 126; fi\n    ;;\n  *)\n    executable=$(command -v \"$executable\") || {{ printf '%s\\n' 'The discovered executable command is unavailable.' >&2; exit 127; }}\n    ;;\nesac\ncase \"$executable\" in\n  *.[jJ][aA][rR])\n    command -v java >/dev/null 2>&1 || {{ printf '%s\\n' 'Java is required to launch this application.' >&2; exit 127; }}\n    nohup java -jar \"$executable\" >/tmp/noland-launch-executable.log 2>&1 &\n    ;;\n  *.[eE][xX][eE])\n    command -v wine >/dev/null 2>&1 || {{ printf '%s\\n' 'Wine is required to launch this Windows application.' >&2; exit 127; }}\n    nohup wine \"$executable\" >/tmp/noland-launch-executable.log 2>&1 &\n    ;;\n  *)\n    if [ ! -x \"$executable\" ]; then printf '%s\\n' \"The discovered executable is not executable: $executable\" >&2; exit 126; fi\n    nohup \"$executable\" >/tmp/noland-launch-executable.log 2>&1 &\n    ;;\nesac",
            exe = shell_quote(executable),
        ),
    };
    if matches!(plan, LaunchPlan::Steam(_)) {
        return Ok(launch);
    }

    let mut script =
        String::with_capacity(GUI_SESSION_ENVIRONMENT_PRELUDE.len() + launch.len() + 1);
    script.push_str(GUI_SESSION_ENVIRONMENT_PRELUDE);
    script.push_str(&launch);
    script.push('\n');
    Ok(script)
}

async fn ensure_remote_steam_appmanifest(
    remote: &RemoteExec,
    target_user: &str,
    entry: &LaunchLibraryEntry,
    app_id: u32,
) -> AppResult<()> {
    let payload = serde_json::json!({
        "app_id": app_id,
        "name": non_empty_or(&entry.item.display_name, &format!("Steam {app_id}")),
        "installdir": steam_install_dir_hint(entry, app_id),
    });
    let payload = serde_json::to_string(&payload).map_err(|error| {
        AppError::Serialization(format!(
            "steam manifest payload serialization failed: {error}"
        ))
    })?;
    let script = format!(
        r#"python3 - <<'PY'
import json
import pathlib
import time

payload = json.loads({payload})
app_id = int(payload["app_id"])
name = payload["name"]
installdir = payload["installdir"]
steamapps_dirs = [
    pathlib.Path.home() / ".steam/steam/steamapps",
    pathlib.Path.home() / ".steam/debian-installation/steamapps",
    pathlib.Path.home() / ".local/share/Steam/steamapps",
    pathlib.Path.home() / ".var/app/com.valvesoftware.Steam/data/Steam/steamapps",
]
for steamapps in steamapps_dirs:
    manifest = steamapps / f"appmanifest_{{app_id}}.acf"
    if manifest.is_file() and manifest.stat().st_size > 0:
        raise SystemExit(0)

target = next((path for path in steamapps_dirs if path.is_dir()), steamapps_dirs[2])
target.mkdir(parents=True, exist_ok=True)
(target / "common").mkdir(parents=True, exist_ok=True)
manifest = target / f"appmanifest_{{app_id}}.acf"
manifest.write_text(
    '"AppState"\n' +
    '{{\n' +
    f'\t"appid"\t\t"{{app_id}}"\n' +
    f'\t"name"\t\t"{{name}}"\n' +
    '\t"StateFlags"\t\t"4"\n' +
    f'\t"installdir"\t\t"{{installdir}}"\n' +
    f'\t"LastUpdated"\t\t"{{int(time.time())}}"\n' +
    '}}\n',
    encoding='utf-8'
)
PY"#,
        payload = shell_quote(&payload),
    );
    let output = run_remote_user_script(
        remote,
        target_user,
        &script,
        Duration::from_secs(20),
        "steam manifest repair failed",
    )
    .await?;
    if output.status_code == 0 {
        return Ok(());
    }
    let details = [output.stderr.trim(), output.stdout.trim()]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or("remote Steam manifest repair returned no details");
    Err(AppError::Command(format!(
        "Could not prepare Steam metadata for {}: {}",
        entry.item.display_name, details
    )))
}

fn steam_install_dir_hint(entry: &LaunchLibraryEntry, app_id: u32) -> String {
    let candidate = non_empty_or(&entry.item.display_name, &format!("Steam {app_id}"));
    let sanitized = candidate
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = sanitized.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        format!("Steam-{app_id}")
    } else {
        trimmed.to_string()
    }
}

fn normalized_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn normalized_launcher(value: Option<String>) -> Option<String> {
    normalized_non_empty(value).map(|value| value.to_ascii_lowercase())
}

fn steamish_entry(entry: &LaunchLibraryEntry) -> bool {
    entry.steam_app_id.is_some() || entry.item.app_id.trim().starts_with("steam:")
}

fn parse_steam_app_id(app_id: &str, launcher: Option<&str>) -> Option<u32> {
    let app_id = app_id.trim();
    if let Some(value) = app_id.strip_prefix("steam:") {
        return value.parse().ok();
    }

    launcher
        .filter(|value| value.trim().eq_ignore_ascii_case("steam"))
        .and_then(|_| app_id.parse().ok())
}

fn needs_steam_title_lookup(entry: &LaunchLibraryEntry) -> bool {
    entry.steam_app_id.is_none()
        && parse_steam_app_id(&entry.item.app_id, entry.launcher.as_deref()).is_none()
        && (entry.item.app_id.trim().starts_with("steam:")
            || entry
                .launcher
                .as_deref()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("steam")))
        && !entry.item.display_name.trim().is_empty()
}

#[derive(Debug, serde::Deserialize)]
struct SteamStoreSearchResponse {
    #[serde(default)]
    items: Vec<SteamStoreSearchItem>,
}

#[derive(Debug, serde::Deserialize)]
struct SteamStoreSearchItem {
    id: u32,
    name: String,
    #[serde(rename = "type")]
    item_type: String,
}

async fn resolve_steam_app_id_by_title(entry: &LaunchLibraryEntry) -> AppResult<Option<u32>> {
    let mut titles = Vec::new();
    push_unique_steam_title(&mut titles, &entry.item.display_name);
    for alias in &entry.item.aliases {
        push_unique_steam_title(&mut titles, alias);
    }
    if titles.is_empty() {
        return Ok(None);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("Noland-Connect/0.1 Steam title resolver")
        .build()?;

    for title in &titles {
        let response = client
            .get("https://store.steampowered.com/api/storesearch/")
            .query(&[("term", title.as_str()), ("l", "english"), ("cc", "US")])
            .send()
            .await?
            .error_for_status()?
            .json::<SteamStoreSearchResponse>()
            .await?;
        if let Some(app_id) = select_steam_app_id(&response, &titles) {
            return Ok(Some(app_id));
        }
    }

    Ok(None)
}

fn select_steam_app_id(response: &SteamStoreSearchResponse, titles: &[String]) -> Option<u32> {
    response
        .items
        .iter()
        .filter(|item| item.item_type.eq_ignore_ascii_case("app"))
        .find(|item| {
            let candidate = normalize_steam_title(&item.name);
            !candidate.is_empty()
                && titles
                    .iter()
                    .any(|title| normalize_steam_title(title) == candidate)
        })
        .map(|item| item.id)
}

fn push_unique_steam_title(titles: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty()
        || titles
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(value))
    {
        return;
    }
    titles.push(value.to_string());
}

fn normalize_steam_title(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
        executable_search_terms, launch_plan, launch_script, merge_launch_library,
        repair_entry_launch_metadata, select_steam_app_id, steam_install_dir_hint, AgentAppRecord,
        AgentCatalogAppRecord, LaunchLibraryEntry, LaunchPlan, SteamStoreSearchItem,
        SteamStoreSearchResponse,
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

    #[test]
    fn executable_search_terms_prefer_real_title_tokens_over_generic_launcher_tokens() {
        let entry = LaunchLibraryEntry {
            item: crate::models::launch_library::LaunchLibraryItem {
                app_id: "steam:3241660".to_string(),
                display_name: "R.E.P.O.".to_string(),
                aliases: vec!["REPO".to_string()],
                installed: true,
                in_shared_storage: false,
                latest_bundle_id: None,
                source_labels: vec![],
                launchable: true,
                launch_method: "search".to_string(),
                restore_required: false,
                artwork_key: "R.E.P.O.".to_string(),
            },
            canonical_executable: None,
            desktop_entry_id: None,
            steam_app_id: Some(3241660),
            launcher: Some("steam".to_string()),
        };

        let terms = executable_search_terms(&entry);
        assert!(!terms.iter().any(|value| value == "3241660"));
        assert!(!terms
            .iter()
            .any(|value| value.eq_ignore_ascii_case("steam")));
        assert!(terms
            .iter()
            .any(|value| value.eq_ignore_ascii_case("R.E.P.O.")));
        assert!(terms
            .iter()
            .any(|value| value.eq_ignore_ascii_case("R E P O")));
        assert!(terms.iter().any(|value| value.eq_ignore_ascii_case("REPO")));
    }

    #[test]
    fn executable_search_terms_prioritize_titles_over_generic_launcher_metadata() {
        let entry = LaunchLibraryEntry {
            item: crate::models::launch_library::LaunchLibraryItem {
                app_id: "lutris:native".to_string(),
                display_name: "Cyberpunk 2077".to_string(),
                aliases: vec!["Cyberpunk".to_string()],
                installed: true,
                in_shared_storage: false,
                latest_bundle_id: None,
                source_labels: vec![],
                launchable: true,
                launch_method: "search".to_string(),
                restore_required: false,
                artwork_key: "Cyberpunk 2077".to_string(),
            },
            canonical_executable: None,
            desktop_entry_id: Some("net.lutris.Lutris".to_string()),
            steam_app_id: None,
            launcher: Some("native".to_string()),
        };

        let terms = executable_search_terms(&entry);
        assert_eq!(terms.first().map(String::as_str), Some("Cyberpunk 2077"));
        assert!(terms
            .iter()
            .any(|value| value.eq_ignore_ascii_case("Cyberpunk")));
        assert!(!terms
            .iter()
            .any(|value| value.eq_ignore_ascii_case("native")));
        assert!(!terms
            .iter()
            .any(|value| value.eq_ignore_ascii_case("net.lutris.Lutris")));
    }

    #[test]
    fn recovers_numeric_steam_id_only_when_launcher_metadata_confirms_steam() {
        let records = |launcher: Option<&str>| {
            merge_launch_library(
                vec![AgentAppRecord {
                    app_id: "3241660".to_string(),
                    display_name: "R.E.P.O.".to_string(),
                    canonical_executable: None,
                    desktop_entry_id: None,
                    steam_app_id: None,
                    launcher: launcher.map(ToOwned::to_owned),
                    aliases: Vec::new(),
                    icon_path: None,
                }],
                Vec::new(),
            )
        };

        let steam = records(Some(" Steam "));
        assert_eq!(launch_plan(&steam[0]), Some(LaunchPlan::Steam(3241660)));
        assert!(steam[0].item.launchable);
        assert_eq!(steam[0].item.launch_method, "steam");

        let unconfirmed = records(None);
        assert_eq!(launch_plan(&unconfirmed[0]), None);
        assert!(!unconfirmed[0].item.launchable);
    }

    #[test]
    fn launcher_only_metadata_does_not_mark_an_entry_launchable() {
        let entries = merge_launch_library(
            vec![AgentAppRecord {
                app_id: "heroic:unknown-game".to_string(),
                display_name: "Unknown Game".to_string(),
                canonical_executable: None,
                desktop_entry_id: None,
                steam_app_id: None,
                launcher: Some("heroic".to_string()),
                aliases: Vec::new(),
                icon_path: None,
            }],
            Vec::new(),
        );

        assert_eq!(launch_plan(&entries[0]), None);
        assert!(!entries[0].item.launchable);
        assert!(entries[0].item.launch_method.is_empty());
    }

    #[test]
    fn steam_launcher_without_an_id_is_launchable_via_title_lookup() {
        let entries = merge_launch_library(
            vec![AgentAppRecord {
                app_id: "steam:unknown".to_string(),
                display_name: "R.E.P.O.".to_string(),
                canonical_executable: None,
                desktop_entry_id: None,
                steam_app_id: None,
                launcher: Some("steam".to_string()),
                aliases: vec!["REPO".to_string()],
                icon_path: None,
            }],
            Vec::new(),
        );

        assert_eq!(launch_plan(&entries[0]), None);
        assert!(entries[0].item.launchable);
        assert_eq!(entries[0].item.launch_method, "steam_lookup");
    }

    #[test]
    fn steam_title_lookup_selects_only_exact_app_or_alias_matches() {
        let response = SteamStoreSearchResponse {
            items: vec![
                SteamStoreSearchItem {
                    id: 1,
                    name: "Repository Manager".to_string(),
                    item_type: "app".to_string(),
                },
                SteamStoreSearchItem {
                    id: 2,
                    name: "R.E.P.O. Soundtrack".to_string(),
                    item_type: "app".to_string(),
                },
                SteamStoreSearchItem {
                    id: 3,
                    name: "R.E.P.O.".to_string(),
                    item_type: "bundle".to_string(),
                },
                SteamStoreSearchItem {
                    id: 3_241_660,
                    name: "R.E.P.O.".to_string(),
                    item_type: "app".to_string(),
                },
            ],
        };

        assert_eq!(
            select_steam_app_id(&response, &["REPO".to_string()]),
            Some(3_241_660)
        );
        assert_eq!(
            select_steam_app_id(&response, &["Different Game".to_string()]),
            None
        );
    }

    #[test]
    fn steam_launch_script_sends_the_app_id_without_starting_a_display() {
        let script = launch_script(&LaunchPlan::Steam(3_241_660)).unwrap();

        assert!(script.contains("/usr/games:/snap/bin:${PATH:-}"));
        assert!(script.contains("steam -applaunch 3241660"));
        assert!(script.contains("flatpak run --user com.valvesoftware.Steam -applaunch 3241660"));
        assert!(!script.contains("DISPLAY"));
        assert!(!script.contains("DBUS_SESSION_BUS_ADDRESS"));
        assert!(!script.contains("xrandr"));
        assert!(!script.contains("python3"));
    }

    #[test]
    fn executable_launch_script_treats_the_executable_as_a_literal_value() {
        let executable = "/opt/Games/O'Brien; $(touch /tmp/unsafe)/game";
        let script = launch_script(&LaunchPlan::Executable(executable.to_string())).unwrap();

        assert!(script.contains("executable='/opt/Games/O'\"'\"'Brien; $(touch /tmp/unsafe)/game'"));
        assert!(script.contains("nohup \"$executable\""));
        assert!(!script.contains(&format!("nohup {executable}")));
    }

    #[test]
    fn executable_launch_script_uses_portable_app_runtimes() {
        let jar = launch_script(&LaunchPlan::Executable("/apps/example.jar".to_string())).unwrap();
        assert!(jar.contains("nohup java -jar \"$executable\""));
        assert!(jar.contains("Java is required to launch this application."));

        let exe = launch_script(&LaunchPlan::Executable("/apps/example.exe".to_string())).unwrap();
        assert!(exe.contains("nohup wine \"$executable\""));
        assert!(exe.contains("Wine is required to launch this Windows application."));
    }

    #[test]
    fn repair_entry_metadata_infers_missing_steam_fields() {
        let mut entry = LaunchLibraryEntry {
            item: crate::models::launch_library::LaunchLibraryItem {
                app_id: "steam:3241660".to_string(),
                display_name: "R.E.P.O.".to_string(),
                aliases: vec![],
                installed: true,
                in_shared_storage: false,
                latest_bundle_id: None,
                source_labels: vec![],
                launchable: false,
                launch_method: String::new(),
                restore_required: false,
                artwork_key: "R.E.P.O.".to_string(),
            },
            canonical_executable: None,
            desktop_entry_id: Some("  ".to_string()),
            steam_app_id: None,
            launcher: Some("  ".to_string()),
        };

        repair_entry_launch_metadata(&mut entry);

        assert_eq!(entry.steam_app_id, Some(3_241_660));
        assert_eq!(entry.launcher.as_deref(), Some("steam"));
        assert_eq!(entry.desktop_entry_id, None);
        assert_eq!(launch_plan(&entry), Some(LaunchPlan::Steam(3_241_660)));
    }

    #[test]
    fn steam_install_dir_hint_sanitizes_hostile_titles() {
        let entry = LaunchLibraryEntry {
            item: crate::models::launch_library::LaunchLibraryItem {
                app_id: "steam:42".to_string(),
                display_name: "O'Brien: The / Test".to_string(),
                aliases: vec![],
                installed: true,
                in_shared_storage: false,
                latest_bundle_id: None,
                source_labels: vec![],
                launchable: true,
                launch_method: "steam".to_string(),
                restore_required: false,
                artwork_key: "O'Brien: The / Test".to_string(),
            },
            canonical_executable: None,
            desktop_entry_id: None,
            steam_app_id: Some(42),
            launcher: Some("steam".to_string()),
        };

        assert_eq!(steam_install_dir_hint(&entry, 42), "O_Brien_ The _ Test");
    }

    #[test]
    fn launch_plan_skips_generic_launcher_desktop_entries_for_non_launcher_titles() {
        let entry = LaunchLibraryEntry {
            item: crate::models::launch_library::LaunchLibraryItem {
                app_id: "lutris:native".to_string(),
                display_name: "Cyberpunk 2077".to_string(),
                aliases: vec!["Cyberpunk".to_string()],
                installed: true,
                in_shared_storage: false,
                latest_bundle_id: None,
                source_labels: vec![],
                launchable: true,
                launch_method: "search".to_string(),
                restore_required: false,
                artwork_key: "Cyberpunk 2077".to_string(),
            },
            canonical_executable: None,
            desktop_entry_id: Some("net.lutris.Lutris".to_string()),
            steam_app_id: None,
            launcher: Some("lutris".to_string()),
        };

        assert_eq!(launch_plan(&entry), None);
    }
}
