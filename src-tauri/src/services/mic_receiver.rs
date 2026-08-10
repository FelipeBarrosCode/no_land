use std::{
    collections::{HashSet, VecDeque},
    env, fs,
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::GzEncoder, Compression};
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::remote_exec::RemoteExec;

const INSTALL_SCRIPT: &str = include_str!("../../scripts/install_cloud_mic_agent.sh");

pub struct MicReceiverProvisioner;

impl MicReceiverProvisioner {
    /// Install and start the Noland microphone receiver on the remote VM.
    pub async fn install(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let normalized_script = INSTALL_SCRIPT.replace("\r\n", "\n").replace('\r', "\n");
        let encoded = STANDARD.encode(normalized_script.as_bytes());
        let safe_user = sanitize_username(target_user)?;
        let source_bundle_path = create_receiver_source_bundle().await?;

        info!(
            target_user = safe_user,
            source_bundle = %source_bundle_path.display(),
            "Installing Noland microphone receiver"
        );

        let upload_output = {
            let r = remote.clone();
            let bundle_path = source_bundle_path.clone();
            tokio::task::spawn_blocking(move || {
                r.scp(
                    &bundle_path,
                    "/tmp/noland-mic-agent-src.tgz",
                    Duration::from_secs(300),
                )
            })
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if upload_output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed uploading mic receiver source bundle to VM: {} {}",
                upload_output.stderr.trim(),
                upload_output.stdout.trim()
            )));
        }

        let build_command = r#"sudo bash -lc 'set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export RUSTUP_HOME=/root/.rustup
export CARGO_HOME=/root/.cargo
export PATH="$CARGO_HOME/bin:$PATH"
apt-get update -y
apt-get install -y build-essential pkg-config cmake curl clang ca-certificates libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev pipewire pipewire-pulse wireplumber gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav pulseaudio-utils
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
fi
rustup toolchain install stable --profile minimal >/tmp/noland-rustup.stdout.log 2>/tmp/noland-rustup.stderr.log || true
rustup default stable >/tmp/noland-rustup-default.stdout.log 2>/tmp/noland-rustup-default.stderr.log
cargo --version >/tmp/noland-cargo-version.log 2>&1
rustc --version >/tmp/noland-rustc-version.log 2>&1
rm -rf /tmp/noland-mic-build
mkdir -p /tmp/noland-mic-build
cd /tmp/noland-mic-build
tar -xzf /tmp/noland-mic-agent-src.tgz 2>/tmp/noland-mic-tar.log
rm -f /tmp/noland-mic-build/vm-cloud-mic-agent/Cargo.lock
if ! cargo build --release --manifest-path /tmp/noland-mic-build/vm-cloud-mic-agent/Cargo.toml >/tmp/noland-mic-build.stdout.log 2>/tmp/noland-mic-build.stderr.log; then
  echo "=== RUSTUP STDOUT ==="
  tail -n 60 /tmp/noland-rustup.stdout.log || true
  echo "=== RUSTUP STDERR ==="
  tail -n 60 /tmp/noland-rustup.stderr.log || true
  echo "=== CARGO VERSION ==="
  cat /tmp/noland-cargo-version.log || true
  echo "=== RUSTC VERSION ==="
  cat /tmp/noland-rustc-version.log || true
  echo "=== TAR STDERR ==="
  tail -n 40 /tmp/noland-mic-tar.log || true
  echo "=== CARGO STDOUT ==="
  tail -n 120 /tmp/noland-mic-build.stdout.log || true
  echo "=== CARGO STDERR ==="
  tail -n 120 /tmp/noland-mic-build.stderr.log || true
  exit 1
fi
cp /tmp/noland-mic-build/vm-cloud-mic-agent/target/release/noland-mic-receiver /tmp/noland-mic-receiver
chmod +x /tmp/noland-mic-receiver'"#;

        let build_output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(build_command, Duration::from_secs(1800)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        if build_output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed building noland-mic-receiver on VM: {} {}",
                build_output.stderr.trim(),
                build_output.stdout.trim()
            )));
        }

        let command = format!(
            "sudo bash -lc 'set -euo pipefail; base64 -d > /tmp/noland-mic-install.sh <<\"EOF\"\n{}\nEOF\nchmod +x /tmp/noland-mic-install.sh\n/tmp/noland-mic-install.sh \"{}\"\nrm -f /tmp/noland-mic-install.sh'",
            encoded, safe_user,
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&command, Duration::from_secs(300)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };

        let stdout = output.stdout.trim();
        let stderr = output.stderr.trim();

        if !stdout.is_empty() {
            info!(
                target_user = safe_user,
                "Mic receiver install stdout:\n{stdout}"
            );
        }
        if !stderr.is_empty() {
            warn!(
                target_user = safe_user,
                "Mic receiver install stderr:\n{stderr}"
            );
        }

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Mic receiver install failed: {stderr} {stdout}"
            )));
        }

        info!(
            target_user = safe_user,
            "Mic receiver installed successfully"
        );
        Ok(stdout.to_string())
    }
}

fn receiver_workspace_root() -> AppResult<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
        AppError::State("Failed to resolve src-tauri workspace parent directory".to_string())
    })
}

fn is_receiver_source_dir(path: &Path) -> bool {
    path.join("Cargo.toml").is_file() && path.join("src").is_dir()
}

