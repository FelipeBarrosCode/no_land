//! Deploy and start noland-state-agent on the remote disposable instance.

use std::path::PathBuf;
use std::time::Duration;

use crate::errors::{AppError, AppResult};
use crate::services::remote_exec::RemoteExec;

const AGENT_SOCKET: &str = "/run/noland/state-agent.sock";

pub async fn ensure_state_agent(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
    if probe_agent(remote).await.is_ok() {
        return Ok(());
    }

    let local_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../state-agent");
    if !local_src.join("Cargo.toml").exists() {
        return Err(AppError::Provisioning(
            "state-agent workspace is missing next to src-tauri; cannot deploy the tracker.".into(),
        ));
    }

    let stamp = chrono::Utc::now().timestamp();
    let local_tar = std::env::temp_dir().join(format!("noland-state-agent-{stamp}.tar.gz"));
    let status = std::process::Command::new("tar")
        .args([
            "-czf",
            &local_tar.display().to_string(),
            "--exclude",
            "target",
            "--exclude",
            ".git",
            "-C",
            &local_src.display().to_string(),
            ".",
        ])
        .status()
        .map_err(|e| AppError::Command(format!("tar state-agent: {e}")))?;
    if !status.success() {
        return Err(AppError::Command(
            "failed to pack state-agent sources".into(),
        ));
    }

    let remote_tar = format!("/tmp/noland-state-agent-{stamp}.tar.gz");
    {
        let remote = remote.clone();
        let tar = local_tar.clone();
        let dest = remote_tar.clone();
        tokio::task::spawn_blocking(move || remote.scp(&tar, &dest, Duration::from_secs(180)))
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??;
    }

    let bootstrap = include_str!("../../../../state-agent/scripts/bootstrap-remote.sh");
    let encoded_script = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        bootstrap.as_bytes(),
    );
    let setup = format!(
        "sudo mkdir -p /opt/noland/state-agent && sudo tar -xzf {tar} -C /opt/noland/state-agent && printf %s {script} | base64 -d > /tmp/noland-bootstrap-agent.sh && sudo bash /tmp/noland-bootstrap-agent.sh /opt/noland/state-agent /usr/local/bin/noland-state-agent {user} && sudo chown -R {user}: /var/lib/noland /run/noland || true",
        tar = shell_escape(&remote_tar),
        script = shell_escape(&encoded_script),
        user = shell_escape(target_user),
    );
    let output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh_until_complete(&setup))
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??
    };
    let _ = std::fs::remove_file(&local_tar);
    if !output.stdout.contains("STATE_AGENT_READY") && output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed to start state-agent: {} {}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }

    // Give the socket a moment, then require a health reply.
    for _ in 0..10 {
        if probe_agent(remote).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    probe_agent(remote).await.map(|_| ())
}

pub async fn probe_agent(remote: &RemoteExec) -> AppResult<serde_json::Value> {
    call_agent_raw(remote, "GetHealth", serde_json::json!({})).await
}

pub async fn call_agent_raw(
    remote: &RemoteExec,
    method: &str,
    params: serde_json::Value,
) -> AppResult<serde_json::Value> {
    let request = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{request}\n").as_bytes(),
    );
    let cmd = format!(
        "python3 -c 'import socket,sys,base64; req=base64.b64decode(sys.argv[1]); s=socket.socket(socket.AF_UNIX); s.settimeout(300); s.connect(\"{sock}\"); s.sendall(req); s.shutdown(1); sys.stdout.buffer.write(b\"\".join(iter(lambda:s.recv(65536), b\"\")))' {req}",
        sock = AGENT_SOCKET,
        req = shell_escape(&encoded),
    );
    let output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(600)))
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??
    };
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "state-agent RPC {method} failed: {} {}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }
    let line = output
        .stdout
        .lines()
        .find(|l| l.trim().starts_with('{'))
        .ok_or_else(|| AppError::State("state-agent returned no JSON".into()))?;
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| AppError::State(format!("agent RPC parse: {e}: {line}")))?;
    if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
        return Err(AppError::Provisioning(format!(
            "state-agent {method}: {err}"
        )));
    }
    Ok(value.get("result").cloned().unwrap_or(value))
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
