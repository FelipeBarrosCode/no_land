use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{header, StatusCode, Url};
use serde::Deserialize;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use crate::services::{app_context::AppContext, remote_exec::RemoteExec};

/// Bundle Indexer service.
///
/// Generates a bundle-index.json on the provisioned VM by running an
/// embedded Python script over SSH. The resulting JSON is written to
/// /var/lib/noland/bundle-index.json on the VM and then uploaded to
/// the remote backup storage via rclone.
pub struct BundleIndexer;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct B2AuthorizeResponse {
    authorization_token: String,
    download_url: String,
}

impl BundleIndexer {
    /// Generate the bundle index on the VM and upload it to remote storage.
    pub async fn generate_and_upload(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
        // Ensure the local directory exists
        let mkdir_cmd = "sudo mkdir -p /var/lib/noland && sudo chmod 755 /var/lib/noland";
        let mkdir_out = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(mkdir_cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if mkdir_out.status_code != 0 {
            warn!("mkdir /var/lib/noland warn: {}", mkdir_out.stderr.trim());
        }

        // Write the indexer script to the VM
        let script_path = "/tmp/noland_bundle_indexer.py";
        let write_script_cmd = format!(
            "cat > {} <<'PYTHON_EOF'\n{}\nPYTHON_EOF\nchmod +x {}",
            script_path,
            INDEXER_PYTHON_SCRIPT,
            script_path
        );
        let write_out = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&write_script_cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if write_out.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to write bundle indexer script: {}",
                write_out.stderr.trim()
            )));
        }

        // Run the indexer
        let index_path = "/var/lib/noland/bundle-index.json";
        let run_cmd = format!(
            "sudo python3 {} --user {} --instance-id {} --output {}",
            script_path, target_user, instance_id, index_path
        );
        let run_out = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&run_cmd, Duration::from_secs(120)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if run_out.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Bundle indexer failed: {}",
                run_out.stderr.trim()
            )));
        }

        info!("Bundle index generated on VM at {}", index_path);

        // Upload to remote storage (always plain B2 remote for client-side index reads)
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;
        if settings.enabled && !settings.backblaze_application_key.is_empty() {
            let remote_dest = format!(
                "{}:{}/{}/metadata/bundle-index.json",
                settings.remote_name, settings.bucket_name, settings.destination_prefix
            );
            let upload_cmd = format!(
                "sudo -u {user} rclone copyto {src} {dest} --checksum 2>&1",
                user = target_user,
                src = shell_escape(index_path),
                dest = shell_escape(&remote_dest),
            );
            let upload_out = {
                let r = remote.clone();
                tokio::task::spawn_blocking(move || r.ssh(&upload_cmd, Duration::from_secs(120)))
                    .await
                    .map_err(|e| AppError::Command(format!("join failure: {e}")))??
            };
            if upload_out.status_code != 0 {
                return Err(AppError::Provisioning(format!(
                    "Bundle index upload failed: {}",
                    upload_out.stderr.trim()
                )));
            }
            info!("Bundle index uploaded to {}", remote_dest);
        }

        Ok(())
    }

    /// Read the bundle index directly from Backblaze B2.
    ///
    /// This intentionally avoids SSH/VM dependency so the client can browse
    /// available bundles directly from shared storage metadata.
    pub async fn read_from_remote(
        context: &AppContext,
        _remote: &RemoteExec,
        instance_id: u64,
        _target_user: &str,
    ) -> AppResult<crate::models::app_state::BundleIndex> {
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            return Err(AppError::Provisioning(
                "Shared storage backup is not enabled. Configure settings first.".to_string(),
            ));
        }

        let auth = Self::authorize_b2(settings).await?;
        let index_url = Self::build_b2_index_url(&auth.download_url, settings)?;

        let client = reqwest::Client::new();
        let response = client
            .get(index_url)
            .header(header::AUTHORIZATION, auth.authorization_token)
            .send()
            .await
            .map_err(|e| AppError::Provisioning(format!("Failed to read bundle index from B2: {e}")))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(AppError::NotFound(
                "No restore index available yet. Run backup first.".to_string(),
            ));
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Provisioning(format!(
                "Failed to read bundle index from B2 ({}): {}",
                status,
                body.trim()
            )));
        }

        let json_str = response
            .text()
            .await
            .map_err(|e| AppError::Provisioning(format!("Failed reading bundle index body: {e}")))?;
        let json_str = json_str.trim();
        if json_str.is_empty() {
            return Err(AppError::NotFound(
                "No restore index available yet. Run backup first.".to_string(),
            ));
        }

        let index: crate::models::app_state::BundleIndex =
            serde_json::from_str(json_str).map_err(|e| {
                AppError::Serialization(format!(
                    "Failed to parse bundle index: {e}. Raw: {}",
                    &json_str[..json_str.len().min(200)]
                ))
            })?;

        if index.instance_id != instance_id {
            warn!(
                "Bundle index instance_id mismatch: expected {}, got {}",
                instance_id, index.instance_id
            );
        }

        Ok(index)
    }

    async fn authorize_b2(
        settings: &crate::models::app_state::SharedStorageSettings,
    ) -> AppResult<B2AuthorizeResponse> {
        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            return Err(AppError::Provisioning(
                "Backblaze credentials are missing. Configure shared storage first.".to_string(),
            ));
        }

        let credentials = format!(
            "{}:{}",
            settings.backblaze_key_id, settings.backblaze_application_key
        );
        let auth_header = format!("Basic {}", STANDARD.encode(credentials));

        let response = reqwest::Client::new()
            .get("https://api.backblazeb2.com/b2api/v2/b2_authorize_account")
            .header(header::AUTHORIZATION, auth_header)
            .send()
            .await
            .map_err(|e| AppError::Provisioning(format!("Backblaze authorization failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Provisioning(format!(
                "Backblaze authorization failed ({}): {}",
                status,
                body.trim()
            )));
        }

        response
            .json::<B2AuthorizeResponse>()
            .await
            .map_err(|e| AppError::Serialization(format!("Failed to parse Backblaze auth response: {e}")))
    }

    fn build_b2_index_url(
        download_url: &str,
        settings: &crate::models::app_state::SharedStorageSettings,
    ) -> AppResult<Url> {
        let mut url = Url::parse(download_url)
            .map_err(|e| AppError::InvalidInput(format!("Invalid Backblaze download URL: {e}")))?;

        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| AppError::InvalidInput("Backblaze download URL does not support path segments".to_string()))?;
            segments.push("file");
            segments.push(&settings.bucket_name);
            for seg in settings.destination_prefix.split('/').filter(|s| !s.trim().is_empty()) {
                segments.push(seg);
            }
            segments.push("metadata");
            segments.push("bundle-index.json");
        }

        Ok(url)
    }
}

