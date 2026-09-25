//! Deploy and start the standalone network-quality agent on a remote instance.

use std::{
    collections::{HashSet, VecDeque},
    env,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

use super::remote_exec::{ExecOutput, RemoteExec};

const INSTALL_SCRIPT_TEMPLATE: &str =
    include_str!("../../../network-agent/scripts/install-remote.sh");
const SYSTEMD_UNIT: &str =
    include_str!("../../../network-agent/systemd/noland-network-agent.service");
const WG0_WRAPPER: &str = include_str!("../../../network-agent/scripts/run-on-wg0.sh");

const LOCAL_ARCHIVE_NAME: &str = "noland-network-agent-src.tgz";
const LOCAL_INSTALLER_NAME: &str = "noland-network-agent-install.sh";
const AGENT_BINARY_PATH: &str = "/usr/local/bin/noland-network-agent";
const AGENT_SERVICE: &str = "noland-network-agent.service";
const AGENT_REVISION_PATH: &str = "/usr/local/share/noland-network-agent/install-revision";
const DEPLOYMENT_REVISION: &str = "1";
const NETWORK_AGENT_MANIFEST: &str = include_str!("../../../network-agent/Cargo.toml");

static DEPLOYMENT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct NetworkAgentProvisioner;

impl NetworkAgentProvisioner {
    /// Ensure the standalone network agent is installed and enabled on the remote VM.
    pub async fn ensure(remote: &RemoteExec) -> AppResult<()> {
        let _deployment_guard = DEPLOYMENT_LOCK.lock().await;
        let expected_version = expected_agent_version()?;
        let expected_revision = format!("{expected_version}-{DEPLOYMENT_REVISION}");
        if probe_existing(remote, &expected_version, &expected_revision).await? {
            info!(service = AGENT_SERVICE, "Remote network agent is ready");
            return Ok(());
        }

        warn!(
            service = AGENT_SERVICE,
            expected_version,
            expected_revision,
            "Remote network agent is missing or outdated; building it from bundled source"
        );

        let staging = LocalStagingDir::create()?;
        let source_dir = find_network_agent_source_dir()?;
        let archive_path = create_source_bundle(source_dir, staging.path()).await?;
        let installer_path = create_install_script(staging.path())?;
        let archive_sha256 = file_sha256(&archive_path)?;
        let installer_sha256 = file_sha256(&installer_path)?;

        let deployment_id = Uuid::new_v4().simple().to_string();
        let remote_archive = format!("/tmp/noland-network-agent-src-{deployment_id}.tgz");
        let remote_installer = format!("/tmp/noland-network-agent-install-{deployment_id}.sh");
        let root_staging = format!("/run/noland-network-agent-install-{deployment_id}");
        let root_archive = format!("{root_staging}/source.tgz");
        let root_installer = format!("{root_staging}/install.sh");
        let remote_build = format!("{root_staging}/build");

        info!(
            source_bundle = %archive_path.display(),
            "Uploading standalone network-agent deployment assets"
        );
        upload_file(
            remote,
            archive_path,
            &remote_archive,
            Duration::from_secs(300),
            "network-agent source bundle",
        )
        .await?;
        upload_file(
            remote,
            installer_path,
            &remote_installer,
            Duration::from_secs(60),
            "network-agent installer",
        )
        .await?;

        let install_script = format!(
            r#"set -euo pipefail
install -d -o root -g root -m 0700 {root_staging}
cleanup() {{ rm -rf {root_staging}; rm -f {remote_archive} {remote_installer}; }}
trap cleanup EXIT
install -o root -g root -m 0600 {remote_archive} {root_archive}
install -o root -g root -m 0600 {remote_installer} {root_installer}
rm -f {remote_archive} {remote_installer}
printf '%s  %s\n' {archive_sha256} {root_archive} | sha256sum -c -
printf '%s  %s\n' {installer_sha256} {root_installer} | sha256sum -c -
chmod 0700 {root_installer}
{root_installer} {root_archive} {remote_build} {expected_revision}"#,
            root_staging = shell_single_quote(&root_staging),
            remote_archive = shell_single_quote(&remote_archive),
            remote_installer = shell_single_quote(&remote_installer),
            root_archive = shell_single_quote(&root_archive),
            root_installer = shell_single_quote(&root_installer),
            remote_build = shell_single_quote(&remote_build),
            archive_sha256 = shell_single_quote(&archive_sha256),
            installer_sha256 = shell_single_quote(&installer_sha256),
            expected_revision = shell_single_quote(&expected_revision),
        );
        let output = run_root_script(remote, install_script, Duration::from_secs(30 * 60)).await?;
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed building or installing noland-network-agent on the remote instance: {}",
                concise_remote_failure(&output)
            )));
        }

        if !output.stdout.contains("NOLAND_NETWORK_AGENT_READY") {
            return Err(AppError::Provisioning(format!(
                "Network-agent installer completed without its readiness marker: {}",
                concise_remote_failure(&output)
            )));
        }

        info!(
            service = AGENT_SERVICE,
            expected_version, "Remote network agent installed and started"
        );
        Ok(())
    }
}

