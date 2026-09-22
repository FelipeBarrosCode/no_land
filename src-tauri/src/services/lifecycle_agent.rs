//! Deploy, configure, and query the lifecycle agent on a remote instance.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use noland_rclone_adapter::EphemeralRcloneSession;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

use super::{
    app_context::AppContext,
    remote_exec::{ExecOutput, RemoteExec},
    shared_storage::{
        agent_runtime::ensure_state_agent, provider_profiles::shared_profile_manager,
        rclone_adapter::mint_ephemeral_session, shared_storage_manager::SharedStorageManager,
    },
};

const INSTALLER_TEMPLATE: &str =
    include_str!("../../../state-agent/scripts/install-lifecycle-agent.sh");
const SYSTEMD_UNIT: &str =
    include_str!("../../../state-agent/systemd/noland-lifecycle-agent.service");

const AGENT_VERSION: &str = "0.1.0";
const DEPLOYMENT_REVISION: &str = "13";
const AGENT_BINARY: &str = "/usr/local/bin/noland-lifecycle-agent";
const AGENT_SERVICE: &str = "noland-lifecycle-agent.service";
const REVISION_PATH: &str = "/usr/local/share/noland-lifecycle-agent/install-revision";
const CONFIG_PATH: &str = "/etc/noland/lifecycle/config.json";
const CAPABILITY_PATH: &str = "/var/lib/noland/lifecycle/storage-capability.json";
const STATUS_SOCKET: &str = "/run/noland/lifecycle/agent.sock";
const MAX_STATUS_BYTES: usize = 1024 * 1024;