fn shell_escape(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

/// Self-contained Python bundle indexer script.
const INDEXER_PYTHON_SCRIPT: &str = r#"#!/usr/bin/env python3
"""Noland Connect Bundle Indexer MVP

Scans an Ubuntu home directory and generates a bundle-index.json that
groups user files into app bundles, project bundles, and folder bundles.
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

# ------------------------------------------------------------------
# Alias map
# ------------------------------------------------------------------
ALIAS_MAP = {
    "code": "vscode",
    "visual-studio-code": "vscode",
    "com-visualstudio-code": "vscode",
    "discordapp-discord": "discord",
    "com-discordapp-discord": "discord",
    "bravesoftware": "brave",
    "google-chrome": "chrome",
}

# ------------------------------------------------------------------
# Name normalisation
# ------------------------------------------------------------------
COMMON_EXTS = (".desktop", ".appimage", ".deb", ".zip", ".tar.gz",
               ".tar.bz2", ".tar.xz", ".7z", ".rar", ".tgz")


def normalize_name(raw: str) -> str:
    s = raw.strip().lower()
    # Remove common extensions
    for ext in sorted(COMMON_EXTS, key=len, reverse=True):
        if s.endswith(ext):
            s = s[: -len(ext)]
            break
    # Remove obvious version suffixes like -4.2, _v4.2, .4.2
    s = re.sub(r"[-_.]v?\d+(\.\d+)*(-stable|-beta|-alpha)?$", "", s)
    # Replace spaces/underscores/dots with hyphens
    s = re.sub(r"[\s_.]+", "-", s)
    # Remove common prefixes com-, org-, io- when useful
    s = re.sub(r"^(com|org|io|net|app)-", "", s)
    s = s.strip("-")
    # Apply alias map
    return ALIAS_MAP.get(s, s)


# ------------------------------------------------------------------
# Desktop file parser
# ------------------------------------------------------------------
def parse_desktop_file(path: Path) -> dict:
    data = {"name": None, "exec": None, "icon": None,
            "categories": None, "startup_wm_class": None}
    try:
        text = path.read_text(encoding="utf-8", errors="ignore")
    except Exception:
        return data
    for line in text.splitlines():
        line = line.strip()
        if line.lower().startswith("name=") and data["name"] is None:
            data["name"] = line.split("=", 1)[1].strip()
        elif line.lower().startswith("exec="):
            data["exec"] = line.split("=", 1)[1].strip()
        elif line.lower().startswith("icon="):
            data["icon"] = line.split("=", 1)[1].strip()
        elif line.lower().startswith("categories="):
            data["categories"] = line.split("=", 1)[1].strip()
        elif line.lower().startswith("startupwmclass="):
            data["startup_wm_class"] = line.split("=", 1)[1].strip()
    return data


# ------------------------------------------------------------------
# Detectors
# ------------------------------------------------------------------
def detect_launchers(home: Path, user: str) -> list:
    """Detect .desktop launchers."""
    results = []
    user_apps = home / ".local" / "share" / "applications"
    system_apps = Path("/usr/share/applications")

    for src_dir, is_user in [(user_apps, True), (system_apps, False)]:
        if not src_dir.exists():
            continue
        for f in src_dir.iterdir():
            if not f.is_file() or not f.suffix == ".desktop":
                continue
            info = parse_desktop_file(f)
            name = info.get("name") or f.stem
            norm = normalize_name(name)
            rel_source = f"home/{user}/.local/share/applications/{f.name}" if is_user else f"usr/share/applications/{f.name}"
            target = f"/home/{user}/.local/share/applications/{f.name}" if is_user else str(f)
            results.append({
                "normalized_key": norm,
                "name": name,
                "signal": "desktop_launcher",
                "source": rel_source,
                "target": target,
                "kind": "file",
                "default_selected": is_user,
                "is_user": is_user,
                "confidence_boost": 0.30,
            })
    return results


def detect_config_folders(home: Path, user: str) -> list:
    """Detect ~/.config/* folders."""
    results = []
    config_dir = home / ".config"
    if not config_dir.exists():
        return results
    for entry in config_dir.iterdir():
        if not entry.is_dir():
            continue
        norm = normalize_name(entry.name)
        results.append({
            "normalized_key": norm,
            "name": entry.name,
            "signal": "config_folder",
            "source": f"home/{user}/.config/{entry.name}",
            "target": f"/home/{user}/.config/{entry.name}",
            "kind": "folder",
            "default_selected": True,
            "confidence_boost": 0.25,
        })
    return results


def detect_local_share(home: Path, user: str) -> list:
    """Detect ~/.local/share/* folders (excluding applications)."""
    results = []
    share_dir = home / ".local" / "share"
    if not share_dir.exists():
        return results
    for entry in share_dir.iterdir():
        if not entry.is_dir():
            continue
        if entry.name == "applications":
            continue
        norm = normalize_name(entry.name)
        results.append({
            "normalized_key": norm,
            "name": entry.name,
            "signal": "local_share_folder",
            "source": f"home/{user}/.local/share/{entry.name}",
            "target": f"/home/{user}/.local/share/{entry.name}",
            "kind": "folder",
            "default_selected": True,
            "confidence_boost": 0.25,
        })
    return results


def detect_downloaded_apps(home: Path, user: str) -> list:
    """Detect AppImages, .deb, archives under ~/Applications and ~/Downloads."""
    results = []
    app_exts = {".appimage", ".deb", ".tar.gz", ".tgz", ".zip", ".7z", ".rar"}

    for scan_dir in [home / "Applications", home / "Downloads"]:
        if not scan_dir.exists():
            continue
        for entry in scan_dir.iterdir():
            if entry.is_file():
                if entry.suffix.lower() in app_exts or any(str(entry).lower().endswith(e) for e in app_exts):
                    norm = normalize_name(entry.name)
                    rel = os.path.relpath(entry, home)
                    results.append({
                        "normalized_key": norm,
                        "name": entry.name,
                        "signal": "downloaded_app",
                        "source": f"home/{user}/{rel}",
                        "target": str(entry),
                        "kind": "file",
                        "default_selected": True,
                        "confidence_boost": 0.20,
                    })
            elif entry.is_dir():
                # Heuristic: folder contains an executable or looks like an extracted app
                has_exec = any(
                    (entry / child).stat().st_mode & 0o111 != 0
                    for child in os.listdir(entry)
                    if (entry / child).is_file()
                ) if entry.exists() else False
                if has_exec:
                    norm = normalize_name(entry.name)
                    rel = os.path.relpath(entry, home)
                    results.append({
                        "normalized_key": norm,
                        "name": entry.name,
                        "signal": "application_folder",
                        "source": f"home/{user}/{rel}",
                        "target": str(entry),
                        "kind": "folder",
                        "default_selected": True,
                        "confidence_boost": 0.20,
                    })
    return results


def detect_projects(home: Path, user: str) -> list:
    """Detect project folders under common project directories."""
    results = []
    project_roots = ["Projects", "Code", "Workspace", "Games", "Documents"]
    for root_name in project_roots:
        root = home / root_name
        if not root.exists():
            continue
        for entry in root.iterdir():
            if not entry.is_dir():
                continue
            norm = normalize_name(entry.name)
            rel = os.path.relpath(entry, home)
            is_git = (entry / ".git").is_dir()
            signals = ["project_folder"]
            if is_git:
                signals.append("git_repo")
            results.append({
                "normalized_key": norm,
                "name": entry.name,
                "signal": "project_folder",
                "source": f"home/{user}/{rel}",
                "target": str(entry),
                "kind": "folder",
                "default_selected": True,
                "confidence_boost": 0.30 + (0.15 if is_git else 0),
                "signals": signals,
            })
    return results


# ------------------------------------------------------------------
# Bundle merging + scoring
# ------------------------------------------------------------------
def merge_bundles(partials: list) -> list:
    groups = {}
    for p in partials:
        key = p["normalized_key"]
        if key not in groups:
            groups[key] = []
        groups[key].append(p)

    bundles = []
    for key, items in groups.items():
        signals = set()
        folder_bundles = []
        total_confidence = 0.0
        name = key.replace("-", " ").title()

        # Prefer name from desktop launcher or downloaded app
        for item in items:
            if item["signal"] == "desktop_launcher":
                name = item["name"]
                break
            elif item["signal"] == "downloaded_app":
                name = item["name"]

        for item in items:
            signals.add(item["signal"])
            total_confidence += item.get("confidence_boost", 0.0)
            folder_bundles.append({
                "id": item["signal"].replace("_", "_"),
                "label": label_for_signal(item["signal"]),
                "source": item["source"],
                "target": item["target"],
                "kind": item["kind"],
                "default_selected": item.get("default_selected", True),
            })

        confidence = min(0.99, total_confidence + (0.10 if key in ALIAS_MAP else 0.0))
        bundle_type = "app"
        if "project_folder" in signals and "desktop_launcher" not in signals:
            bundle_type = "project"
        elif "downloaded_app" in signals and "desktop_launcher" not in signals:
            bundle_type = "download"

        bundles.append({
            "id": f"{bundle_type}.{key}",
            "name": name,
            "type": bundle_type,
            "confidence": round(confidence, 2),
            "signals": sorted(signals),
            "folder_bundles": folder_bundles,
        })

    return bundles


def label_for_signal(signal: str) -> str:
    mapping = {
        "desktop_launcher": "Launcher",
        "config_folder": "Settings",
        "local_share_folder": "App data",
        "downloaded_app": "Application binary",
        "application_folder": "Application folder",
        "project_folder": "Project folder",
    }
    return mapping.get(signal, signal.replace("_", " ").title())


# ------------------------------------------------------------------
# Main
# ------------------------------------------------------------------
def main():
    parser = argparse.ArgumentParser(description="Noland Bundle Indexer")
    parser.add_argument("--user", required=True)
    parser.add_argument("--instance-id", required=True, type=int)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    home = Path(f"/home/{args.user}")
    if not home.exists():
        print(f"Home directory {home} does not exist", file=sys.stderr)
        sys.exit(1)

    partials = []
    partials.extend(detect_launchers(home, args.user))
    partials.extend(detect_config_folders(home, args.user))
    partials.extend(detect_local_share(home, args.user))
    partials.extend(detect_downloaded_apps(home, args.user))
    partials.extend(detect_projects(home, args.user))

    bundles = merge_bundles(partials)
    # Sort by confidence desc, then name
    bundles.sort(key=lambda b: (-b["confidence"], b["name"].lower()))

    index = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "instance_id": args.instance_id,
        "snapshot_id": "latest",
        "host": {
            "username": args.user,
            "home": str(home),
            "os": "ubuntu",
        },
        "bundles": bundles,
    }

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(index, f, indent=2)

    print(f"Bundle index written to {out_path} ({len(bundles)} bundles)")


if __name__ == "__main__":
    main()
"#;
