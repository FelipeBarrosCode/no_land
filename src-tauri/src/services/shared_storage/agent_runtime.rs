//! Deploy and start noland-state-agent on the remote disposable instance.

use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::errors::{AppError, AppResult};
use crate::services::remote_exec::RemoteExec;

const AGENT_SOCKET: &str = "/run/noland/state-agent.sock";
const REQUIRED_AGENT_API_VERSION: u64 = 22;

// Keep this aligned with install-lifecycle-agent.sh. A healthy running agent
// does not imply its shared installation workspace is safe to reuse as root.
const TRUSTED_WORKSPACE_PROBE: &str = r#"set -eu
link=/opt/noland/state-agent
test -L "$link"
test "$(stat -c '%u' "$link")" = 0
src=$(readlink -f "$link")
case "$src" in /opt/noland/state-agent-*) ;; *) exit 1 ;; esac
stamp=${src#/opt/noland/state-agent-}
case "$stamp" in ''|*[!0-9]*) exit 1 ;; esac
for path in /opt /opt/noland "$src"; do
  test "$(stat -c '%u' "$path")" = 0
  test -z "$(find "$path" -maxdepth 0 -perm /022 -print)"
done
untrusted=$(find "$src" -xdev \( ! -user root -o -perm /022 \) -print -quit)
test -z "$untrusted"
"#;

pub async fn ensure_state_agent(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
    let health = probe_agent(remote).await.ok();
    let active = deployment_has_active_work(remote, health.as_ref()).await?;
    let compatible = health.as_ref().and_then(|health| {
        health
            .get("agent_api_version")
            .and_then(serde_json::Value::as_u64)
    }) == Some(REQUIRED_AGENT_API_VERSION);
    // Reattach to compatible work before refreshing builds. Never restart a
    // daemon that still owns a capture, transfer, or restore transaction.
    if compatible && (active || prebuilt_deployment_matches(remote).await?) {
        return Ok(());
    }
    if active {
        return Err(AppError::Provisioning(
            "Wait for active Shared Storage operations before updating the agent.".into(),
        ));
    }

    let local_src = find_state_agent_source_dir()?;

    let stamp = chrono::Utc::now().timestamp();
    let local_tar = std::env::temp_dir().join(format!("noland-state-agent-{stamp}.tar.gz"));
    let source = local_src.clone();
    let archive_path = local_tar.clone();
    tokio::task::spawn_blocking(move || pack_state_agent_sources(&source, &archive_path))
        .await
        .map_err(|error| AppError::Command(format!("agent archive join failure: {error}")))??;

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
    // Windows checkouts (core.autocrlf=true) keep these scripts CRLF in the working
    // tree, and the bytes are embedded verbatim at compile time. bash on the remote
    // instance rejects `set -euo pipefail\r`, so normalize before shipping.
    let normalized_bootstrap = bootstrap.replace("\r\n", "\n").replace('\r', "\n");
    let encoded_script = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        normalized_bootstrap.as_bytes(),
    );
    let sudo = remote.sudo_prefix();
    let source_fingerprint = state_agent_source_fingerprint(&local_src)?;
    let worker = format!(
        "flock -w 1200 /run/lock/noland-state-agent-bootstrap.lock bash {script} {src} /usr/local/bin/noland-state-agent {user}; code=$?; if [ \"$code\" = 0 ]; then printf %s {fingerprint} > {src}/.noland-source-fingerprint || code=$?; fi; printf '%s\\n' \"$code\" > {status}",
        script = shell_escape(&remote_script),
        src = shell_escape(&remote_src),
        user = shell_escape(target_user),
        status = shell_escape(&remote_status),
        fingerprint = shell_escape(&source_fingerprint),
    );
    let launcher = format!(
        "rm -f {status} {log}; nohup sh -c {worker} > {log} 2>&1 < /dev/null &",
        status = shell_escape(&remote_status),
        log = shell_escape(&remote_log),
        worker = shell_escape(&worker),
    );
    let setup = format!(
        "{sudo}rm -rf {src} && {sudo}mkdir -p {src} && {sudo}tar -xzf {tar} -C {src} && {sudo}chown -R root:root {src} && {sudo}find {src} -xdev \\( -type f -o -type d \\) -exec chmod go-w {{}} + && printf %s {encoded} | base64 -d > {script} && chmod 700 {script} && {sudo}sh -c {launcher}",
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

async fn deployment_has_active_work(
    remote: &RemoteExec,
    health: Option<&serde_json::Value>,
) -> AppResult<bool> {
    let Some(health) = health else {
        return Ok(false);
    };
    if let Some(count) = health
        .get("active_operations")
        .and_then(serde_json::Value::as_u64)
    {
        return Ok(count > 0);
    }
    if health
        .get("unfinished_operations")
        .and_then(serde_json::Value::as_u64)
        == Some(0)
    {
        return Ok(false);
    }
    // Older agents count INTERRUPTED rows as unfinished. Inspect actual managed
    // work rather than letting historical interruptions block every update.
    let recent = call_agent_raw(
        remote,
        "ListRecentOperations",
        serde_json::json!({"projection":"matching", "limit":1000}),
    )
    .await?;
    Ok(legacy_list_has_active_work(&recent))
}

fn legacy_list_has_active_work(recent: &serde_json::Value) -> bool {
    recent
        .get("operations")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|operations| {
            operations.len() >= 1000
                || operations
                    .iter()
                    .any(|op| op.get("running").and_then(serde_json::Value::as_bool) != Some(false))
        })
}