async fn probe_existing(
    remote: &RemoteExec,
    expected_version: &str,
    expected_revision: &str,
) -> AppResult<bool> {
    let expected_version_output = format!("noland-network-agent {expected_version}");
    let script = format!(
        r#"installed_version="$({binary} --version 2>/dev/null || true)"
installed_revision="$(cat {revision_path} 2>/dev/null || true)"
if [[ -x {binary} ]] \
    && [[ "$installed_version" == {expected_version} ]] \
    && [[ "$installed_revision" == {expected_revision} ]] \
    && [[ "$(systemctl show --property=LoadState --value {service} 2>/dev/null || true)" == "loaded" ]]; then
    systemctl enable --now {service} >/dev/null
    printf '%s\n' NOLAND_NETWORK_AGENT_READY
else
    printf '%s\n' NOLAND_NETWORK_AGENT_MISSING
fi"#,
        binary = AGENT_BINARY_PATH,
        revision_path = AGENT_REVISION_PATH,
        expected_version = shell_single_quote(&expected_version_output),
        expected_revision = shell_single_quote(expected_revision),
        service = AGENT_SERVICE,
    );
    let output = run_root_script(remote, script, Duration::from_secs(45)).await?;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed checking the existing remote network agent: {}",
            concise_remote_failure(&output)
        )));
    }

    Ok(output.stdout.contains("NOLAND_NETWORK_AGENT_READY"))
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
            AppError::Provisioning(format!("Remote network-agent task failed to join: {error}"))
        })?
        .map_err(|error| {
            AppError::Provisioning(format!(
                "Could not execute remote network-agent command: {error}"
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
            "Failed uploading {description} to the remote instance: {}",
            concise_remote_failure(&output)
        )));
    }

    Ok(())
}

struct LocalStagingDir {
    path: PathBuf,
}

impl LocalStagingDir {
    fn create() -> AppResult<Self> {
        let path =
            env::temp_dir().join(format!("noland-network-agent-{}", Uuid::new_v4().simple()));
        fs::create_dir(&path).map_err(|error| {
            AppError::Provisioning(format!(
                "Could not create private network-agent staging directory {}: {error}",
                path.display()
            ))
        })?;
        set_owner_only_dir_permissions(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LocalStagingDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.path.display(), %error, "Could not remove network-agent staging directory");
            }
        }
    }
}

fn expected_agent_version() -> AppResult<String> {
    let mut in_package = false;
    for line in NETWORK_AGENT_MANIFEST.lines() {
        let line = line.trim();
        if line == "[package]" {
            in_package = true;
            continue;
        }
        if in_package && line.starts_with('[') {
            break;
        }
        if in_package {
            if let Some(version) = line
                .strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
            {
                return Ok(version.to_string());
            }
        }
    }
    Err(AppError::Provisioning(
        "Bundled network-agent Cargo.toml has no package version".to_string(),
    ))
}

fn file_sha256(path: &Path) -> AppResult<String> {
    let mut file = File::open(path).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not open deployment asset {} for hashing: {error}",
            path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            AppError::Provisioning(format!(
                "Could not hash deployment asset {}: {error}",
                path.display()
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
            "Could not secure staging directory {}: {error}",
            path.display()
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
            "Could not secure staging file {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_owner_only_file_permissions(_path: &Path) -> AppResult<()> {
    Ok(())
}

fn network_agent_workspace_root() -> AppResult<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            AppError::Provisioning(
                "Could not resolve the workspace containing network-agent".to_string(),
            )
        })
}