static DEPLOYMENT_CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleRankedApp {
    pub app_id: String,
    pub foreground_active_ms: u64,
    pub process_runtime_ms: u64,
    pub launch_count: u64,
    pub last_active_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleAgentStatus {
    pub enabled: bool,
    pub state: String,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub idle_duration_ms: u64,
    pub timeout_ms: u64,
    pub time_remaining_ms: u64,
    pub timeout_reached_at: Option<DateTime<Utc>>,
    pub active_run_id: Option<String>,
    pub ranked_apps: Vec<LifecycleRankedApp>,
    pub last_error: Option<String>,
}

pub struct LifecycleAgentProvisioner;

impl LifecycleAgentProvisioner {
    pub async fn ensure_installed(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let _guard = DEPLOYMENT_CONFIG_LOCK.lock().await;
        ensure_installed_locked(remote, target_user).await
    }

    pub async fn configure_for_instance(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
    ) -> AppResult<()> {
        let settings = context.load_state().await.auto_shutdown.settings;
        Self::configure_for_instance_settings(context, remote, instance_id, &settings).await
    }

    pub async fn configure_for_instance_settings(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        settings: &crate::models::app_state::AutoShutdownSettings,
    ) -> AppResult<()> {
        let _guard = DEPLOYMENT_CONFIG_LOCK.lock().await;

        {
            let state = context.load_state().await;
            if !state
                .provisioned_servers
                .iter()
                .any(|server| server.instance_id == instance_id)
            {
                return Err(AppError::NotFound(format!(
                    "Provisioned instance {instance_id} is not present in application state"
                )));
            }

            if settings.enabled {
                let active_profile = state
                    .shared_storage_profiles
                    .iter()
                    .find(|profile| profile.active)
                    .ok_or_else(|| {
                        AppError::InvalidInput(
                            "Activate a shared-storage profile before enabling lifecycle automation"
                                .to_string(),
                        )
                    })?;
                let profile_secret = state
                    .shared_storage_credentials
                    .profiles
                    .get(&active_profile.id)
                    .ok_or_else(|| {
                        AppError::InvalidInput(
                            "Reconnect the active shared-storage profile before enabling lifecycle automation"
                                .to_string(),
                        )
                    })?;
                let has_repository_key = !state
                    .shared_storage_credentials
                    .repository_key_hex
                    .trim()
                    .is_empty()
                    || !profile_secret.repository_key_hex.trim().is_empty();
                if !has_repository_key {
                    return Err(AppError::InvalidInput(
                        "The active shared-storage profile has no repository key".to_string(),
                    ));
                }
                if state.credentials.vast_api_key.trim().is_empty() {
                    return Err(AppError::InvalidInput(
                        "A Vast.ai API key is required for lifecycle automation".to_string(),
                    ));
                }
            }
        }

        let config = LifecycleConfig::from_settings(
            settings.enabled,
            instance_id,
            settings.inactivity_hours,
            settings.backup_app_limit,
            context.config.vast_base_url.clone(),
        )?;

        ensure_state_agent(remote, &context.config.audio_target_user).await?;
        ensure_installed_locked(remote, &context.config.audio_target_user).await?;

        let staging = LocalStagingDir::create("noland-lifecycle-config")?;
        let config_bytes = serde_json::to_vec_pretty(&config).map_err(|error| {
            AppError::Serialization(format!(
                "Could not serialize lifecycle configuration: {error}"
            ))
        })?;
        let config_sha256 = sha256_bytes(&config_bytes);
        let config_file = staging.write_private("config.json", &config_bytes)?;

        let capability_asset = if settings.enabled {
            Some(
                prepare_capability(context, instance_id, &staging)
                    .await
                    .map_err(redact_capability_error)?,
            )
        } else {
            None
        };

        install_configuration(
            remote,
            &config_file,
            &config_sha256,
            capability_asset.as_ref(),
            config.instance_id,
            config.enabled,
        )
        .await?;

        info!(
            instance_id,
            enabled = settings.enabled,
            "Lifecycle-agent configuration installed"
        );
        Ok(())
    }

    pub async fn get_status(remote: &RemoteExec) -> AppResult<LifecycleAgentStatus> {
        let _guard = DEPLOYMENT_CONFIG_LOCK.lock().await;
        let request_id = Uuid::new_v4().to_string();
        let python = format!(
            r#"import json
import socket
import sys

limit = {MAX_STATUS_BYTES}
request_id = sys.argv[1]
request = {{"id": request_id, "method": "GetStatus", "params": {{}}}}
stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
stream.settimeout(10)
stream.connect({socket_path:?})
stream.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode("utf-8"))
data = bytearray()
while len(data) <= limit:
    chunk = stream.recv(min(65536, limit + 1 - len(data)))
    if not chunk:
        break
    data.extend(chunk)
    if b"\n" in chunk:
        break
newline = data.find(b"\n")
if newline < 0:
    raise RuntimeError("lifecycle RPC returned an incomplete response")
line = bytes(data[:newline])
if len(line) > limit:
    raise RuntimeError("lifecycle RPC response exceeded the output limit")
sys.stdout.buffer.write(line + b"\n")
"#,
            socket_path = STATUS_SOCKET,
        );
        let command = format!(
            "python3 -c {} {}",
            shell_single_quote(&python),
            shell_single_quote(&request_id)
        );
        let output = run_root_script(remote, command, Duration::from_secs(20)).await?;
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Could not query lifecycle-agent status: {}",
                concise_remote_failure(&output)
            )));
        }
        if output.stdout.len() > MAX_STATUS_BYTES + 1 {
            return Err(AppError::Provisioning(
                "Lifecycle-agent status response exceeded the output limit".to_string(),
            ));
        }

        let envelope: RpcEnvelope<LifecycleAgentStatus> =
            serde_json::from_str(output.stdout.trim()).map_err(|error| {
                AppError::Serialization(format!(
                    "Lifecycle-agent returned an invalid status envelope: {error}"
                ))
            })?;
        if envelope.id != request_id {
            return Err(AppError::Provisioning(
                "Lifecycle-agent status response ID did not match the request".to_string(),
            ));
        }
        if envelope.error.is_some() {
            return Err(AppError::Provisioning(
                "Lifecycle-agent rejected the status request".to_string(),
            ));
        }
        envelope.result.ok_or_else(|| {
            AppError::Provisioning(
                "Lifecycle-agent status response contained no result".to_string(),
            )
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleConfig {
    enabled: bool,
    instance_id: u64,
    inactivity_seconds: u64,
    backup_app_limit: u8,
    controller_dead_zone: f32,
    state_agent_socket: &'static str,
    status_socket: &'static str,
    activity_socket: &'static str,
    database_path: &'static str,
    capability_path: &'static str,
    vast_base_url: String,
    provider_action: &'static str,
}

impl LifecycleConfig {
    fn from_settings(
        enabled: bool,
        instance_id: u64,
        inactivity_hours: f32,
        backup_app_limit: u8,
        vast_base_url: String,
    ) -> AppResult<Self> {
        if !inactivity_hours.is_finite() || !(1.0 / 12.0..=24.0).contains(&inactivity_hours) {
            return Err(AppError::InvalidInput(
                "Inactivity timeout must be between 5 minutes and 24 hours".to_string(),
            ));
        }
        if !(1..=10).contains(&backup_app_limit) {
            return Err(AppError::InvalidInput(
                "Backup app limit must be between 1 and 10".to_string(),
            ));
        }
        let base_url = vast_base_url.trim();
        if !base_url.starts_with("https://")
            || base_url.len() <= "https://".len()
            || base_url.chars().any(char::is_whitespace)
        {
            return Err(AppError::InvalidInput(
                "Vast API base URL must be an HTTPS URL".to_string(),
            ));
        }

        Ok(Self {
            enabled,
            instance_id,
            inactivity_seconds: (f64::from(inactivity_hours) * 3600.0).round() as u64,
            backup_app_limit,
            controller_dead_zone: 0.15,
            state_agent_socket: "/run/noland/state-agent.sock",
            status_socket: STATUS_SOCKET,
            activity_socket: "/run/noland/sunshine-events.sock",
            database_path: "/var/lib/noland/lifecycle/runtime.db",
            capability_path: CAPABILITY_PATH,
            vast_base_url: base_url.to_string(),
            provider_action: "destroy",
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageCapability {
    version: u32,
    instance_id: u64,
    profile_id: String,
    expires_at: DateTime<Utc>,
    session: EphemeralRcloneSession,
    master_key_hex: String,
    provider: VastCapability,
}

impl Drop for StorageCapability {
    fn drop(&mut self) {
        self.master_key_hex.clear();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VastCapability {
    kind: &'static str,
    api_key: String,
    instance_id: u64,
    action: &'static str,
}

impl Drop for VastCapability {
    fn drop(&mut self) {
        self.api_key.clear();
    }
}

struct CapabilityAsset {
    path: PathBuf,
    sha256: String,
}

#[derive(Deserialize)]
struct RpcEnvelope<T> {
    id: String,
    result: Option<T>,
    error: Option<String>,
}

async fn ensure_installed_locked(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
    validate_target_user(target_user)?;
    if probe_existing(remote, target_user).await? {
        info!(service = AGENT_SERVICE, "Remote lifecycle agent is ready");
        return Ok(());
    }

    let staging = LocalStagingDir::create("noland-lifecycle-install")?;
    let installer = materialize_installer(&staging)?;
    let installer_sha256 = file_sha256(&installer)?;
    let deployment_id = Uuid::new_v4().simple().to_string();
    let remote_upload = format!("/tmp/noland-lifecycle-install-{deployment_id}.sh");
    let root_staging = format!("/run/noland-lifecycle-install-{deployment_id}");
    let root_installer = format!("{root_staging}/install.sh");

    if let Err(error) = upload_file(
        remote,
        installer,
        &remote_upload,
        Duration::from_secs(60),
        "lifecycle-agent installer",
    )
    .await
    {
        cleanup_remote_uploads(remote, &[&remote_upload]).await;
        return Err(error);
    }

    let script = format!(
        r#"set -euo pipefail
umask 077
if [[ -e {root_staging} || -L {root_staging} ]]; then
  echo 'randomized lifecycle-agent staging path already exists' >&2
  exit 1
fi
mkdir -m 0700 {root_staging}
cleanup() {{ rm -rf {root_staging}; rm -f {remote_upload}; }}
trap cleanup EXIT
if [[ ! -f {remote_upload} || -L {remote_upload} ]]; then
  echo 'uploaded lifecycle-agent installer is not a regular file' >&2
  exit 1
fi
install -o root -g root -m 0600 {remote_upload} {root_installer}
rm -f {remote_upload}
printf '%s  %s\n' {installer_sha256} {root_installer} | sha256sum -c -
chmod 0700 {root_installer}
{root_installer} {target_user} {version} {revision} {root_staging}"#,
        root_staging = shell_single_quote(&root_staging),
        remote_upload = shell_single_quote(&remote_upload),
        root_installer = shell_single_quote(&root_installer),
        installer_sha256 = shell_single_quote(&installer_sha256),
        target_user = shell_single_quote(target_user),
        version = shell_single_quote(AGENT_VERSION),
        revision = shell_single_quote(DEPLOYMENT_REVISION),
    );
    let output = run_root_script(remote, script, Duration::from_secs(30 * 60)).await?;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed building or installing noland-lifecycle-agent: {}",
            concise_remote_failure(&output)
        )));
    }
    if !output.stdout.contains("NOLAND_LIFECYCLE_AGENT_READY") {
        return Err(AppError::Provisioning(format!(
            "Lifecycle-agent installer completed without its readiness marker: {}",
            concise_remote_failure(&output)
        )));
    }

    info!(
        service = AGENT_SERVICE,
        version = AGENT_VERSION,
        revision = DEPLOYMENT_REVISION,
        "Remote lifecycle agent installed and started"
    );
    Ok(())
}

async fn probe_existing(remote: &RemoteExec, target_user: &str) -> AppResult<bool> {
    let expected_output = format!("noland-lifecycle-agent {AGENT_VERSION}");
    let script = format!(
        r#"set -euo pipefail
if ! id {target_user} >/dev/null 2>&1; then
  printf '%s\n' NOLAND_LIFECYCLE_AGENT_MISSING
  exit 0
fi
expected_group="$(id -gn {target_user})"
installed_version="$(timeout 5s {binary} --version 2>/dev/null || true)"
installed_revision="$(cat {revision_path} 2>/dev/null || true)"
unit_state="$(systemctl show --property=LoadState --value {service} 2>/dev/null || true)"
unit_group="$(systemctl show --property=Group --value {service} 2>/dev/null || true)"
if [[ -x {binary} ]] \
    && [[ "$installed_version" == {expected_version} ]] \
    && [[ "$installed_revision" == {expected_revision} ]] \
    && [[ "$unit_state" == "loaded" ]] \
    && [[ "$unit_group" == "$expected_group" ]]; then
  systemctl enable {service} >/dev/null
  if ! systemctl is-active --quiet {service}; then
    systemctl start {service}
  fi
  printf '%s\n' NOLAND_LIFECYCLE_AGENT_READY
else
  printf '%s\n' NOLAND_LIFECYCLE_AGENT_MISSING
fi"#,
        target_user = shell_single_quote(target_user),
        binary = AGENT_BINARY,
        revision_path = REVISION_PATH,
        service = AGENT_SERVICE,
        expected_version = shell_single_quote(&expected_output),
        expected_revision = shell_single_quote(DEPLOYMENT_REVISION),
    );
    let output = run_root_script(remote, script, Duration::from_secs(45)).await?;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed checking the existing lifecycle agent: {}",
            concise_remote_failure(&output)
        )));
    }
    Ok(output
        .stdout
        .lines()
        .any(|line| line.trim() == "NOLAND_LIFECYCLE_AGENT_READY"))
}