/// API compatibility alone is insufficient: an updated application may ship
/// new agents without changing the RPC schema. Refresh the managed workspace
/// when its architecture-specific build manifest differs from the app bundle
/// or its ownership/permissions fail the lifecycle installer's trust checks.
async fn prebuilt_deployment_matches(remote: &RemoteExec) -> AppResult<bool> {
    let source = find_state_agent_source_dir()?;
    let fingerprint = state_agent_source_fingerprint(&source)?;
    let mut cases = String::new();
    let mut has_prebuilt_manifests = true;
    for arch in ["x86_64", "aarch64"] {
        let path = source.join("prebuilt").join(arch).join("SHA256SUMS");
        if !path.is_file() {
            // Source-only installs still need a trusted workspace, even though
            // there is no bundled binary manifest to compare.
            has_prebuilt_manifests = false;
            break;
        }
        let hash = format!("{:x}", Sha256::digest(fs::read(path)?));
        cases.push_str(&format!("{arch}) expected={hash} ;;\n"));
    }
    let manifest_probe = if has_prebuilt_manifests {
        format!(
            "arch=$(uname -m); case \"$arch\" in\n{cases}*) exit 1 ;; esac\nactual=$(sha256sum /opt/noland/state-agent/prebuilt/$arch/SHA256SUMS 2>/dev/null | cut -d' ' -f1); test \"$actual\" = \"$expected\""
        )
    } else {
        String::new()
    };
    let command = format!(
        "{}sh -c {}",
        remote.sudo_prefix(),
        shell_escape(&format!("{TRUSTED_WORKSPACE_PROBE}\ntest \"$(cat /opt/noland/state-agent/.noland-source-fingerprint 2>/dev/null)\" = {}\n{manifest_probe}", shell_escape(&fingerprint)))
    );
    Ok(ssh_command(remote, command, Duration::from_secs(20))
        .await?
        .status_code
        == 0)
}

