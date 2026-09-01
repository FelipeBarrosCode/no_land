//! Deploy and start noland-state-agent on the remote disposable instance.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use crate::errors::{AppError, AppResult};
use crate::services::remote_exec::RemoteExec;

const AGENT_SOCKET: &str = "/run/noland/state-agent.sock";
const REQUIRED_AGENT_API_VERSION: u64 = 10;

pub async fn ensure_state_agent(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
    if probe_agent(remote).await.ok().and_then(|health| {
        health
            .get("agent_api_version")
            .and_then(serde_json::Value::as_u64)
    }) == Some(REQUIRED_AGENT_API_VERSION)
    {
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
        .env("COPYFILE_DISABLE", "1")
        .args([
            "--no-xattrs",
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
    let _ = std::fs::remove_file(&local_tar);

    let remote_src = format!("/opt/noland/state-agent-{stamp}");
    let remote_script = format!("/tmp/noland-bootstrap-agent-{stamp}.sh");
    let remote_log = format!("/tmp/noland-bootstrap-agent-{stamp}.log");
    let remote_status = format!("/tmp/noland-bootstrap-agent-{stamp}.status");
    let bootstrap = include_str!("../../../../state-agent/scripts/bootstrap-remote.sh");
    let encoded_script = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        bootstrap.as_bytes(),
    );
    let sudo = remote.sudo_prefix();
    let worker = format!(
        "flock -w 1200 /run/lock/noland-state-agent-bootstrap.lock bash {script} {src} /usr/local/bin/noland-state-agent {user}; code=$?; printf '%s\\n' \"$code\" > {status}",
        script = shell_escape(&remote_script),
        src = shell_escape(&remote_src),
        user = shell_escape(target_user),
        status = shell_escape(&remote_status),
    );
    let launcher = format!(
        "rm -f {status} {log}; nohup sh -c {worker} > {log} 2>&1 < /dev/null &",
        status = shell_escape(&remote_status),
        log = shell_escape(&remote_log),
        worker = shell_escape(&worker),
    );
    let setup = format!(
        "{sudo}rm -rf {src} && {sudo}mkdir -p {src} && {sudo}tar -xzf {tar} -C {src} && printf %s {encoded} | base64 -d > {script} && chmod 700 {script} && {sudo}sh -c {launcher}",
        sudo = sudo,
        src = shell_escape(&remote_src),
        tar = shell_escape(&remote_tar),
        encoded = shell_escape(&encoded_script),
        script = shell_escape(&remote_script),
        launcher = shell_escape(&launcher),
    );
    let setup_output = ssh_command(remote, setup, Duration::from_secs(180)).await?;
    if setup_output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed to launch state-agent installation: {}",
            concise_remote_failure(&setup_output.stdout, &setup_output.stderr)
        )));
    }

    wait_for_bootstrap(remote, &remote_status, &remote_log).await
}

