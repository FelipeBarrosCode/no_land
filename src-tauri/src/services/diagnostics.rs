use std::{fs, path::PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{
    errors::{AppError, AppResult},
    services::{app_context::AppContext, health_check::SystemHealthReport},
    utils::logging::{log_file_path, recent_log_excerpt},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReportResponse {
    pub path: String,
    pub summary: String,
    pub report_markdown: String,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn report_dir(app: &AppHandle) -> AppResult<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| AppError::Io(format!("resolve app data dir: {error}")))?;
    let dir = app_data.join("diagnostic-reports");
    fs::create_dir_all(&dir)
        .map_err(|error| AppError::Io(format!("create diagnostics dir: {error}")))?;
    Ok(dir)
}

fn redact_sensitive(value: &str) -> String {
    let mut redacted = Vec::new();
    for line in value.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("privatekey")
            || lower.contains("private_key")
            || lower.contains("password")
            || lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("authorization")
            || lower.contains("bearer ")
            || lower.contains("client_secret")
        {
            redacted.push("[redacted sensitive line]".to_string());
        } else {
            redacted.push(line.to_string());
        }
    }
    redacted.join("\n")
}

pub async fn write_diagnostic_report(
    app: &AppHandle,
    context: &AppContext,
    reason: Option<String>,
    frontend_error: Option<String>,
    health: Option<SystemHealthReport>,
) -> AppResult<DiagnosticReportResponse> {
    let timestamp = now_unix();
    let path = report_dir(app)?.join(format!("noland-diagnostics-{timestamp}.md"));
    let state = context.load_state().await;
    let logs = context.provisioning_logs.read().await.clone();
    let health = match health {
        Some(report) => report,
        None => crate::services::health_check::run_system_health_report(app, context).await,
    };
    let log_excerpt =
        recent_log_excerpt(400).unwrap_or_else(|error| format!("Could not read app log: {error}"));
    let log_path = log_file_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable".to_string());

    let mut body = String::new();
    body.push_str("# Noland Connect Diagnostic Report\n\n");
    body.push_str(&format!("Generated: `{timestamp}`\n\n"));
    body.push_str(&format!(
        "Reason: `{}`\n\n",
        reason.unwrap_or_else(|| "manual".to_string())
    ));
    if let Some(error) = frontend_error {
        body.push_str("## Frontend Error\n\n```text\n");
        body.push_str(&redact_sensitive(&error));
        body.push_str("\n```\n\n");
    }

    body.push_str("## Health Summary\n\n");
    body.push_str(&format!(
        "- Overall: `{}`\n",
        if health.ok { "ok" } else { "failed" }
    ));
    body.push_str(&format!("- Summary: {}\n", health.summary));
    body.push_str(&format!("- OS: {} / {}\n\n", health.os, health.arch));
    for probe in &health.probes {
        body.push_str(&format!(
            "- `{:?}` **{}**: {}\n",
            probe.status, probe.label, probe.summary
        ));
        if let Some(details) = &probe.details {
            body.push_str(&format!("  - Details: `{}`\n", redact_sensitive(details)));
        }
        if let Some(hint) = &probe.fix_hint {
            body.push_str(&format!("  - Fix: {}\n", hint));
        }
    }

    body.push_str("\n## App State Snapshot\n\n");
    body.push_str(&format!("- State schema version: `{}`\n", state.version));
    body.push_str(&format!(
        "- Onboarding completed: `{}`\n",
        state.onboarding_completed
    ));
    body.push_str(&format!(
        "- Orchestration state: `{:?}`\n",
        state.orchestration_state
    ));
    body.push_str(&format!(
        "- Current instance id: `{:?}`\n",
        state.instance.instance_id
    ));
    body.push_str(&format!(
        "- Post-WireGuard stage: `{:?}`\n",
        state.post_wireguard_setup.stage
    ));
    body.push_str(&format!(
        "- WireGuard config path: `{}`\n",
        state.wireguard.config_path
    ));
    body.push_str(&format!(
        "- Shared storage profiles: `{}`\n",
        state.shared_storage_profiles.len()
    ));
    body.push_str(&format!(
        "- Provisioned servers: `{}`\n\n",
        state.provisioned_servers.len()
    ));

    body.push_str("## Recent Provisioning Events\n\n");
    if logs.is_empty() {
        body.push_str("No provisioning events recorded.\n\n");
    } else {
        for event in logs.iter().take(80) {
            body.push_str(&format!(
                "- `{:?}` error=`{}` message={} details={}\n",
                event.state,
                event.is_error,
                event.message,
                event.details.as_deref().unwrap_or("")
            ));
        }
        body.push('\n');
    }

    body.push_str("## App Log Excerpt\n\n");
    body.push_str(&format!("Path: `{log_path}`\n\n```text\n"));
    body.push_str(&redact_sensitive(&log_excerpt));
    body.push_str("\n```\n");

    fs::write(&path, &body)
        .map_err(|error| AppError::Io(format!("write diagnostic report: {error}")))?;
    Ok(DiagnosticReportResponse {
        path: path.display().to_string(),
        summary: health.summary,
        report_markdown: body,
    })
}
