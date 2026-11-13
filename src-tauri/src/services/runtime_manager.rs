//! Cloud-desktop runtime installed automatically through the provisioning SSH channel.
use super::remote_exec::RemoteExec;
use crate::errors::{AppError, AppResult};
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

const BINARY: &str = "/usr/local/bin/noland-runtime";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub enabled: bool,
    // Kept for compatibility with older clients. Provisioning does not require
    // a separate runtime release, public key, or signing secret.
    pub release_configured: bool,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeApp {
    pub id: String,
    pub name: String,
    pub file: String,
    pub profile: String,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub async fn run(remote: &RemoteExec, user: &str, args: &[&str]) -> AppResult<serde_json::Value> {
    let command = format!(
        "sudo -n -i -u {} -- {} {}",
        quote(user),
        BINARY,
        args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
    );
    let remote = remote.clone();
    let output =
        tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(240)))
            .await
            .map_err(|e| AppError::Command(e.to_string()))??;
    if output.status_code != 0 {
        return Err(AppError::Command(format!(
            "Runtime companion: {}",
            output.stderr.trim()
        )));
    }
    serde_json::from_str(output.stdout.trim())
        .map_err(|e| AppError::Serialization(format!("Invalid runtime response: {e}")))
}

pub async fn status(remote: &RemoteExec, user: &str) -> AppResult<RuntimeStatus> {
    let probe = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        probe.ssh(&format!("test -x {BINARY}"), Duration::from_secs(15))
    })
    .await
    .map_err(|e| AppError::Command(e.to_string()))??;
    let enabled = output.status_code == 0;
    Ok(RuntimeStatus {
        enabled,
        release_configured: true,
        details: if enabled {
            Some(run(remote, user, &["status", "--json"]).await?)
        } else {
            None
        },
    })
}

pub async fn apps(remote: &RemoteExec, user: &str) -> AppResult<Vec<RuntimeApp>> {
    let value = status(remote, user)
        .await?
        .details
        .and_then(|d| d.get("apps").cloned())
        .unwrap_or_else(|| serde_json::json!([]));
    serde_json::from_value(value).map_err(|e| AppError::Serialization(e.to_string()))
}

// Embed the exact locked sources in every client build. No resource-directory
// discovery, published artifact URLs, or source checkout is needed by the user.
const SOURCES: &[(&str, &[u8])] = &[
    (
        "runtime-agent/Cargo.toml",
        include_bytes!("../../../runtime-agent/Cargo.toml"),
    ),
    (
        "runtime-agent/Cargo.lock",
        include_bytes!("../../../runtime-agent/Cargo.lock"),
    ),
    (
        "runtime-agent/src/main.rs",
        include_bytes!("../../../runtime-agent/src/main.rs"),
    ),
    (
        "runtime-agent/src/lib.rs",
        include_bytes!("../../../runtime-agent/src/lib.rs"),
    ),
    (
        "runtime-agent/src/engine.rs",
        include_bytes!("../../../runtime-agent/src/engine.rs"),
    ),
    (
        "runtime-agent/src/session.rs",
        include_bytes!("../../../runtime-agent/src/session.rs"),
    ),
    (
        "runtime-agent/src/elf.rs",
        include_bytes!("../../../runtime-agent/src/elf.rs"),
    ),
    (
        "runtime-agent/src/maintenance.rs",
        include_bytes!("../../../runtime-agent/src/maintenance.rs"),
    ),
    (
        "runtime-agent/src/store.rs",
        include_bytes!("../../../runtime-agent/src/store.rs"),
    ),
    (
        "runtime-agent/src/artifact.rs",
        include_bytes!("../../../runtime-agent/src/artifact.rs"),
    ),
    (
        "runtime-agent/src/gui.py",
        include_bytes!("../../../runtime-agent/src/gui.py"),
    ),
    (
        "runtime-contracts/Cargo.toml",
        include_bytes!("../../../runtime-contracts/Cargo.toml"),
    ),
    (
        "runtime-contracts/src/lib.rs",
        include_bytes!("../../../runtime-contracts/src/lib.rs"),
    ),
    (
        "runtime-images/ubuntu24/packages.txt",
        include_bytes!("../../../runtime-images/ubuntu24/packages.txt"),
    ),
    (
        "runtime-images/ubuntu24/prerequisites.json",
        include_bytes!("../../../runtime-images/ubuntu24/prerequisites.json"),
    ),
    (
        "scripts/runtime/maintain-runtime.py",
        include_bytes!("../../../scripts/runtime/maintain-runtime.py"),
    ),
    (
        "runtime-images/ubuntu24/glibc-probe.c",
        include_bytes!("../../../runtime-images/ubuntu24/glibc-probe.c"),
    ),
    (
        "scripts/runtime/install-cloud-runtime.sh",
        include_bytes!("../../../scripts/runtime/install-cloud-runtime.sh"),
    ),
];

fn source_bundle() -> AppResult<Vec<u8>> {
    let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for (path, bytes) in SOURCES {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, path, *bytes)
            .map_err(|e| AppError::Command(e.to_string()))?;
    }
    archive
        .into_inner()
        .and_then(|gzip| gzip.finish())
        .map_err(|e| AppError::Command(e.to_string()))
}

pub async fn enable(remote: &RemoteExec, user: &str) -> AppResult<()> {
    if user.is_empty()
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err(AppError::InvalidInput("Invalid desktop user".into()));
    }
    let bytes = source_bundle()?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let script = format!(
        r#"set -eu
work=$(mktemp -d /var/tmp/noland-runtime-source.XXXXXXXX)
trap 'rm -rf -- "$work"' EXIT
cat > "$work/source.tgz"
printf '%s  %s\n' {digest} "$work/source.tgz" | sha256sum --check --status
tar -xzf "$work/source.tgz" -C "$work"
bash "$work/scripts/runtime/install-cloud-runtime.sh" {user} {digest}
"#,
        digest = quote(&digest),
        user = quote(user)
    );
    let command = format!(
        "{}sh -c {}",
        if remote.is_root() { "" } else { "sudo -n " },
        quote(&script)
    );
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh_with_stdin(&command, bytes, Duration::from_secs(2700))
    })
    .await
    .map_err(|e| AppError::Command(e.to_string()))??;
    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Cloud runtime setup failed (resume Provisioning to retry): {} {}",
            output.stderr.trim(),
            output.stdout.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_remote_arguments() {
        assert_eq!(
            super::quote("/tmp/a'; $(touch nope)"),
            "'/tmp/a'\"'\"'; $(touch nope)'"
        );
    }

    #[test]
    fn embedded_sources_form_a_complete_locked_workspace_without_signing_configuration() {
        let bytes = super::source_bundle().unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
        let paths = archive
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert!(paths.contains(&"runtime-agent/src/gui.py".into()));
        assert!(paths.contains(&"runtime-agent/Cargo.lock".into()));
        assert!(paths.contains(&"runtime-contracts/src/lib.rs".into()));
        assert!(paths.contains(&"scripts/runtime/install-cloud-runtime.sh".into()));
        assert!(paths
            .iter()
            .all(|path| !path.contains("target/") && !path.contains(".pem")));
    }
}
