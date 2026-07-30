use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
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

async fn create_receiver_source_bundle() -> AppResult<PathBuf> {
    let workspace_root = receiver_workspace_root()?;
    let archive_path = env::temp_dir().join("noland-mic-agent-src.tgz");
    if archive_path.exists() {
        std::fs::remove_file(&archive_path).map_err(|error| {
            AppError::Command(format!(
                "Failed removing old mic source bundle {}: {error}",
                archive_path.display()
            ))
        })?;
    }

    let archive_string = archive_path.display().to_string();
    let workspace_string = workspace_root.display().to_string();
    let tar_command = format!(
        "COPYFILE_DISABLE=1 COPY_EXTENDED_ATTRIBUTES_DISABLE=1 tar --no-mac-metadata --no-xattrs -czf '{}' -C '{}' --exclude=vm-cloud-mic-agent/Cargo.lock --exclude=vm-cloud-mic-agent/target vm-cloud-mic-agent",
        archive_string.replace('"', "\\\""),
        workspace_string.replace('"', "\\\"")
    );
    let output = tokio::task::spawn_blocking(move || {
        RemoteExec::run_local(
            "sh",
            &["-c", tar_command.as_str()],
            Duration::from_secs(120),
        )
    })
    .await
    .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed packaging mic receiver source bundle: {} {}",
            output.stderr.trim(),
            output.stdout.trim()
        )));
    }

    if !archive_path.is_file() {
        return Err(AppError::NotFound(format!(
            "Mic receiver source bundle was not created at {}",
            archive_path.display()
        )));
    }

    Ok(archive_path)
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