/// Content identity, not RPC compatibility or checkout timestamps. Only shipped
/// source/config and prebuilt manifests participate; build artifacts do not.
fn state_agent_source_fingerprint(root: &Path) -> AppResult<String> {
    fn visit(root: &Path, directory: &Path, hash: &mut Sha256) -> AppResult<()> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink()
                || matches!(
                    entry.file_name().to_str(),
                    Some("target" | ".git" | ".DS_Store")
                )
            {
                continue;
            }
            if kind.is_dir() {
                visit(root, &path, hash)?;
            } else if kind.is_file()
                && (path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext,
                            "rs" | "toml" | "lock" | "sql" | "c" | "h" | "sh" | "service"
                        )
                    })
                    || entry.file_name() == "SHA256SUMS")
            {
                let relative = path
                    .strip_prefix(root)
                    .expect("source descendant")
                    .to_string_lossy()
                    .replace('\\', "/");
                hash.update((relative.len() as u64).to_le_bytes());
                hash.update(relative.as_bytes());
                let bytes = fs::read(path)?;
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    hash.update(b"noland-state-agent-source-v1\0");
    visit(root, root, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

pub fn locate_state_agent_source_dir() -> AppResult<PathBuf> {
    find_state_agent_source_dir()
}

fn find_state_agent_source_dir() -> AppResult<PathBuf> {
    if let Ok(explicit) = env::var("NOLAND_STATE_AGENT_SOURCE_DIR") {
        let candidate = PathBuf::from(explicit.trim());
        if is_state_agent_source_dir(&candidate) {
            return Ok(candidate);
        }
    }

    let workspace_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../state-agent");
    if is_state_agent_source_dir(&workspace_candidate) {
        return Ok(workspace_candidate);
    }

    let relative_candidates = [
        "state-agent",
        "_up_/state-agent",
        "../state-agent",
        "../../state-agent",
        "resources/state-agent",
        "Resources/state-agent",
        "resources/_up_/state-agent",
        "Resources/_up_/state-agent",
        "../Resources/state-agent",
        "../resources/state-agent",
        "../Resources/_up_/state-agent",
        "../resources/_up_/state-agent",
        "usr/lib/noland-connect/resources/state-agent",
        "usr/lib/noland-connect/resources/_up_/state-agent",
        "usr/lib/noland-connect/_up_/state-agent",
        "lib/noland-connect/resources/state-agent",
        "lib/noland-connect/resources/_up_/state-agent",
    ];

    for seed in state_agent_source_search_seeds() {
        for relative in relative_candidates {
            let candidate = seed.join(relative);
            if is_state_agent_source_dir(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(AppError::Provisioning(
        "Could not find the bundled state-agent source directory. Reinstall this app or set NOLAND_STATE_AGENT_SOURCE_DIR to a valid source tree."
            .into(),
    ))
}

fn is_state_agent_source_dir(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
        && path.join("crates/noland-state-agent/Cargo.toml").is_file()
        && path.join("systemd/noland-state-agent.service").is_file()
}

fn state_agent_source_search_seeds() -> Vec<PathBuf> {
    let mut seeds = Vec::new();

    if let Ok(resource_dir) = env::var("NOLAND_TAURI_RESOURCE_DIR") {
        let candidate = PathBuf::from(resource_dir.trim());
        if !candidate.as_os_str().is_empty() {
            push_path_ancestors(&candidate, 4, &mut seeds);
        }
    }

    if let Ok(current_exe) = env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            push_path_ancestors(exe_dir, 8, &mut seeds);
        }
    }

    if let Ok(cwd) = env::current_dir() {
        push_path_ancestors(&cwd, 4, &mut seeds);
    }

    seeds
}

fn pack_state_agent_sources(source_dir: &Path, archive_path: &Path) -> AppResult<()> {
    let archive_file = File::create(archive_path)
        .map_err(|error| AppError::Command(format!("create state-agent archive: {error}")))?;
    let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);

    append_archive_dir(&mut archive, source_dir, source_dir)?;
    archive
        .finish()
        .map_err(|error| AppError::Command(format!("finish state-agent archive: {error}")))?;
    Ok(())
}

fn append_archive_dir(
    archive: &mut tar::Builder<flate2::write::GzEncoder<File>>,
    root: &Path,
    current: &Path,
) -> AppResult<()> {
    for entry in fs::read_dir(current)
        .map_err(|error| AppError::Command(format!("read state-agent source dir: {error}")))?
    {
        let entry = entry.map_err(|error| {
            AppError::Command(format!("read state-agent source entry: {error}"))
        })?;
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|error| {
            AppError::Command(format!("resolve state-agent archive path: {error}"))
        })?;

        if should_skip_state_agent_archive_path(relative) {
            continue;
        }

        let file_type = entry.file_type().map_err(|error| {
            AppError::Command(format!("inspect state-agent source entry: {error}"))
        })?;
        if file_type.is_dir() {
            archive
                .append_dir(relative, &path)
                .map_err(|error| AppError::Command(format!("archive state-agent dir: {error}")))?;
            append_archive_dir(archive, root, &path)?;
        } else if file_type.is_file() {
            archive
                .append_path_with_name(&path, relative)
                .map_err(|error| {
                    AppError::Command(format!(
                        "archive state-agent file {}: {error}",
                        path.display()
                    ))
                })?;
        }
    }

    Ok(())
}

fn should_skip_state_agent_archive_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(name.as_ref(), "target" | ".git" | ".DS_Store")
    })
}

