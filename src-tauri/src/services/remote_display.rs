use std::{collections::HashMap, time::Duration};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{AppError, AppResult};

use super::{
    display_profile::{DisplayModeSpec, DisplayProfile},
    remote_exec::RemoteExec,
};

const SHARED_XAUTHORITY: &str = "/etc/X11/.Xauthority-noland";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceDisplayStatus {
    pub desired_profile: DisplayProfile,
    pub desired_profile_hash: String,
    pub installed_profile_hash: String,
    pub output_name: Option<String>,
    pub active_mode: Option<DisplayModeSpec>,
    pub selected_mode: Option<DisplayModeSpec>,
    pub xorg_active: bool,
    pub sunshine_active: bool,
    pub profile_update_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyDisplayModeResult {
    pub status: InstanceDisplayStatus,
    pub xorg_restarted: bool,
}

pub struct RemoteDisplayService;

impl RemoteDisplayService {
    pub async fn status(
        remote: &RemoteExec,
        desired_profile: DisplayProfile,
        desired_edid_base64: &str,
    ) -> AppResult<InstanceDisplayStatus> {
        let desired_profile_hash = edid_sha256(desired_edid_base64)?;
        let output = run_remote(
            remote,
            format!(
                r#"sudo bash -lc 'set -u
XAUTH="{xauthority}"
EDID_SHA=""
if [ -f /etc/X11/edid.bin ]; then EDID_SHA=$(sha256sum /etc/X11/edid.bin | cut -d" " -f1); fi
XORG_ACTIVE=$(systemctl is-active noland-xorg 2>/dev/null || true)
SUNSHINE_ACTIVE=$(systemctl is-active sunshine 2>/dev/null || true)
OUTPUT=""
ACTIVE_RES=""
ACTIVE_RATE=""
if [ "$XORG_ACTIVE" = "active" ] && [ -f "$XAUTH" ]; then
  XRANDR=$(DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query 2>/dev/null || true)
  OUTPUT=$(printf "%s\n" "$XRANDR" | grep " connected" | sed -E "s/^([^[:space:]]+).*/\\1/" | sed -n "1p")
  ACTIVE_LINE=$(printf "%s\n" "$XRANDR" | grep -E "^[[:space:]]+[0-9]+x[0-9]+" | grep "\\*" | sed -n "1p" || true)
  ACTIVE_RES=$(printf "%s\n" "$ACTIVE_LINE" | sed -E "s/^[[:space:]]*([^[:space:]]+).*/\\1/")
  for TOKEN in $ACTIVE_LINE; do
    case "$TOKEN" in *\**) ACTIVE_RATE=${{TOKEN%%\**}} ;; esac
  done
fi
SELECTED=""
if [ -f /etc/noland/display.env ]; then
  . /etc/noland/display.env
  SELECTED="${{NOLAND_WIDTH:-}}x${{NOLAND_HEIGHT:-}}@${{NOLAND_REFRESH_MILLIHZ:-}}"
fi
printf "EDID_SHA=%s\nOUTPUT=%s\nACTIVE_RES=%s\nACTIVE_RATE=%s\nSELECTED=%s\nXORG_ACTIVE=%s\nSUNSHINE_ACTIVE=%s\n" "$EDID_SHA" "$OUTPUT" "$ACTIVE_RES" "$ACTIVE_RATE" "$SELECTED" "$XORG_ACTIVE" "$SUNSHINE_ACTIVE"'"#,
                xauthority = SHARED_XAUTHORITY,
            ),
            Duration::from_secs(30),
        )
        .await?;

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed reading remote display status: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        let values = parse_key_values(&output.stdout);
        let installed_profile_hash = values.get("EDID_SHA").cloned().unwrap_or_default();
        let active_mode = parse_active_mode(
            values.get("ACTIVE_RES").map(String::as_str).unwrap_or(""),
            values.get("ACTIVE_RATE").map(String::as_str).unwrap_or(""),
        );
        let selected_mode = values
            .get("SELECTED")
            .and_then(|value| parse_selected_mode(value));

        Ok(InstanceDisplayStatus {
            desired_profile,
            profile_update_required: installed_profile_hash != desired_profile_hash,
            desired_profile_hash,
            installed_profile_hash,
            output_name: values
                .get("OUTPUT")
                .filter(|value| !value.is_empty())
                .cloned(),
            active_mode,
            selected_mode,
            xorg_active: values
                .get("XORG_ACTIVE")
                .is_some_and(|value| value == "active"),
            sunshine_active: values
                .get("SUNSHINE_ACTIVE")
                .is_some_and(|value| value == "active"),
        })
    }

    pub async fn apply(
        remote: &RemoteExec,
        target_user: &str,
        desired_profile: DisplayProfile,
        desired_edid_base64: &str,
        mode: DisplayModeSpec,
    ) -> AppResult<ApplyDisplayModeResult> {
        mode.validate()
            .map_err(|error| AppError::InvalidInput(error.to_string()))?;
        if !desired_profile.advertised_modes.contains(&mode) {
            return Err(AppError::InvalidInput(format!(
                "{} is not advertised by the desired display profile",
                mode.label()
            )));
        }
        let safe_user = sanitize_username(target_user)?;
        let desired_hash = edid_sha256(desired_edid_base64)?;
        let refresh_hz = mode.refresh_millihz as f64 / 1000.0;
        let script = format!(
            r#"sudo bash -lc 'set -euo pipefail
TARGET_USER="{target_user}"
WIDTH={width}
HEIGHT={height}
REFRESH_MILLIHZ={refresh_millihz}
REFRESH_HZ="{refresh_hz:.3}"
XAUTH="{xauthority}"
DESIRED_HASH="{desired_hash}"
EDID_CHANGED=0
mkdir -p /etc/noland /etc/systemd/system/sunshine.service.d /usr/local/sbin /etc/X11
SUNSHINE_CONF=$(getent passwd "$TARGET_USER" | cut -d: -f6)/.config/sunshine/sunshine.conf
ROLLBACK_DIR=$(mktemp -d /tmp/noland-display-rollback.XXXXXX)
backup_file() {{
  SOURCE="$1"; KEY="$2"
  if [ -e "$SOURCE" ]; then cp -a "$SOURCE" "$ROLLBACK_DIR/$KEY"; touch "$ROLLBACK_DIR/$KEY.exists"; fi
}}
restore_file() {{
  TARGET="$1"; KEY="$2"
  if [ -f "$ROLLBACK_DIR/$KEY.exists" ]; then cp -a "$ROLLBACK_DIR/$KEY" "$TARGET"; else rm -f "$TARGET"; fi
}}
backup_file /etc/X11/edid.bin edid
backup_file /etc/noland/display.env display-env
backup_file /usr/local/sbin/noland-apply-display-mode display-script
backup_file /etc/systemd/system/noland-display-mode.service display-unit
backup_file /etc/systemd/system/sunshine.service.d/noland-display.conf sunshine-dropin
backup_file "$SUNSHINE_CONF" sunshine-conf
rollback_all() {{
  trap - ERR
  restore_file /etc/X11/edid.bin edid
  restore_file /etc/noland/display.env display-env
  restore_file /usr/local/sbin/noland-apply-display-mode display-script
  restore_file /etc/systemd/system/noland-display-mode.service display-unit
  restore_file /etc/systemd/system/sunshine.service.d/noland-display.conf sunshine-dropin
  restore_file "$SUNSHINE_CONF" sunshine-conf
  systemctl daemon-reload 2>/dev/null || true
  systemctl restart noland-xorg 2>/dev/null || true
  for ATTEMPT in $(seq 1 20); do
    if DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query >/dev/null 2>&1; then break; fi
    sleep 1
  done
  if systemctl cat noland-display-mode.service >/dev/null 2>&1; then systemctl restart noland-display-mode.service 2>/dev/null || true; fi
  systemctl restart sunshine 2>/dev/null || true
  rm -rf "$ROLLBACK_DIR" /tmp/noland-edid.bin
}}
trap rollback_all ERR
base64 -d > /tmp/noland-edid.bin <<"NOLANDEDID"
{edid_base64}
NOLANDEDID
UPLOADED_HASH=$(sha256sum /tmp/noland-edid.bin | cut -d" " -f1)
if [ "$UPLOADED_HASH" != "$DESIRED_HASH" ]; then echo "Uploaded EDID hash mismatch" >&2; rollback_all; exit 1; fi
CURRENT_HASH=""
if [ -f /etc/X11/edid.bin ]; then CURRENT_HASH=$(sha256sum /etc/X11/edid.bin | cut -d" " -f1); fi
if [ "$CURRENT_HASH" != "$DESIRED_HASH" ]; then
  install -m 0644 /tmp/noland-edid.bin /etc/X11/edid.bin
  EDID_CHANGED=1
fi
if ! systemctl cat noland-xorg >/dev/null 2>&1; then echo "noland-xorg.service is not installed; run provisioning first" >&2; rollback_all; exit 1; fi
systemctl daemon-reload
systemctl enable noland-xorg >/dev/null
if [ "$EDID_CHANGED" = "1" ]; then
  systemctl stop sunshine 2>/dev/null || true
  systemctl restart noland-xorg || {{ rollback_all; exit 1; }}
else
  systemctl start noland-xorg
fi
DISPLAY_READY=0
for ATTEMPT in $(seq 1 30); do
  if DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query >/dev/null 2>&1; then DISPLAY_READY=1; break; fi
  sleep 1
done
if [ "$DISPLAY_READY" != "1" ]; then
  journalctl -u noland-xorg --no-pager -n 80 2>/dev/null || true
  rollback_all
  echo "Xorg did not become ready after applying the display profile" >&2
  exit 1
fi
cat > /etc/noland/display.env <<NOLANDDISPLAY
NOLAND_WIDTH=$WIDTH
NOLAND_HEIGHT=$HEIGHT
NOLAND_REFRESH_MILLIHZ=$REFRESH_MILLIHZ
NOLAND_EDID_SHA256=$DESIRED_HASH
NOLANDDISPLAY
cat > /usr/local/sbin/noland-apply-display-mode <<"NOLANDSCRIPT"
#!/bin/bash
set -euo pipefail
. /etc/noland/display.env
XAUTH=/etc/X11/.Xauthority-noland
DISPLAY_READY=0
for ATTEMPT in $(seq 1 30); do
  if DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query >/dev/null 2>&1; then DISPLAY_READY=1; break; fi
  sleep 1
done
[ "$DISPLAY_READY" = "1" ]
REFRESH_HZ=$(printf "%d.%03d" "$((NOLAND_REFRESH_MILLIHZ / 1000))" "$((NOLAND_REFRESH_MILLIHZ % 1000))")
OUTPUT=$(DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query | grep " connected" | sed -E "s/^([^[:space:]]+).*/\\1/" | sed -n "1p")
[ -n "$OUTPUT" ]
RESOLUTION="${{NOLAND_WIDTH}}x${{NOLAND_HEIGHT}}"
if ! DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query | grep -Eq "^[[:space:]]+${{RESOLUTION}}[[:space:]]"; then
  MODELINE=$(cvt -r "$NOLAND_WIDTH" "$NOLAND_HEIGHT" "$REFRESH_HZ" | grep Modeline | sed "s/Modeline //")
  MODE_NAME=$(printf "%s" "$MODELINE" | sed -E "s/^\"([^\"]+)\".*/\\1/")
  eval DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --newmode "$MODELINE" 2>/dev/null || true
  DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --addmode "$OUTPUT" "$MODE_NAME" 2>/dev/null || true
  DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --output "$OUTPUT" --mode "$MODE_NAME"
else
  DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --output "$OUTPUT" --mode "$RESOLUTION" --rate "$REFRESH_HZ"
fi
ACTIVE=$(DISPLAY=:0 XAUTHORITY="$XAUTH" xrandr --query | grep -E "^[[:space:]]+${{RESOLUTION}}[[:space:]]" | grep "\\*" || true)
[ -n "$ACTIVE" ]
ACTIVE_RATE=""
for TOKEN in $ACTIVE; do case "$TOKEN" in *\**) ACTIVE_RATE=${{TOKEN%%\**}} ;; esac; done
[ -n "$ACTIVE_RATE" ]
awk -v actual="$ACTIVE_RATE" -v expected="$REFRESH_HZ" 'BEGIN {{ delta=actual-expected; if (delta<0) delta=-delta; exit(delta<=0.5 ? 0 : 1) }}'
printf "%s" "$OUTPUT" > /run/noland-display-output
NOLANDSCRIPT
chmod 0755 /usr/local/sbin/noland-apply-display-mode
cat > /etc/systemd/system/noland-display-mode.service <<"NOLANDUNIT"
[Unit]
Description=Apply Noland virtual display mode
Requires=noland-xorg.service
After=noland-xorg.service
Before=sunshine.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/noland-apply-display-mode
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
NOLANDUNIT
cat > /etc/systemd/system/sunshine.service.d/noland-display.conf <<"NOLANDDROPIN"
[Unit]
Requires=noland-display-mode.service
After=noland-display-mode.service network-online.target
Wants=network-online.target
NOLANDDROPIN
systemctl daemon-reload
systemctl enable noland-display-mode >/dev/null
if ! /usr/local/sbin/noland-apply-display-mode; then rollback_all; exit 1; fi
OUTPUT=$(cat /run/noland-display-output)
SUNSHINE_CONF=$(getent passwd "$TARGET_USER" | cut -d: -f6)/.config/sunshine/sunshine.conf
if [ -f "$SUNSHINE_CONF" ]; then
  if grep -q "^output_name" "$SUNSHINE_CONF"; then sed -i "s/^output_name.*/output_name = $OUTPUT/" "$SUNSHINE_CONF"; else printf "output_name = %s\n" "$OUTPUT" >> "$SUNSHINE_CONF"; fi
  chown "$TARGET_USER:$(id -gn "$TARGET_USER")" "$SUNSHINE_CONF"
fi
systemctl restart sunshine
SUNSHINE_READY=0
for ATTEMPT in $(seq 1 30); do
  if systemctl is-active --quiet sunshine && curl -k -s --connect-timeout 3 https://localhost:47990/pin >/dev/null 2>&1; then SUNSHINE_READY=1; break; fi
  sleep 1
done
if [ "$SUNSHINE_READY" != "1" ]; then
  systemctl status sunshine --no-pager 2>/dev/null || true
  journalctl -u sunshine --no-pager -n 80 2>/dev/null || true
  rollback_all
  exit 1
fi
trap - ERR
rm -rf "$ROLLBACK_DIR" /tmp/noland-edid.bin
printf "NOLAND_DISPLAY_APPLIED\nXORG_RESTARTED=%s\nOUTPUT=%s\n" "$EDID_CHANGED" "$OUTPUT"'"#,
            target_user = safe_user,
            width = mode.width,
            height = mode.height,
            refresh_millihz = mode.refresh_millihz,
            refresh_hz = refresh_hz,
            xauthority = SHARED_XAUTHORITY,
            desired_hash = desired_hash,
            edid_base64 = desired_edid_base64,
        );

        let output = run_remote(remote, script, Duration::from_secs(180)).await?;
        if output.status_code != 0 || !output.stdout.contains("NOLAND_DISPLAY_APPLIED") {
            return Err(AppError::Provisioning(format!(
                "Failed applying remote display mode {}: {} {}",
                mode.label(),
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        let xorg_restarted = output.stdout.lines().any(|line| line == "XORG_RESTARTED=1");
        let status = Self::status(remote, desired_profile, desired_edid_base64).await?;
        if status.active_mode.as_ref().is_none_or(|active| {
            active.width != mode.width
                || active.height != mode.height
                || active.refresh_millihz.abs_diff(mode.refresh_millihz) > 500
        }) {
            return Err(AppError::Provisioning(format!(
                "Remote display command completed, but {} is not active",
                mode.label()
            )));
        }

        Ok(ApplyDisplayModeResult {
            status,
            xorg_restarted,
        })
    }
}

pub fn edid_sha256(edid_base64: &str) -> AppResult<String> {
    let bytes = STANDARD.decode(edid_base64.trim()).map_err(|error| {
        AppError::InvalidInput(format!("Headless EDID is not valid base64: {error}"))
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

async fn run_remote(
    remote: &RemoteExec,
    command: String,
    timeout: Duration,
) -> AppResult<crate::services::remote_exec::ExecOutput> {
    let remote = remote.clone();
    tokio::task::spawn_blocking(move || remote.ssh(&command, timeout))
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))?
}

fn sanitize_username(value: &str) -> AppResult<&str> {
    if !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        Ok(value)
    } else {
        Err(AppError::InvalidInput(
            "Invalid target username".to_string(),
        ))
    }
}

fn parse_key_values(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn parse_active_mode(resolution: &str, refresh: &str) -> Option<DisplayModeSpec> {
    let (width, height) = resolution.split_once('x')?;
    let refresh =
        refresh.trim_end_matches(|character: char| !character.is_ascii_digit() && character != '.');
    let refresh_hz = refresh.parse::<f64>().ok()?;
    Some(DisplayModeSpec::new(
        width.parse().ok()?,
        height.parse().ok()?,
        (refresh_hz * 1000.0).round() as u32,
    ))
}

fn parse_selected_mode(value: &str) -> Option<DisplayModeSpec> {
    let (resolution, refresh) = value.split_once('@')?;
    let (width, height) = resolution.split_once('x')?;
    Some(DisplayModeSpec::new(
        width.parse().ok()?,
        height.parse().ok()?,
        refresh.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_xrandr_mode() {
        assert_eq!(
            parse_active_mode("1920x1080", "59.94"),
            Some(DisplayModeSpec::new(1920, 1080, 59_940))
        );
    }

    #[test]
    fn parses_persisted_selection() {
        assert_eq!(
            parse_selected_mode("2560x1440@120000"),
            Some(DisplayModeSpec::new(2560, 1440, 120_000))
        );
    }
}