async fn prepare_capability(
    context: &AppContext,
    instance_id: u64,
    staging: &LocalStagingDir,
) -> AppResult<CapabilityAsset> {
    let active = SharedStorageManager::resolve_active_profile(context)
        .await
        .map_err(|_| AppError::Provisioning("Could not resolve lifecycle storage access".into()))?;
    let profile_id = active.profile.id.clone();
    let mut repository_key = shared_profile_manager()
        .retrieve_repository_key(context, &active.profile)
        .await
        .map_err(|_| {
            AppError::Provisioning("Could not retrieve the lifecycle repository key".into())
        })?;
    let seed = Uuid::new_v4().to_string();
    let session = match mint_ephemeral_session(
        &active.profile.provider,
        &active.credentials,
        &active.provider_fields,
        active.profile.bucket.as_deref(),
        active.profile.prefix.as_deref(),
        &active.remote_name,
        &seed,
    ) {
        Ok(session) => session,
        Err(_) => {
            repository_key.fill(0);
            return Err(AppError::Provisioning(
                "Could not mint lifecycle storage access".into(),
            ));
        }
    };
    drop(active);

    let master_key_hex = hex::encode(repository_key);
    repository_key.fill(0);
    let api_key = {
        let state = context.load_state().await;
        let key = state.credentials.vast_api_key.clone();
        if key.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "A Vast.ai API key is required for lifecycle automation".into(),
            ));
        }
        key
    };

    let capability = StorageCapability {
        version: 1,
        instance_id,
        profile_id,
        expires_at: Utc::now() + chrono::Duration::days(30),
        session,
        master_key_hex,
        provider: VastCapability {
            kind: "vast",
            api_key,
            instance_id,
            action: "destroy",
        },
    };
    let mut bytes = serde_json::to_vec_pretty(&capability).map_err(|_| {
        AppError::Serialization("Could not serialize lifecycle capability".to_string())
    })?;
    drop(capability);
    let sha256 = sha256_bytes(&bytes);
    let write_result = staging.write_private("storage-capability.json", &bytes);
    bytes.fill(0);
    let path = write_result?;
    Ok(CapabilityAsset { path, sha256 })
}