fn find_network_agent_source_dir() -> AppResult<PathBuf> {
    if let Ok(explicit) = env::var("NOLAND_NETWORK_AGENT_SOURCE_DIR") {
        let candidate = PathBuf::from(explicit.trim());
        if is_network_agent_source_dir(&candidate) {
            return Ok(candidate);
        }
    }

    if let Ok(workspace_root) = network_agent_workspace_root() {
        let candidate = workspace_root.join("network-agent");
        if is_network_agent_source_dir(&candidate) {
            return Ok(candidate);
        }
    }

    let relative_candidates = [
        "network-agent",
        "_up_/network-agent",
        "../network-agent",
        "../../network-agent",
        "resources/network-agent",
        "Resources/network-agent",
        "resources/_up_/network-agent",
        "Resources/_up_/network-agent",
        "../resources/network-agent",
        "../Resources/network-agent",
        "../resources/_up_/network-agent",
        "../Resources/_up_/network-agent",
        "usr/lib/noland-connect/resources/network-agent",
        "usr/lib/noland-connect/resources/_up_/network-agent",
        "usr/lib/noland-connect/_up_/network-agent",
        "usr/lib/Noland Connect/resources/network-agent",
        "usr/lib/Noland Connect/resources/_up_/network-agent",
        "usr/lib/Noland Connect/_up_/network-agent",
        "lib/noland-connect/resources/network-agent",
        "lib/noland-connect/resources/_up_/network-agent",
        "lib/noland-connect/_up_/network-agent",
        "lib/Noland Connect/resources/network-agent",
        "lib/Noland Connect/resources/_up_/network-agent",
        "lib/Noland Connect/_up_/network-agent",
        ".local/lib/Noland Connect/_up_/network-agent",
        ".local/lib/noland-connect/_up_/network-agent",
    ];

    for seed in network_agent_source_search_seeds() {
        for relative in relative_candidates {
            let candidate = seed.join(relative);
            if is_network_agent_source_dir(&candidate) {
                return Ok(candidate);
            }
        }
    }

    for seed in network_agent_source_search_seeds() {
        if let Some(candidate) = find_named_dir_recursively(&seed, "network-agent", 6) {
            if is_network_agent_source_dir(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(AppError::Provisioning(
        "Could not find the bundled network-agent source directory. Reinstall this app or set NOLAND_NETWORK_AGENT_SOURCE_DIR to a valid source tree."
            .to_string(),
    ))
}

fn is_network_agent_source_dir(path: &Path) -> bool {
    path.join("Cargo.toml").is_file() && path.join("src").is_dir()
}

fn network_agent_source_search_seeds() -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(resource_dir) = env::var("NOLAND_TAURI_RESOURCE_DIR") {
        let candidate = PathBuf::from(resource_dir.trim());
        if !candidate.as_os_str().is_empty() {
            push_path_ancestors(&candidate, 4, &mut seeds, &mut seen);
        }
    }

    if let Ok(current_exe) = env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            push_path_ancestors(exe_dir, 8, &mut seeds, &mut seen);
        }
    }

    if let Ok(cwd) = env::current_dir() {
        push_path_ancestors(&cwd, 4, &mut seeds, &mut seen);
    }

    if let Ok(workspace_root) = network_agent_workspace_root() {
        push_path_ancestors(&workspace_root, 2, &mut seeds, &mut seen);
    }

    seeds
}

fn push_path_ancestors(
    start: &Path,
    levels: usize,
    output: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    let mut current = Some(start);
    for _ in 0..levels {
        let Some(path) = current else {
            break;
        };
        let candidate = path.to_path_buf();
        if seen.insert(candidate.clone()) {
            output.push(candidate);
        }
        current = path.parent();
    }
}

fn find_named_dir_recursively(root: &Path, wanted: &str, max_depth: usize) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }

    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = HashSet::new();
    while let Some((directory, depth)) = queue.pop_front() {
        if !visited.insert(directory.clone()) {
            continue;
        }

        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = entry.file_name();
            if name.to_string_lossy() == wanted {
                return Some(path);
            }
            if depth < max_depth && name != "target" && name != ".git" && name != "node_modules" {
                queue.push_back((path, depth + 1));
            }
        }
    }

    None
}