async fn wait_for_bootstrap(
    remote: &RemoteExec,
    remote_status: &str,
    remote_log: &str,
) -> AppResult<()> {
    const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(20 * 60);
    const POLL_INTERVAL: Duration = Duration::from_secs(3);

    let deadline = tokio::time::Instant::now() + BOOTSTRAP_TIMEOUT;
    let status_command = format!(
        "if test -f {status}; then cat {status}; else printf RUNNING; fi",
        status = shell_escape(remote_status)
    );
    let mut last_connection_error = None;

    while tokio::time::Instant::now() < deadline {
        if probe_agent(remote)
            .await
            .ok()
            .is_some_and(|health| agent_api_is_compatible(&health))
        {
            return Ok(());
        }

        match ssh_command(remote, status_command.clone(), Duration::from_secs(20)).await {
            Ok(output) if output.status_code == 0 => {
                let status = output.stdout.trim();
                if let Ok(exit_code) = status.parse::<i32>() {
                    if exit_code != 0 {
                        let log = read_bootstrap_log(remote, remote_log).await;
                        return Err(AppError::Provisioning(format!(
                            "Failed to install or start state-agent (exit {exit_code}): {log}"
                        )));
                    }
                }
            }
            Ok(output) => {
                last_connection_error =
                    Some(concise_remote_failure(&output.stdout, &output.stderr));
            }
            Err(error) => {
                // Reboots and package-manager SSH restarts are transient. The detached
                // bootstrap keeps running and is checked again on the next poll.
                last_connection_error = Some(error.to_string());
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let log = read_bootstrap_log(remote, remote_log).await;
    Err(AppError::Timeout(format!(
        "Timed out waiting for detached state-agent installation. Last SSH error: {}. Bootstrap log: {}",
        last_connection_error.unwrap_or_else(|| "none".to_string()),
        log
    )))
}

async fn read_bootstrap_log(remote: &RemoteExec, remote_log: &str) -> String {
    let command = format!("tail -n 60 {} 2>/dev/null", shell_escape(remote_log));
    match ssh_command(remote, command, Duration::from_secs(30)).await {
        Ok(output) => concise_remote_failure(&output.stdout, &output.stderr),
        Err(error) => format!("log unavailable: {error}"),
    }
}

async fn ssh_command(
    remote: &RemoteExec,
    command: String,
    timeout: Duration,
) -> AppResult<crate::services::remote_exec::ExecOutput> {
    let remote = remote.clone();
    tokio::task::spawn_blocking(move || remote.ssh(&command, timeout))
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))?
}

fn agent_api_is_compatible(health: &serde_json::Value) -> bool {
    health
        .get("agent_api_version")
        .and_then(serde_json::Value::as_u64)
        == Some(REQUIRED_AGENT_API_VERSION)
}

pub async fn probe_agent(remote: &RemoteExec) -> AppResult<serde_json::Value> {
    call_agent_raw(remote, "GetHealth", serde_json::json!({})).await
}

pub async fn call_agent_raw(
    remote: &RemoteExec,
    method: &str,
    params: serde_json::Value,
) -> AppResult<serde_json::Value> {
    let request_id = uuid::Uuid::new_v4();
    let request = serde_json::json!({
        "id": request_id.to_string(),
        "method": method,
        "params": params,
    });
    let local_request = std::env::temp_dir().join(format!("noland-rpc-{request_id}.json"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut request_file = options
        .open(&local_request)
        .map_err(|error| AppError::State(format!("create state-agent RPC request: {error}")))?;
    let _local_request_guard = TempFileGuard(local_request.clone());
    request_file
        .write_all(format!("{request}\n").as_bytes())
        .and_then(|_| request_file.sync_all())
        .map_err(|error| AppError::State(format!("write state-agent RPC request: {error}")))?;
    drop(request_file);
    let remote_request = format!("/run/noland/noland-rpc-{request_id}.json");
    let transfer_task = {
        let remote = remote.clone();
        let local_request = local_request.clone();
        let remote_request = remote_request.clone();
        tokio::task::spawn_blocking(move || {
            remote.scp(&local_request, &remote_request, Duration::from_secs(60))
        })
    };
    let transfer = match transfer_task.await {
        Ok(Ok(transfer)) => transfer,
        Ok(Err(error)) => {
            cleanup_remote_request(remote, &remote_request).await;
            return Err(error);
        }
        Err(error) => {
            cleanup_remote_request(remote, &remote_request).await;
            return Err(AppError::Command(format!("join failure: {error}")));
        }
    };
    if transfer.status_code != 0 {
        cleanup_remote_request(remote, &remote_request).await;
        return Err(AppError::Provisioning(format!(
            "failed to transfer state-agent RPC request: {}",
            concise_remote_failure(&transfer.stdout, &transfer.stderr)
        )));
    }
    let rpc_timeout_secs = match method {
        "StartSeal" => 2 * 60 * 60,
        _ => 5 * 60,
    };
    let ssh_timeout = Duration::from_secs(rpc_timeout_secs + 60);
    let cmd = format!(
        "python3 -c 'import glob,os,socket,sys,time; path=sys.argv[1]; now=time.time(); [(os.unlink(p) if p != path and now-os.path.getmtime(p) > 300 else None) for p in glob.glob(\"/run/noland/noland-rpc-*.json\")]; req=open(path,\"rb\").read(); os.unlink(path); s=socket.socket(socket.AF_UNIX); s.settimeout({timeout}); s.connect(\"{sock}\"); s.sendall(req); s.shutdown(1); sys.stdout.buffer.write(b\"\".join(iter(lambda:s.recv(65536), b\"\")))' {request}",
        timeout = rpc_timeout_secs,
        sock = AGENT_SOCKET,
        request = shell_escape(&remote_request),
    );
    let output_task = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh(&cmd, ssh_timeout))
    };
    let output = match output_task.await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            cleanup_remote_request(remote, &remote_request).await;
            return Err(error);
        }
        Err(error) => {
            cleanup_remote_request(remote, &remote_request).await;
            return Err(AppError::Command(format!("join failure: {error}")));
        }
    };
    if output.status_code != 0 {
        cleanup_remote_request(remote, &remote_request).await;
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

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn cleanup_remote_request(remote: &RemoteExec, remote_request: &str) {
    let command = format!("rm -f -- {}", shell_escape(remote_request));
    let remote = remote.clone();
    let _ =
        tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(15))).await;
}

fn concise_remote_failure(stdout: &str, stderr: &str) -> String {
    const MAX_LINES: usize = 30;
    let combined = format!("{stdout}\n{stderr}");
    let lines = combined
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