fn push_path_ancestors(start: &Path, levels: usize, output: &mut Vec<PathBuf>) {
    let mut current = Some(start);
    for _ in 0..levels {
        let Some(path) = current else {
            break;
        };
        let candidate = path.to_path_buf();
        if !output.contains(&candidate) {
            output.push(candidate);
        }
        current = path.parent();
    }
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
    let mut request_bytes = RpcInput(serde_json::to_vec(&request)?);
    request_bytes.0.push(b'\n');
    if request_bytes.0.len() > MAX_RPC_BYTES {
        return Err(AppError::InvalidInput(
            "state-agent RPC request is too large".into(),
        ));
    }
    let (socket_timeout, transport_timeout) = rpc_deadlines(method);
    let cmd = format!(
        "python3 -c 'import socket,sys; req=sys.stdin.buffer.read(4194305); s=socket.socket(socket.AF_UNIX); s.settimeout({timeout}); s.connect(\"{sock}\"); s.sendall(req); s.shutdown(1); sys.stdout.buffer.write(s.makefile(\"rb\").readline(4194305))'",
        timeout = socket_timeout.as_secs(),
        sock = AGENT_SOCKET,
    );
    let output = run_rpc_transport(
        rpc_ssh_command(remote, &cmd)?,
        request_bytes,
        transport_timeout,
    )
    .await?;
    parse_rpc_response(&output, &request_id.to_string(), method)
}

const MAX_RPC_BYTES: usize = 4 * 1024 * 1024;

fn rpc_deadlines(method: &str) -> (Duration, Duration) {
    let seconds = match method {
        "GetHealth"
        | "GetOperationStatus"
        | "GetBackupStatus"
        | "GetRestoreStatus"
        | "GetSealStatus"
        | "ListRecentOperations"
        | "CancelOperation" => (8, 15),
        "StartBackup" | "StartRestore" | "StartSeal" | "RetryOperation" => (30, 45),
        _ => (300, 315),
    };
    (
        Duration::from_secs(seconds.0),
        Duration::from_secs(seconds.1),
    )
}

// Keep the child owned by this future, including while writing stdin. Dropping
// a poll/admission future kills its SSH process rather than detaching a blocking
// task. Request bodies and transport output are never logged or put in argv.
fn rpc_ssh_command(remote: &RemoteExec, command: &str) -> AppResult<tokio::process::Command> {
    use crate::services::os_detection::OsDetection;
    use crate::utils::{
        managed_binaries::configure_bundled_linux_runtime, process::configure_no_window,
    };
    let os = OsDetection::new();
    let binary = os
        .locate_app_managed_binary("ssh", "NOLAND_SSH_BIN", cfg!(target_os = "windows"))
        .ok_or_else(|| {
            AppError::Command("Bundled SSH is unavailable for state-agent RPC".into())
        })?;
    let mut process = std::process::Command::new(&binary);
    configure_no_window(&mut process);
    configure_bundled_linux_runtime(
        &mut process,
        &binary,
        "ssh-runtime",
        os.managed_binary_target_triple(),
    );
    process.args([
        "-T",
        "-p",
        &remote.ssh_port.to_string(),
        "-i",
        &remote.private_key_path,
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        &format!("UserKnownHostsFile={}", os.ssh_known_hosts_null_file()),
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "BatchMode=yes",
        "-o",
        "PreferredAuthentications=publickey",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "PermitLocalCommand=no",
        "-l",
        &remote.ssh_user,
        "--",
        &remote.ssh_host,
        command,
    ]);
    Ok(tokio::process::Command::from(process))
}