fn redact_capability_error(error: AppError) -> AppError {
    match error {
        AppError::InvalidInput(message) => AppError::InvalidInput(message),
        _ => AppError::Provisioning(
            "Could not prepare lifecycle storage capability; reconnect storage and retry"
                .to_string(),
        ),
    }
}

async fn install_configuration(
    remote: &RemoteExec,
    config_file: &Path,
    config_sha256: &str,
    capability: Option<&CapabilityAsset>,
    expected_instance_id: u64,
    expected_enabled: bool,
) -> AppResult<()> {
    let operation_id = Uuid::new_v4().simple().to_string();
    let remote_config = format!("/tmp/noland-lifecycle-config-{operation_id}.json");
    let root_staging = format!("/run/noland-lifecycle-config-{operation_id}");
    let root_config = format!("{root_staging}/config.json");
    let root_capability = format!("{root_staging}/storage-capability.json");

    if let Err(error) = upload_file(
        remote,
        config_file.to_path_buf(),
        &remote_config,
        Duration::from_secs(60),
        "lifecycle configuration",
    )
    .await
    {
        cleanup_remote_uploads(remote, &[&remote_config]).await;
        return Err(error);
    }

    let create_staging = format!(
        r#"set -euo pipefail
umask 077
if [[ -e {root_staging} || -L {root_staging} ]]; then
  echo 'randomized lifecycle configuration staging path already exists' >&2
  exit 1
fi
mkdir -m 0700 {root_staging}"#,
        root_staging = shell_single_quote(&root_staging),
    );
    match run_root_script(remote, create_staging, Duration::from_secs(20)).await {
        Ok(output) if output.status_code == 0 => {}
        Ok(output) => {
            cleanup_configuration_staging(remote, &remote_config, &root_staging).await;
            return Err(AppError::Provisioning(format!(
                "Could not create lifecycle-agent root staging: {}",
                concise_remote_failure(&output)
            )));
        }
        Err(error) => {
            cleanup_configuration_staging(remote, &remote_config, &root_staging).await;
            return Err(error);
        }
    }

    if let Some(capability) = capability {
        if let Err(error) = stream_root_private_file(
            remote,
            &capability.path,
            &root_staging,
            &root_capability,
            &capability.sha256,
        )
        .await
        {
            cleanup_configuration_staging(remote, &remote_config, &root_staging).await;
            return Err(error);
        }
    }

    let capability_stage = capability.map_or_else(
        || ":".to_string(),
        |capability| {
            format!(
                r#"if [[ ! -f {root_capability} || -L {root_capability} ]]; then
  echo 'staged lifecycle capability is not a regular file' >&2
  exit 1
fi
printf '%s  %s\n' {capability_sha256} {root_capability} | sha256sum -c -"#,
                root_capability = shell_single_quote(&root_capability),
                capability_sha256 = shell_single_quote(&capability.sha256),
            )
        },
    );
    let capability_install = if capability.is_some() {
        format!(
            r#"install -d -o root -g root -m 0750 /var/lib/noland/lifecycle
capability_temp="$(mktemp /var/lib/noland/lifecycle/.storage-capability.json.XXXXXX)"
install -o root -g root -m 0600 {root_capability} "$capability_temp"
mv -f "$capability_temp" {capability_path}
capability_temp="""#,
            root_capability = shell_single_quote(&root_capability),
            capability_path = shell_single_quote(CAPABILITY_PATH),
        )
    } else {
        format!("rm -f {}", shell_single_quote(CAPABILITY_PATH))
    };
    let readiness_python = r#"import json
import socket
import sys

reload_request = {"id": "config-reload", "method": "ReloadConfig", "params": {}}
health_request = {"id": "config-readiness", "method": "GetHealth", "params": {}}
stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
stream.settimeout(2)
stream.connect(sys.argv[1])
reader = stream.makefile("rb")
stream.sendall((json.dumps(reload_request, separators=(",", ":")) + "\n").encode())
reload_response = json.loads(reader.readline(65537))
if reload_response.get("id") != reload_request["id"] or reload_response.get("error") is not None:
    raise SystemExit(1)
stream.sendall((json.dumps(health_request, separators=(",", ":")) + "\n").encode())
health_response = json.loads(reader.readline(65537))
result = health_response.get("result") or {}
expected_instance = int(sys.argv[2])
expected_enabled = sys.argv[3] == "true"
if health_response.get("id") != health_request["id"] or health_response.get("error") is not None:
    raise SystemExit(1)
if result.get("instanceId") != expected_instance or result.get("enabled") is not expected_enabled:
    raise SystemExit(1)
"#;

    let script = format!(
        r#"set -euo pipefail
umask 077
if [[ ! -d {root_staging} || -L {root_staging} ]]; then
  echo 'lifecycle configuration root staging is invalid' >&2
  exit 1
fi
config_temp=""
capability_temp=""
cleanup() {{
  rm -rf {root_staging}
  rm -f {remote_config}
  if [[ -n "$config_temp" ]]; then rm -f "$config_temp"; fi
  if [[ -n "$capability_temp" ]]; then rm -f "$capability_temp"; fi
}}
trap cleanup EXIT
if [[ ! -f {remote_config} || -L {remote_config} ]]; then
  echo 'uploaded lifecycle configuration is not a regular file' >&2
  exit 1
fi
install -o root -g root -m 0600 {remote_config} {root_config}
rm -f {remote_config}
printf '%s  %s\n' {config_sha256} {root_config} | sha256sum -c -
{capability_stage}
{capability_install}
install -d -o root -g root -m 0755 /etc/noland/lifecycle
config_temp="$(mktemp /etc/noland/lifecycle/.config.json.XXXXXX)"
install -o root -g root -m 0644 {root_config} "$config_temp"
mv -f "$config_temp" {config_path}
config_temp=""
systemctl restart {service}
for _ in $(seq 1 30); do
  if python3 -c {readiness_python} {status_socket} {expected_instance_id} {expected_enabled}; then
    printf '%s\n' NOLAND_LIFECYCLE_CONFIG_READY
    exit 0
  fi
  sleep 1
  if ! systemctl is-active --quiet {service}; then
    systemctl --no-pager --full status {service} >&2 || true
    exit 1
  fi
done
echo 'lifecycle-agent did not report the expected configuration after restart' >&2
exit 1"#,
        root_staging = shell_single_quote(&root_staging),
        remote_config = shell_single_quote(&remote_config),
        root_config = shell_single_quote(&root_config),
        config_sha256 = shell_single_quote(config_sha256),
        capability_stage = capability_stage,
        capability_install = capability_install,
        config_path = shell_single_quote(CONFIG_PATH),
        service = AGENT_SERVICE,
        readiness_python = shell_single_quote(readiness_python),
        status_socket = shell_single_quote(STATUS_SOCKET),
        expected_instance_id = shell_single_quote(&expected_instance_id.to_string()),
        expected_enabled = shell_single_quote(if expected_enabled { "true" } else { "false" }),
    );
    let output = run_root_script(remote, script, Duration::from_secs(90)).await?;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed installing lifecycle-agent configuration: {}",
            concise_remote_failure(&output)
        )));
    }
    if !output.stdout.contains("NOLAND_LIFECYCLE_CONFIG_READY") {
        return Err(AppError::Provisioning(
            "Lifecycle-agent configuration completed without its readiness marker".to_string(),
        ));
    }
    Ok(())
}

async fn stream_root_private_file(
    remote: &RemoteExec,
    local_path: &Path,
    root_staging: &str,
    destination: &str,
    expected_sha256: &str,
) -> AppResult<()> {
    let bytes = fs::read(local_path).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not read private lifecycle capability for secure upload: {error}"
        ))
    })?;
    let script = format!(
        r#"set -euo pipefail
umask 077
if [[ ! -d {root_staging} || -L {root_staging} ]]; then
  echo 'lifecycle capability root staging is invalid' >&2
  exit 1
fi
destination_temp="$(mktemp {root_staging}/.capability.XXXXXX)"
cleanup() {{ rm -f "$destination_temp"; }}
trap cleanup EXIT
cat > "$destination_temp"
chown root:root "$destination_temp"
chmod 0600 "$destination_temp"
printf '%s  %s\n' {expected_sha256} "$destination_temp" | sha256sum -c -
mv -f "$destination_temp" {destination}
destination_temp="""#,
        root_staging = shell_single_quote(root_staging),
        expected_sha256 = shell_single_quote(expected_sha256),
        destination = shell_single_quote(destination),
    );
    let command = if remote.is_root() {
        format!("bash -lc {}", shell_single_quote(&script))
    } else {
        format!("sudo -n bash -lc {}", shell_single_quote(&script))
    };
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh_with_stdin(&command, bytes, Duration::from_secs(60))
    })
    .await
    .map_err(|error| {
        AppError::Provisioning(format!(
            "Secure lifecycle capability upload task failed to join: {error}"
        ))
    })?
    .map_err(|error| {
        AppError::Provisioning(format!(
            "Could not securely upload lifecycle capability: {error}"
        ))
    })?;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed securely staging lifecycle capability: {}",
            concise_remote_failure(&output)
        )));
    }
    Ok(())
}

