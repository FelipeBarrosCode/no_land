use std::time::Duration;

use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use crate::services::{app_context::AppContext, remote_exec::RemoteExec};

use super::shared_storage_manager::SharedStorageManager;

/// Bundle Indexer service.
///
/// Generates a bundle-index.json on the provisioned VM by running an
/// embedded Python script over SSH. The resulting JSON is written to
/// /var/lib/noland/bundle-index.json on the VM and then uploaded to
/// the remote backup storage via rclone.
pub struct BundleIndexer;

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
        info!(
            event = "shared_storage_bundle_index_mkdir_output",
            instance_id = instance_id,
            status_code = mkdir_out.status_code,
            stdout = %mkdir_out.stdout.trim(),
            stderr = %mkdir_out.stderr.trim(),
            "Bundle index mkdir command output"
        );
        if mkdir_out.status_code != 0 {
            warn!("mkdir /var/lib/noland warn: {}", mkdir_out.stderr.trim());
        }

        // Write the indexer script to the VM
        let script_path = "/tmp/noland_bundle_indexer.py";
        let write_script_cmd = format!(
            "cat > {} <<'PYTHON_EOF'\n{}\nPYTHON_EOF\nchmod +x {}",
            script_path, INDEXER_PYTHON_SCRIPT, script_path
        );
        let write_out = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&write_script_cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        info!(
            event = "shared_storage_bundle_index_write_output",
            instance_id = instance_id,
            status_code = write_out.status_code,
            stdout = %write_out.stdout.trim(),
            stderr = %write_out.stderr.trim(),
            "Bundle index script write command output"
        );
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
        info!(
            event = "shared_storage_bundle_index_run_output",
            instance_id = instance_id,
            status_code = run_out.status_code,
            stdout = %run_out.stdout.trim(),
            stderr = %run_out.stderr.trim(),
            "Bundle index generation command output"
        );
        if run_out.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Bundle indexer failed: {}",
                run_out.stderr.trim()
            )));
        }

        info!("Bundle index generated on VM at {}", index_path);

        let active_profile =
            SharedStorageManager::prepare_active_profile_remote(context, remote, target_user)
                .await?;
        let namespace = SharedStorageManager::namespace_for_profile(&active_profile);
        let remote_dest = format!("{}/bundle-index.json", namespace.catalogs_root);
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
        info!(
            event = "shared_storage_bundle_index_upload_output",
            instance_id = instance_id,
            status_code = upload_out.status_code,
            stdout = %upload_out.stdout.trim(),
            stderr = %upload_out.stderr.trim(),
            "Bundle index upload command output"
        );
        if upload_out.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Bundle index upload failed: {}",
                upload_out.stderr.trim()
            )));
        }
        info!("Bundle index uploaded to {}", remote_dest);

        Ok(())
    }

    pub async fn read_from_remote(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<crate::models::app_state::BundleIndex> {
        let active_profile =
            SharedStorageManager::prepare_active_profile_remote(context, remote, target_user)
                .await?;
        let namespace = SharedStorageManager::namespace_for_profile(&active_profile);
        let index_path = format!("{}/bundle-index.json", namespace.catalogs_root);
        let read_cmd = format!(
            "sudo -u {user} rclone cat {src} 2>&1",
            user = target_user,
            src = shell_escape(&index_path),
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&read_cmd, Duration::from_secs(120)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            let stderr = output.stderr.trim();
            let stdout = output.stdout.trim();
            let combined = if stderr.is_empty() { stdout } else { stderr };
            if combined.contains("object not found")
                || combined.contains("directory not found")
                || combined.contains("not found")
            {
                return Err(AppError::NotFound(
                    "No restore index available yet. Run backup/export first.".to_string(),
                ));
            }
            return Err(AppError::Provisioning(format!(
                "Failed to read bundle index from shared storage: {}",
                combined
            )));
        }

        let json_str = output.stdout.trim();
        if json_str.is_empty() {
            return Err(AppError::NotFound(
                "No restore index available yet. Run backup/export first.".to_string(),
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
}

fn shell_escape(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

/// Self-contained Python bundle indexer script.
const INDEXER_PYTHON_SCRIPT: &str = r#"#!/usr/bin/env python3
"""Noland Connect Bundle Indexer

Builds software-centric bundles by grouping a software anchor with the related
paths it writes across the machine.
"""

import argparse
import json
import os
import re
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ALIAS_MAP = {
    "code": "vscode",
    "visual-studio-code": "vscode",
    "com-visualstudio-code": "vscode",
    "discordapp-discord": "discord",
    "com-discordapp-discord": "discord",
    "bravesoftware": "brave",
    "google-chrome": "chrome",
}

COMMON_EXTS = (
    ".desktop",
    ".appimage",
    ".deb",
    ".zip",
    ".tar.gz",
    ".tar.bz2",
    ".tar.xz",
    ".7z",
    ".rar",
    ".tgz",
)


class BundleBuilder:
    def __init__(self, bundle_id: str, name: str, bundle_type: str, normalized_key: str):
        self.id = bundle_id
        self.name = name
        self.bundle_type = bundle_type
        self.normalized_key = normalized_key
        self.signals = set()
        self.folder_bundles = []
        self.seen_targets = set()
        self.confidence = 0.0

    def add_path(self, *, component_id: str, label: str, source: str, target: str, kind: str,
                 signal: str, default_selected: bool = True, confidence_boost: float = 0.0):
        if not target.startswith("/home/"):
            return
        if target in self.seen_targets:
            return
        self.seen_targets.add(target)
        self.signals.add(signal)
        self.confidence += confidence_boost
        self.folder_bundles.append({
            "id": component_id,
            "label": label,
            "source": source,
            "target": target,
            "kind": kind,
            "default_selected": default_selected,
        })

    def to_json(self):
        confidence = min(0.99, max(0.35, self.confidence))
        return {
            "id": self.id,
            "name": self.name,
            "type": self.bundle_type,
            "confidence": round(confidence, 2),
            "signals": sorted(self.signals),
            "folder_bundles": self.folder_bundles,
        }


def normalize_name(raw: str) -> str:
    s = raw.strip().lower()
    for ext in sorted(COMMON_EXTS, key=len, reverse=True):
        if s.endswith(ext):
            s = s[: -len(ext)]
            break
    s = re.sub(r"[-_.]v?\d+(\.\d+)*(-stable|-beta|-alpha)?$", "", s)
    s = re.sub(r"[\s_.]+", "-", s)
    s = re.sub(r"^(com|org|io|net|app)-", "", s)
    s = s.strip("-")
    return ALIAS_MAP.get(s, s)


def tokenize_name(raw: str) -> set:
    normalized = normalize_name(raw)
    parts = [part for part in normalized.split("-") if len(part) > 2]
    tokens = set(parts)
    if normalized:
        tokens.add(normalized)
    return tokens


def parse_desktop_file(path: Path) -> dict:
    data = {"name": None, "exec": None, "icon": None, "categories": None, "startup_wm_class": None}
    try:
        text = path.read_text(encoding="utf-8", errors="ignore")
    except Exception:
        return data
    for line in text.splitlines():
        line = line.strip()
        lower = line.lower()
        if lower.startswith("name=") and data["name"] is None:
            data["name"] = line.split("=", 1)[1].strip()
        elif lower.startswith("exec="):
            data["exec"] = line.split("=", 1)[1].strip()
        elif lower.startswith("icon="):
            data["icon"] = line.split("=", 1)[1].strip()
        elif lower.startswith("categories="):
            data["categories"] = line.split("=", 1)[1].strip()
        elif lower.startswith("startupwmclass="):
            data["startup_wm_class"] = line.split("=", 1)[1].strip()
    return data


def parse_steam_acf(path: Path) -> dict:
    try:
        text = path.read_text(encoding="utf-8", errors="ignore")
    except Exception:
        return {}
    appid = None
    name = None
    for line in text.splitlines():
        parts = re.findall(r'"([^"]+)"', line)
        if len(parts) >= 2:
            if parts[0] == "appid":
                appid = parts[1]
            elif parts[0] == "name":
                name = parts[1]
    return {"appid": appid, "name": name}


def rel_source(path: Path, home: Path, user: str) -> str:
    return f"home/{user}/{os.path.relpath(path, home)}"


def create_component_id(role: str, unique_hint: str = None) -> str:
    suffix = normalize_name(unique_hint or role or "component")
    return f"{role}-{suffix}"


def build_steam_bundles(home: Path, user: str) -> tuple[list, set]:
    steam_root = home / ".local" / "share" / "Steam"
    steamapps = steam_root / "steamapps"
    common_dir = steamapps / "common"
    compatdata_dir = steamapps / "compatdata"
    userdata_dir = steam_root / "userdata"
    shadercache_dir = steamapps / "shadercache"
    workshop_dir = steamapps / "workshop" / "content"
    bundles = []
    used_paths = set()

    if not steamapps.exists():
        return bundles, used_paths

    for manifest in sorted(steamapps.glob("appmanifest_*.acf")):
        meta = parse_steam_acf(manifest)
        appid = meta.get("appid")
        name = meta.get("name")
        if not appid or not name:
            continue
        normalized = normalize_name(name)
        bundle = BundleBuilder(f"steam.{appid}", name, "game", normalized)
        bundle.add_path(
            component_id="steam-manifest",
            label="Steam manifest",
            source=rel_source(manifest, home, user),
            target=str(manifest),
            kind="file",
            signal="steam_manifest",
            default_selected=True,
            confidence_boost=0.25,
        )
        used_paths.add(str(manifest))

        install_dir = common_dir / name
        if install_dir.exists():
            bundle.add_path(
                component_id="install-content",
                label="Game install",
                source=rel_source(install_dir, home, user),
                target=str(install_dir),
                kind="folder",
                signal="steam_install",
                default_selected=True,
                confidence_boost=0.35,
            )
            used_paths.add(str(install_dir))

        compat_dir = compatdata_dir / appid
        if compat_dir.exists():
            bundle.add_path(
                component_id="runtime-prefix",
                label="Proton runtime",
                source=rel_source(compat_dir, home, user),
                target=str(compat_dir),
                kind="runtime-prefix",
                signal="steam_compatdata",
                default_selected=True,
                confidence_boost=0.30,
            )
            used_paths.add(str(compat_dir))

        shader_dir = shadercache_dir / appid
        if shader_dir.exists():
            bundle.add_path(
                component_id="shader-cache",
                label="Shader cache",
                source=rel_source(shader_dir, home, user),
                target=str(shader_dir),
                kind="shader-cache",
                signal="steam_shadercache",
                default_selected=True,
                confidence_boost=0.18,
            )
            used_paths.add(str(shader_dir))

        workshop_app_dir = workshop_dir / appid
        if workshop_app_dir.exists():
            bundle.add_path(
                component_id="workshop-content",
                label="Workshop content",
                source=rel_source(workshop_app_dir, home, user),
                target=str(workshop_app_dir),
                kind="mods",
                signal="steam_workshop",
                default_selected=True,
                confidence_boost=0.18,
            )
            used_paths.add(str(workshop_app_dir))

        if userdata_dir.exists():
            for user_dir in userdata_dir.iterdir():
                app_userdata = user_dir / appid
                if app_userdata.exists():
                    bundle.add_path(
                        component_id=create_component_id("steam-userdata", f"{appid}-{user_dir.name}"),
                        label="Steam userdata",
                        source=rel_source(app_userdata, home, user),
                        target=str(app_userdata),
                        kind="data",
                        signal="steam_userdata",
                        default_selected=True,
                        confidence_boost=0.22,
                    )
                    used_paths.add(str(app_userdata))

        bundles.append({
            "builder": bundle,
            "tokens": tokenize_name(name) | {appid},
        })

    return bundles, used_paths


def build_desktop_anchors(home: Path, user: str) -> tuple[list, set]:
    user_apps = home / ".local" / "share" / "applications"
    bundles = []
    used_paths = set()
    if not user_apps.exists():
        return bundles, used_paths

    for desktop_file in sorted(user_apps.glob("*.desktop")):
        info = parse_desktop_file(desktop_file)
        name = info.get("name") or desktop_file.stem
        normalized = normalize_name(name)
        if not normalized:
            continue
        bundle = BundleBuilder(f"app.{normalized}", name, "app", normalized)
        bundle.add_path(
            component_id="launcher",
            label="Launcher",
            source=rel_source(desktop_file, home, user),
            target=str(desktop_file),
            kind="file",
            signal="desktop_launcher",
            default_selected=True,
            confidence_boost=0.35,
        )
        used_paths.add(str(desktop_file))
        extra_tokens = set()
        for value in [desktop_file.stem, info.get("icon"), info.get("startup_wm_class"), info.get("exec")]:
            if value:
                extra_tokens |= tokenize_name(str(value))
        bundles.append({
            "builder": bundle,
            "tokens": tokenize_name(name) | extra_tokens,
        })

    return bundles, used_paths


def detect_home_data_entries(home: Path, user: str) -> list:
    entries = []
    roots = [
        (home / ".config", "config_folder", "Settings", 0.22, "config"),
        (home / ".local" / "share", "local_share_folder", "App data", 0.22, "data"),
        (home / ".local" / "state", "local_state_folder", "App state", 0.20, "data"),
        (home / ".cache", "cache_folder", "Cache", 0.10, "folder"),
        (home / ".var" / "app", "flatpak_folder", "Flatpak app data", 0.24, "data"),
        (home / ".wine", "wine_folder", "Wine prefix", 0.24, "runtime-prefix"),
        (home / "Applications", "application_folder", "Application folder", 0.25, "folder"),
        (home / "Games", "games_folder", "Game folder", 0.25, "folder"),
    ]
    for base, signal, label, confidence, kind in roots:
        if not base.exists():
            continue
        for entry in base.iterdir():
            if not entry.is_dir():
                continue
            if entry.name in {"applications", "Steam"}:
                continue
            entries.append({
                "normalized_key": normalize_name(entry.name),
                "tokens": tokenize_name(entry.name),
                "name": entry.name,
                "signal": signal,
                "label": label,
                "source": rel_source(entry, home, user),
                "target": str(entry),
                "kind": kind,
                "confidence_boost": confidence,
            })
    return entries


def detect_downloaded_apps(home: Path, user: str) -> list:
    results = []
    scan_dirs = [home / "Applications", home / "Downloads"]
    for scan_dir in scan_dirs:
        if not scan_dir.exists():
            continue
        for entry in scan_dir.iterdir():
            if not entry.exists():
                continue
            if entry.is_file():
                lower_name = entry.name.lower()
                if any(lower_name.endswith(ext) for ext in COMMON_EXTS):
                    results.append({
                        "normalized_key": normalize_name(entry.name),
                        "tokens": tokenize_name(entry.name),
                        "name": entry.name,
                        "signal": "downloaded_app",
                        "label": "Application binary",
                        "source": rel_source(entry, home, user),
                        "target": str(entry),
                        "kind": "file",
                        "confidence_boost": 0.18,
                    })
            elif entry.is_dir():
                try:
                    children = list(entry.iterdir())
                except Exception:
                    children = []
                has_exec = any(child.is_file() and os.access(child, os.X_OK) for child in children)
                if has_exec:
                    results.append({
                        "normalized_key": normalize_name(entry.name),
                        "tokens": tokenize_name(entry.name),
                        "name": entry.name,
                        "signal": "application_folder",
                        "label": "Application folder",
                        "source": rel_source(entry, home, user),
                        "target": str(entry),
                        "kind": "folder",
                        "confidence_boost": 0.20,
                    })
    return results


def best_anchor_for_entry(entry: dict, anchors: list):
    best = None
    best_score = 0
    for anchor in anchors:
        overlap = len(entry["tokens"] & anchor["tokens"])
        if overlap > best_score:
            best = anchor
            best_score = overlap
        elif overlap == best_score and overlap > 0 and best is not None:
            if len(anchor["tokens"]) < len(best["tokens"]):
                best = anchor
    return best if best_score > 0 else None


def attach_related_entries(anchors: list, entries: list, used_paths: set):
    unmatched = []
    for entry in entries:
        if not entry["normalized_key"] or str(entry["target"]) in used_paths:
            continue
        anchor = best_anchor_for_entry(entry, anchors)
        if anchor is None:
            unmatched.append(entry)
            continue
        anchor["builder"].add_path(
            component_id=create_component_id(entry["signal"], entry["name"]),
            label=entry["label"],
            source=entry["source"],
            target=entry["target"],
            kind=entry["kind"],
            signal=entry["signal"],
            default_selected=True,
            confidence_boost=entry["confidence_boost"],
        )
        used_paths.add(str(entry["target"]))
    return unmatched


def build_generic_bundles(unmatched: list) -> list:
    grouped = defaultdict(list)
    for entry in unmatched:
        grouped[entry["normalized_key"]].append(entry)

    bundles = []
    for normalized, items in grouped.items():
        if not normalized:
            continue
        name = items[0]["name"]
        bundle = BundleBuilder(f"bundle.{normalized}", name, "generic", normalized)
        for item in items:
            bundle.add_path(
                component_id=create_component_id(item["signal"], item["name"]),
                label=item["label"],
                source=item["source"],
                target=item["target"],
                kind=item["kind"],
                signal=item["signal"],
                default_selected=True,
                confidence_boost=item["confidence_boost"],
            )
        bundles.append(bundle)
    return bundles


def build_bundles(home: Path, user: str) -> list:
    steam_anchors, used_paths = build_steam_bundles(home, user)
    desktop_anchors, desktop_paths = build_desktop_anchors(home, user)
    used_paths |= desktop_paths

    anchors = steam_anchors + desktop_anchors
    related_entries = detect_home_data_entries(home, user) + detect_downloaded_apps(home, user)
    unmatched = attach_related_entries(anchors, related_entries, used_paths)
    generic_bundles = build_generic_bundles(unmatched)

    builders = [item["builder"] for item in anchors] + generic_bundles
    bundles = [builder.to_json() for builder in builders if builder.folder_bundles]
    bundles.sort(key=lambda bundle: (-bundle["confidence"], bundle["name"].lower()))
    return bundles


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

    bundles = build_bundles(home, args.user)

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