struct RpcInput(Vec<u8>);
impl Drop for RpcInput {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

async fn read_rpc_output(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> AppResult<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(AppError::State(
            "state-agent RPC output exceeds its limit".into(),
        ));
    }
    Ok(bytes)
}

async fn run_rpc_transport(
    mut command: tokio::process::Command,
    input: RpcInput,
    timeout: Duration,
) -> AppResult<Vec<u8>> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped RPC stdin");
    let stdout = child.stdout.take().expect("piped RPC stdout");
    let stderr = child.stderr.take().expect("piped RPC stderr");
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            async {
                stdin.write_all(&input.0).await?;
                stdin.shutdown().await?;
                drop(stdin);
                drop(input);
                Ok::<_, AppError>(())
            },
            read_rpc_output(stdout, MAX_RPC_BYTES),
            read_rpc_output(stderr, 16 * 1024),
            async { child.wait().await.map_err(AppError::from) },
        )
    })
    .await;
    match result {
        Ok(Ok(((), stdout, _, status))) if status.success() => Ok(stdout),
        Ok(Ok(_)) => Err(AppError::Command(
            "state-agent RPC SSH transport failed".into(),
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            Err(AppError::Timeout(
                "state-agent RPC transport deadline exceeded".into(),
            ))
        }
    }
}

fn parse_rpc_response(
    output: &[u8],
    request_id: &str,
    method: &str,
) -> AppResult<serde_json::Value> {
    if output.last() != Some(&b'\n') {
        return Err(AppError::State(
            "state-agent returned a truncated RPC response frame".into(),
        ));
    }
    let line = output
        .split(|byte| *byte == b'\n')
        .find(|line| {
            line.iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                == Some(b'{')
        })
        .ok_or_else(|| AppError::State("state-agent returned no JSON".into()))?;
    let value: serde_json::Value = serde_json::from_slice(line)
        .map_err(|_| AppError::State("state-agent RPC response is invalid JSON".into()))?;
    if value.get("id").and_then(serde_json::Value::as_str) != Some(request_id) {
        return Err(AppError::State(
            "state-agent RPC response ID did not match the request".into(),
        ));
    }
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(AppError::Provisioning(format!(
            "state-agent {method}: {error}"
        )));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| AppError::State("state-agent RPC response has no result".into()))
}

#[cfg(test)]
mod tests {
    use super::{is_state_agent_source_dir, pack_state_agent_sources};
    use flate2::read::GzDecoder;
    use std::{fs, path::PathBuf};

    #[test]
    fn status_and_admission_deadlines_are_bounded_independently_of_remote_work() {
        use std::time::Duration;
        for method in ["GetOperationStatus", "ListRecentOperations", "GetHealth"] {
            let (socket, transport) = super::rpc_deadlines(method);
            assert!(socket < transport);
            assert!(transport <= Duration::from_secs(15));
        }
        assert_eq!(
            super::rpc_deadlines("StartSeal"),
            super::rpc_deadlines("StartBackup")
        );
    }

    #[test]
    fn only_a_correlated_rpc_error_is_a_definitive_rejection() {
        let wrong = b"{\"id\":\"other\",\"error\":\"rejected\"}\n";
        assert!(matches!(
            super::parse_rpc_response(wrong, "request", "StartSeal"),
            Err(crate::errors::AppError::State(_))
        ));
        let rejected = b"{\"id\":\"request\",\"error\":\"rejected\"}\n";
        assert!(matches!(
            super::parse_rpc_response(rejected, "request", "StartSeal"),
            Err(crate::errors::AppError::Provisioning(_))
        ));
        let malformed = b"{request-secret-not-json}";
        assert!(super::parse_rpc_response(malformed, "request", "StartSeal")
            .unwrap_err()
            .to_string()
            .contains("truncated"));
        assert!(
            !super::parse_rpc_response(malformed, "request", "StartSeal")
                .unwrap_err()
                .to_string()
                .contains("request-secret")
        );
    }