async fn cleanup_configuration_staging(
    remote: &RemoteExec,
    remote_config: &str,
    root_staging: &str,
) {
    let script = format!(
        "rm -f -- {}; rm -rf -- {}",
        shell_single_quote(remote_config),
        shell_single_quote(root_staging)
    );
    if let Err(error) = run_root_script(remote, script, Duration::from_secs(15)).await {
        warn!(%error, "Could not clean lifecycle configuration staging");
    }
}

async fn cleanup_remote_uploads(remote: &RemoteExec, paths: &[&str]) {
    let command = format!(
        "rm -f -- {}",
        paths
            .iter()
            .map(|path| shell_single_quote(path))
            .collect::<Vec<_>>()
            .join(" ")
    );
    if let Err(error) = run_root_script(remote, command, Duration::from_secs(15)).await {
        warn!(%error, "Could not clean randomized lifecycle-agent upload paths");
    }
}

async fn run_root_script(
    remote: &RemoteExec,
    script: String,
    timeout: Duration,
) -> AppResult<ExecOutput> {
    let command = if remote.is_root() {
        format!("bash -lc {}", shell_single_quote(&script))
    } else {
        format!("sudo -n bash -lc {}", shell_single_quote(&script))
    };
    let remote = remote.clone();
    tokio::task::spawn_blocking(move || remote.ssh(&command, timeout))
        .await
        .map_err(|error| {
            AppError::Provisioning(format!(
                "Remote lifecycle-agent task failed to join: {error}"
            ))
        })?
        .map_err(|error| {
            AppError::Provisioning(format!(
                "Could not execute remote lifecycle-agent command: {error}"
            ))
        })
}