async fn create_source_bundle(source_dir: PathBuf, staging_dir: &Path) -> AppResult<PathBuf> {
    let archive_path = staging_dir.join(LOCAL_ARCHIVE_NAME);
    let archive_for_task = archive_path.clone();
    tokio::task::spawn_blocking(move || pack_source_bundle(&source_dir, &archive_for_task))
        .await
        .map_err(|error| {
            AppError::Provisioning(format!(
                "Network-agent source bundle task failed to join: {error}"
            ))
        })??;

    if !archive_path.is_file() {
        return Err(AppError::Provisioning(format!(
            "Network-agent source bundle was not created at {}",
            archive_path.display()
        )));
    }
    set_owner_only_file_permissions(&archive_path)?;

    Ok(archive_path)
}

fn pack_source_bundle(source_dir: &Path, archive_path: &Path) -> AppResult<()> {
    let archive_file = File::options()
        .write(true)
        .create_new(true)
        .open(archive_path)
        .map_err(|error| {
            AppError::Provisioning(format!(
                "Could not create network-agent source bundle {}: {error}",
                archive_path.display()
            ))
        })?;
    let encoder = GzEncoder::new(archive_file, Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let archive_root = Path::new("network-agent");

    archive
        .append_dir(archive_root, source_dir)
        .map_err(|error| archive_error("network-agent source directory", error))?;
    append_archive_file(
        &mut archive,
        &source_dir.join("Cargo.toml"),
        &archive_root.join("Cargo.toml"),
    )?;

    let cargo_lock = source_dir.join("Cargo.lock");
    if cargo_lock.is_file() {
        append_archive_file(&mut archive, &cargo_lock, &archive_root.join("Cargo.lock"))?;
    }

    append_archive_tree(
        &mut archive,
        &source_dir.join("src"),
        &archive_root.join("src"),
    )?;

    let encoder = archive.into_inner().map_err(|error| {
        AppError::Provisioning(format!(
            "Could not finalize network-agent tar archive: {error}"
        ))
    })?;
    encoder.finish().map_err(|error| {
        AppError::Provisioning(format!(
            "Could not finalize network-agent gzip archive: {error}"
        ))
    })?;
    Ok(())
}

fn append_archive_tree<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    archive_path: &Path,
) -> AppResult<()> {
    archive
        .append_dir(archive_path, source)
        .map_err(|error| archive_error(&source.display().to_string(), error))?;

    for entry in fs::read_dir(source).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not read network-agent source directory {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            AppError::Provisioning(format!(
                "Could not read network-agent source entry: {error}"
            ))
        })?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }

        let path = entry.path();
        let bundled_path = archive_path.join(name);
        let file_type = entry.file_type().map_err(|error| {
            AppError::Provisioning(format!(
                "Could not inspect network-agent source entry {}: {error}",
                path.display()
            ))
        })?;
        if file_type.is_dir() {
            append_archive_tree(archive, &path, &bundled_path)?;
        } else if file_type.is_file() {
            append_archive_file(archive, &path, &bundled_path)?;
        }
    }

    Ok(())
}

fn append_archive_file<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    archive_path: &Path,
) -> AppResult<()> {
    archive
        .append_path_with_name(source, archive_path)
        .map_err(|error| archive_error(&source.display().to_string(), error))
}

fn archive_error(item: &str, error: std::io::Error) -> AppError {
    AppError::Provisioning(format!(
        "Could not add {item} to the network-agent source bundle: {error}"
    ))
}

fn create_install_script(staging_dir: &Path) -> AppResult<PathBuf> {
    if !INSTALL_SCRIPT_TEMPLATE.contains("__NOLAND_NETWORK_AGENT_WRAPPER__")
        || !INSTALL_SCRIPT_TEMPLATE.contains("__NOLAND_NETWORK_AGENT_UNIT__")
    {
        return Err(AppError::Provisioning(
            "Bundled network-agent installer is missing deployment placeholders".to_string(),
        ));
    }

    let script = INSTALL_SCRIPT_TEMPLATE
        .replace("__NOLAND_NETWORK_AGENT_WRAPPER__", WG0_WRAPPER.trim_end())
        .replace("__NOLAND_NETWORK_AGENT_UNIT__", SYSTEMD_UNIT.trim_end())
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let path = staging_dir.join(LOCAL_INSTALLER_NAME);
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            AppError::Provisioning(format!(
                "Could not create network-agent installer {}: {error}",
                path.display()
            ))
        })?;
    file.write_all(script.as_bytes()).map_err(|error| {
        AppError::Provisioning(format!(
            "Could not write network-agent installer {}: {error}",
            path.display()
        ))
    })?;
    set_owner_only_file_permissions(&path)?;
    Ok(path)
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