fn receiver_source_search_seeds() -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(current_exe) = env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            push_path_ancestors(exe_dir, 6, &mut seeds, &mut seen);
        }
    }

    if let Ok(cwd) = env::current_dir() {
        push_path_ancestors(&cwd, 4, &mut seeds, &mut seen);
    }

    if let Ok(workspace_root) = receiver_workspace_root() {
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
        let owned = path.to_path_buf();
        if seen.insert(owned.clone()) {
            output.push(owned);
        }
        current = path.parent();
    }
}

fn find_receiver_source_dir() -> AppResult<PathBuf> {
    if let Ok(explicit) = env::var("NOLAND_MIC_RECEIVER_SOURCE_DIR") {
        let explicit = PathBuf::from(explicit.trim());
        if is_receiver_source_dir(&explicit) {
            return Ok(explicit);
        }
    }

    if let Ok(workspace_root) = receiver_workspace_root() {
        let workspace_candidate = workspace_root.join("vm-cloud-mic-agent");
        if is_receiver_source_dir(&workspace_candidate) {
            return Ok(workspace_candidate);
        }
    }

    let direct_relative_dirs = [
        "vm-cloud-mic-agent",
        "resources/vm-cloud-mic-agent",
        "Resources/vm-cloud-mic-agent",
        "resources/_up_/vm-cloud-mic-agent",
        "Resources/_up_/vm-cloud-mic-agent",
        "../Resources/vm-cloud-mic-agent",
        "../resources/vm-cloud-mic-agent",
        "../Resources/_up_/vm-cloud-mic-agent",
        "../resources/_up_/vm-cloud-mic-agent",
        "usr/lib/noland-connect/resources/vm-cloud-mic-agent",
        "usr/lib/noland-connect/resources/_up_/vm-cloud-mic-agent",
    ];

    for seed in receiver_source_search_seeds() {
        for relative in direct_relative_dirs {
            let candidate = seed.join(relative);
            if is_receiver_source_dir(&candidate) {
                return Ok(candidate);
            }
        }
    }

    for root in receiver_source_search_seeds() {
        if let Some(found) = find_named_dir_recursively(&root, "vm-cloud-mic-agent", 6) {
            if is_receiver_source_dir(&found) {
                return Ok(found);
            }
        }
    }

    Err(AppError::NotFound(
        "Could not find bundled vm-cloud-mic-agent source directory. Reinstall this app or set NOLAND_MIC_RECEIVER_SOURCE_DIR to a valid source tree."
            .to_string(),
    ))
}

fn find_named_dir_recursively(root: &Path, wanted: &str, max_depth: usize) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }

    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = HashSet::new();

    while let Some((dir, depth)) = queue.pop_front() {
        if !visited.insert(dir.clone()) {
            continue;
        }

        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            if entry.file_name().to_string_lossy() == wanted {
                return Some(path);
            }

            if depth < max_depth {
                queue.push_back((path, depth + 1));
            }
        }
    }

    None
}

async fn create_receiver_source_bundle() -> AppResult<PathBuf> {
    let source_dir = find_receiver_source_dir()?;
    let archive_path = env::temp_dir().join("noland-mic-agent-src.tgz");
    if archive_path.exists() {
        std::fs::remove_file(&archive_path).map_err(|error| {
            AppError::Command(format!(
                "Failed removing old mic source bundle {}: {error}",
                archive_path.display()
            ))
        })?;
    }

    let source_name = source_dir
        .file_name()
        .ok_or_else(|| {
            AppError::State(format!(
                "Failed resolving directory name for mic receiver source bundle at {}",
                source_dir.display()
            ))
        })?
        .to_owned();
    let archive_for_task = archive_path.clone();
    tokio::task::spawn_blocking(move || {
        let output = File::create(&archive_for_task).map_err(|error| {
            AppError::Command(format!(
                "Failed creating mic source bundle {}: {error}",
                archive_for_task.display()
            ))
        })?;
        let encoder = GzEncoder::new(output, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append_source_tree(&mut archive, &source_dir, Path::new(&source_name), true)?;
        let encoder = archive.into_inner().map_err(|error| {
            AppError::Command(format!("Failed finalizing mic source tar archive: {error}"))
        })?;
        encoder.finish().map_err(|error| {
            AppError::Command(format!("Failed finalizing mic source gzip stream: {error}"))
        })?;
        Ok::<(), AppError>(())
    })
    .await
    .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

    if !archive_path.is_file() {
        return Err(AppError::NotFound(format!(
            "Mic receiver source bundle was not created at {}",
            archive_path.display()
        )));
    }

    Ok(archive_path)
}

fn append_source_tree<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    archive_path: &Path,
    source_root: bool,
) -> AppResult<()> {
    archive.append_dir(archive_path, source).map_err(|error| {
        AppError::Command(format!(
            "Failed adding directory {} to mic source bundle: {error}",
            source.display()
        ))
    })?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if source_root && (name == "Cargo.lock" || name == "target") {
            continue;
        }
        let path = entry.path();
        let bundled_path = archive_path.join(&name);
        if path.is_dir() {
            append_source_tree(archive, &path, &bundled_path, false)?;
        } else {
            archive
                .append_path_with_name(&path, &bundled_path)
                .map_err(|error| {
                    AppError::Command(format!(
                        "Failed adding {} to mic source bundle: {error}",
                        path.display()
                    ))
                })?;
        }
    }

    Ok(())
}

fn sanitize_username(value: &str) -> AppResult<&str> {
    if value.is_empty() {
        return Err(AppError::Provisioning("Target user is empty".to_string()));
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(value)
    } else {
        Err(AppError::Provisioning(format!(
            "Invalid target user '{value}': only letters, numbers, '-' and '_' are allowed"
        )))
    }
}