async fn upload_file(
    remote: &RemoteExec,
    local_path: PathBuf,
    remote_path: &str,
    timeout: Duration,
    description: &'static str,
) -> AppResult<()> {
    let remote = remote.clone();
    let remote_path = remote_path.to_string();
    let output =
        tokio::task::spawn_blocking(move || remote.scp(&local_path, &remote_path, timeout))
            .await
            .map_err(|error| {
                AppError::Provisioning(format!("{description} upload task failed to join: {error}"))
            })?
            .map_err(|error| {
                AppError::Provisioning(format!("Could not upload {description}: {error}"))
            })?;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed uploading {description}: {}",
            concise_remote_failure(&output)
        )));
    }
    Ok(())
}

struct LocalStagingDir {
    path: PathBuf,
}

impl LocalStagingDir {
    fn create(prefix: &str) -> AppResult<Self> {
        let path = env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4().simple()));
        fs::create_dir(&path).map_err(|error| {
            AppError::Provisioning(format!(
                "Could not create private lifecycle-agent staging directory: {error}"
            ))
        })?;
        set_owner_only_dir_permissions(&path)?;
        Ok(Self { path })
    }

    fn write_private(&self, name: &str, contents: &[u8]) -> AppResult<PathBuf> {
        let path = self.path.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|error| {
            AppError::Provisioning(format!(
                "Could not create private lifecycle-agent staging file: {error}"
            ))
        })?;
        file.write_all(contents).map_err(|error| {
            AppError::Provisioning(format!(
                "Could not write private lifecycle-agent staging file: {error}"
            ))
        })?;
        file.sync_all().map_err(|error| {
            AppError::Provisioning(format!(
                "Could not flush private lifecycle-agent staging file: {error}"
            ))
        })?;
        set_owner_only_file_permissions(&path)?;
        Ok(path)
    }
}