    #[tokio::test]
    async fn rpc_output_is_bounded_even_without_a_newline_or_eof() {
        assert!(super::read_rpc_output(tokio::io::repeat(b'x'), 1024)
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_transport_owns_and_times_out_its_child() {
        use std::time::Duration;
        let mut echo = tokio::process::Command::new("sh");
        echo.args(["-c", "exec cat"]);
        let bytes = b"request-body\n".to_vec();
        assert_eq!(
            super::run_rpc_transport(echo, super::RpcInput(bytes.clone()), Duration::from_secs(2))
                .await
                .unwrap(),
            bytes
        );
        let mut hang = tokio::process::Command::new("sh");
        hang.args(["-c", "exec sleep 60"]);
        let result =
            super::run_rpc_transport(hang, super::RpcInput(Vec::new()), Duration::from_millis(10))
                .await;
        assert!(matches!(result, Err(crate::errors::AppError::Timeout(_))));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "noland-state-agent-runtime-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn create_minimal_source_tree(root: &PathBuf) {
        fs::create_dir_all(root.join("crates/noland-state-agent/src")).unwrap();
        fs::create_dir_all(root.join("systemd")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(
            root.join("crates/noland-state-agent/Cargo.toml"),
            "[package]\nname = \"noland-state-agent\"\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/noland-state-agent/src/main.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("systemd/noland-state-agent.service"),
            "[Service]\n",
        )
        .unwrap();
        fs::write(root.join("target/debug/large-build-output"), "skip me\n").unwrap();
    }

    #[test]
    fn legacy_interrupted_history_does_not_block_updates_but_active_or_unknown_work_does() {
        assert!(!super::legacy_list_has_active_work(
            &serde_json::json!({"operations":[{"state":"INTERRUPTED","running":false}]})
        ));
        assert!(super::legacy_list_has_active_work(
            &serde_json::json!({"operations":[{"running":true}]})
        ));
        assert!(super::legacy_list_has_active_work(
            &serde_json::json!({"operations":[{}]})
        ));
        assert!(super::legacy_list_has_active_work(&serde_json::json!({})));
    }

    #[test]
    fn source_fingerprint_changes_without_an_api_bump_but_ignores_build_artifacts() {
        let root = temp_dir("fingerprint");
        create_minimal_source_tree(&root);
        let original = super::state_agent_source_fingerprint(&root).unwrap();
        fs::write(root.join("target/debug/generated.rs"), "not shipped").unwrap();
        assert_eq!(
            super::state_agent_source_fingerprint(&root).unwrap(),
            original
        );
        fs::write(
            root.join("crates/noland-state-agent/src/main.rs"),
            "fn main() { println!(\"new\"); }",
        )
        .unwrap();
        assert_ne!(
            super::state_agent_source_fingerprint(&root).unwrap(),
            original
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_minimal_state_agent_source_tree() {
        let root = temp_dir("recognizes-source");
        create_minimal_source_tree(&root);

        assert!(is_state_agent_source_dir(&root));
    }

    #[test]
    fn packs_sources_without_platform_tar_and_excludes_target() {
        let root = temp_dir("pack-source");
        create_minimal_source_tree(&root);
        let archive_path = root.with_extension("tar.gz");

        pack_state_agent_sources(&root, &archive_path).unwrap();

        let archive_file = fs::File::open(&archive_path).unwrap();
        let decoder = GzDecoder::new(archive_file);
        let mut archive = tar::Archive::new(decoder);
        let entries = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert!(entries.iter().any(|path| path == "Cargo.toml"));
        assert!(entries
            .iter()
            .any(|path| path == "crates/noland-state-agent/Cargo.toml"));
        assert!(entries
            .iter()
            .any(|path| path == "systemd/noland-state-agent.service"));
        assert!(!entries.iter().any(|path| path.starts_with("target/")));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&archive_path);
    }
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