impl Drop for LocalStagingDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(%error, "Could not remove private lifecycle-agent staging directory");
            }
        }
    }
}

fn materialize_installer(staging: &LocalStagingDir) -> AppResult<PathBuf> {
    if !INSTALLER_TEMPLATE.contains("__NOLAND_LIFECYCLE_UNIT__")
        || !SYSTEMD_UNIT.contains("__NOLAND_TARGET_GROUP__")
    {
        return Err(AppError::Provisioning(
            "Bundled lifecycle-agent deployment assets have missing placeholders".to_string(),
        ));
    }
    let installer = INSTALLER_TEMPLATE
        .replace("__NOLAND_LIFECYCLE_UNIT__", SYSTEMD_UNIT.trim_end())
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    staging.write_private("install-lifecycle-agent.sh", installer.as_bytes())
}

fn validate_target_user(target_user: &str) -> AppResult<()> {
    let bytes = target_user.as_bytes();
    let valid_first = bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_lowercase() || *byte == b'_');
    let valid_rest = bytes.iter().skip(1).enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(*byte, b'_' | b'-')
            || (*byte == b'$' && index + 2 == bytes.len())
    });
    if !valid_first || !valid_rest {
        return Err(AppError::InvalidInput(
            "Lifecycle-agent target user has an invalid account name".to_string(),
        ));
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn file_sha256(path: &Path) -> AppResult<String> {
    let mut file = File::open(path).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not open lifecycle-agent deployment asset for hashing: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            AppError::Provisioning(format!(
                "Could not hash lifecycle-agent deployment asset: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(unix)]
fn set_owner_only_dir_permissions(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not secure lifecycle-agent staging directory: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn set_owner_only_dir_permissions(_path: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file_permissions(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not secure lifecycle-agent staging file: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn set_owner_only_file_permissions(_path: &Path) -> AppResult<()> {
    Ok(())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn concise_remote_failure(output: &ExecOutput) -> String {
    let combined = format!("{}\n{}", output.stderr.trim(), output.stdout.trim());
    let mut lines = combined
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(80)
        .collect::<Vec<_>>();
    lines.reverse();
    if lines.is_empty() {
        format!("remote command exited with status {}", output.status_code)
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    #[test]
    fn generated_config_matches_daemon_schema() {
        let config = LifecycleConfig::from_settings(
            true,
            42,
            1.25,
            5,
            "https://console.vast.ai".to_string(),
        )
        .unwrap();
        let value = serde_json::to_value(config).unwrap();

        assert_eq!(value["enabled"], true);
        assert_eq!(value["instanceId"], 42);
        assert_eq!(value["inactivitySeconds"], 4500);
        assert_eq!(value["backupAppLimit"], 5);
        let controller_dead_zone = value["controllerDeadZone"].as_f64().unwrap();
        assert!((controller_dead_zone - 0.15).abs() < 0.000_001);
        assert_eq!(value["stateAgentSocket"], "/run/noland/state-agent.sock");
        assert_eq!(value["statusSocket"], STATUS_SOCKET);
        assert_eq!(value["activitySocket"], "/run/noland/sunshine-events.sock");
        assert_eq!(
            value["databasePath"],
            "/var/lib/noland/lifecycle/runtime.db"
        );
        assert_eq!(value["capabilityPath"], CAPABILITY_PATH);
        assert_eq!(value["vastBaseUrl"], "https://console.vast.ai");
        assert_eq!(value["providerAction"], "destroy");
    }

    #[test]
    fn generated_capability_matches_daemon_schema() {
        let capability = StorageCapability {
            version: 1,
            instance_id: 42,
            profile_id: "profile-1".to_string(),
            expires_at: Utc.timestamp_opt(2_000_000_000, 0).unwrap(),
            session: EphemeralRcloneSession {
                operation_id: "seed".to_string(),
                provider: "s3".to_string(),
                backend_type: "s3".to_string(),
                remote_name: "remote".to_string(),
                root: "bucket/prefix".to_string(),
                config_ini: "[remote]\ntype = s3\n".to_string(),
                expires_at_unix: 2_000_000_000,
                upload_concurrency: Some(4),
            },
            master_key_hex: "ab".repeat(32),
            provider: VastCapability {
                kind: "vast",
                api_key: "secret".to_string(),
                instance_id: 42,
                action: "destroy",
            },
        };
        let value = serde_json::to_value(&capability).unwrap();

        assert_eq!(value["version"], 1);
        assert_eq!(value["instanceId"], 42);
        assert_eq!(value["profileId"], "profile-1");
        assert_eq!(value["masterKeyHex"], "ab".repeat(32));
        assert_eq!(value["provider"]["kind"], "vast");
        assert_eq!(value["provider"]["apiKey"], "secret");
        assert_eq!(value["provider"]["instanceId"], 42);
        assert_eq!(value["provider"]["action"], "destroy");
        assert_eq!(value["session"]["operation_id"], "seed");
        assert_eq!(value["session"]["config_ini"], "[remote]\ntype = s3\n");
    }

    #[test]
    fn status_serde_matches_rpc_envelope() {
        let envelope = json!({
            "id": "request-1",
            "result": {
                "enabled": true,
                "state": "MONITORING/IDLE",
                "lastActivityAt": "2026-09-16T12:00:00Z",
                "idleDurationMs": 1000,
                "timeoutMs": 3600000,
                "timeRemainingMs": 3599000,
                "timeoutReachedAt": null,
                "activeRunId": null,
                "rankedApps": [{
                    "appId": "game",
                    "foregroundActiveMs": 5000,
                    "processRuntimeMs": 7000,
                    "launchCount": 2,
                    "lastActiveAt": "2026-09-16T11:59:00Z"
                }],
                "lastError": null
            },
            "error": null
        });
        let parsed: RpcEnvelope<LifecycleAgentStatus> = serde_json::from_value(envelope).unwrap();
        let status = parsed.result.unwrap();

        assert!(status.enabled);
        assert_eq!(status.state, "MONITORING/IDLE");
        assert_eq!(status.ranked_apps[0].app_id, "game");
        let serialized = serde_json::to_value(status).unwrap();
        assert_eq!(serialized["idleDurationMs"], 1000);
        assert_eq!(serialized["rankedApps"][0]["launchCount"], 2);
    }
}
