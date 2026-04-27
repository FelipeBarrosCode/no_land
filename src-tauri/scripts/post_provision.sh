#!/bin/bash
set -euo pipefail

TARGET_USER="${1:-user}"
USER_HOME="/home/${TARGET_USER}"
APP_DIR="${USER_HOME}/.local/share/applications"
BIN_DIR="${USER_HOME}/.local/bin"
WINE_PREFIX="${USER_HOME}/.wine-launchers"
MIC_DIR="${USER_HOME}/.local/share/noland-mic"
MIC_SCRIPT_DIR="${MIC_DIR}/scripts"
MIC_CONFIG_DIR="${USER_HOME}/.config/noland-mic"
MIC_CONFIG_PATH="${MIC_CONFIG_DIR}/config.env"

uid="$(id -u "$TARGET_USER")"
runtime_dir="/run/user/${uid}"
bus_path="${runtime_dir}/bus"

log() {
  printf '[post-provision] %s\n' "$*"
}

run_user() {
  sudo -u "$TARGET_USER" -H bash -lc "$*"
}

run_user_systemctl() {
  sudo -u "$TARGET_USER" env \
    XDG_RUNTIME_DIR="$runtime_dir" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=${bus_path}" \
    systemctl --user "$@"
}

ensure_packages() {
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -y
  apt-get install -y curl wget ca-certificates gnupg software-properties-common xdg-utils unzip python3 \
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-libav gstreamer1.0-pulseaudio pulseaudio-utils
}

install_chrome() {
  log "Installing Google Chrome"
  local deb_path="/tmp/google-chrome-stable_current_amd64.deb"
  wget -q "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb" -O "$deb_path"
  dpkg -i "$deb_path" || apt-get install -f -y
  rm -f "$deb_path"
  run_user "xdg-settings set default-web-browser google-chrome.desktop || true"
}

install_wine() {
  log "Installing latest Wine stable"
  dpkg --add-architecture i386
  mkdir -p /etc/apt/keyrings
  wget -qO /etc/apt/keyrings/winehq-archive.key https://dl.winehq.org/wine-builds/winehq.key

  local codename
  codename="$(. /etc/os-release && echo "${VERSION_CODENAME:-}")"
  if [[ -z "$codename" ]]; then
    codename="jammy"
  fi

  wget -qO "/etc/apt/sources.list.d/winehq-${codename}.sources" "https://dl.winehq.org/wine-builds/ubuntu/dists/${codename}/winehq-${codename}.sources" || true
  apt-get update -y
  apt-get install -y --install-recommends winehq-stable || apt-get install -y wine-stable
}

install_heroic_latest() {
  log "Installing latest Heroic AppImage"
  mkdir -p "$BIN_DIR" "$APP_DIR"

  local heroic_url
  heroic_url="$( (curl -fsSL https://api.github.com/repos/Heroic-Games-Launcher/HeroicGamesLauncher/releases/latest || true) | python3 -c 'import json,sys; d=json.load(sys.stdin); assets=d.get("assets",[]); u="";\nfor a in assets:\n n=a.get("name","")\n if n.endswith(".AppImage") and "arm" not in n.lower():\n  u=a.get("browser_download_url","")\n  break\nprint(u)')"

  if [[ -z "$heroic_url" ]]; then
    log "Could not detect latest Heroic release URL"
    return 1
  fi

  wget -q "$heroic_url" -O "${BIN_DIR}/heroic"
  chmod +x "${BIN_DIR}/heroic"
  chown "$TARGET_USER:$TARGET_USER" "${BIN_DIR}/heroic"

  cat > "${APP_DIR}/heroic.desktop" <<EOF
[Desktop Entry]
Name=Heroic Games Launcher
Comment=Epic, GOG and Amazon launcher
Exec=${BIN_DIR}/heroic
Icon=heroic
Terminal=false
Type=Application
Categories=Game;
StartupNotify=true
EOF
  chown "$TARGET_USER:$TARGET_USER" "${APP_DIR}/heroic.desktop"
}

setup_shared_wine_prefix() {
  log "Preparing shared Wine prefix"
  run_user "mkdir -p '${WINE_PREFIX}' '${APP_DIR}'"
  run_user "WINEPREFIX='${WINE_PREFIX}' wineboot --init >/dev/null 2>&1 || true"

  cat > "${APP_DIR}/ubisoft-connect.desktop" <<EOF
[Desktop Entry]
Name=Ubisoft Connect (Install Manually)
Comment=Run installer in shared Wine prefix
Exec=google-chrome 'https://ubisoftconnect.com'
Terminal=false
Type=Application
Categories=Game;
EOF

  cat > "${APP_DIR}/ea-app.desktop" <<EOF
[Desktop Entry]
Name=EA App (Install Manually)
Comment=Run installer in shared Wine prefix
Exec=google-chrome 'https://www.ea.com/ea-app'
Terminal=false
Type=Application
Categories=Game;
EOF

  cat > "${USER_HOME}/Desktop/Heroic-Store-Setup.txt" <<EOF
Heroic is installed and ready.

Inside Heroic, sign in to:
- Epic Games
- GOG
- Amazon Games

Ubisoft Connect and EA App use the shared Wine prefix:
${WINE_PREFIX}
EOF

  chown "$TARGET_USER:$TARGET_USER" "${APP_DIR}/ubisoft-connect.desktop" "${APP_DIR}/ea-app.desktop" "${USER_HOME}/Desktop/Heroic-Store-Setup.txt"
}

install_noland_mic_pipeline() {
  log "Installing Noland mic receiver pipeline"

  mkdir -p "${MIC_SCRIPT_DIR}" "${MIC_CONFIG_DIR}" "${USER_HOME}/.config/systemd/user"

  cat > "${MIC_SCRIPT_DIR}/create-virtual-mic.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
CONFIG_PATH="${HOME}/.config/noland-mic/config.env"
source "${CONFIG_PATH}"

if ! pactl list short sinks | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SINK}"; then
  pactl load-module module-null-sink sink_name="${VIRTUAL_MIC_SINK}" sink_properties="device.description=Cloud_Mic_Sink" rate=48000 channels=1 >/dev/null
fi

if ! pactl list short sources | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SOURCE}"; then
  pactl load-module module-remap-source master="${VIRTUAL_MIC_SINK}.monitor" source_name="${VIRTUAL_MIC_SOURCE}" source_properties="device.description=Cloud_Mic" >/dev/null
fi

pactl set-default-source "${VIRTUAL_MIC_SOURCE}" || true
EOF

  cat > "${MIC_SCRIPT_DIR}/start-receiver.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
CONFIG_PATH="${HOME}/.config/noland-mic/config.env"
LOG_FILE="/tmp/noland-mic-receiver.log"
PID_FILE="/tmp/noland-mic-receiver.pid"
source "${CONFIG_PATH}"

"$(dirname "$0")/create-virtual-mic.sh"

if [[ -f "${PID_FILE}" ]] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null; then
  exit 0
fi

nohup gst-launch-1.0 -e \
  udpsrc port="${PORT}" caps="application/x-rtp,media=audio,clock-rate=48000,encoding-name=OPUS,payload=96" \
  ! rtpjitterbuffer latency="${JITTER_LATENCY_MS}" drop-on-latency=true \
  ! rtpopusdepay ! opusdec ! audioconvert ! audioresample \
  ! pulsesink device="${VIRTUAL_MIC_SINK}" sync=false async=false \
  >"${LOG_FILE}" 2>&1 &
echo "$!" > "${PID_FILE}"
EOF

  cat > "${MIC_SCRIPT_DIR}/stop-receiver.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
PID_FILE="/tmp/noland-mic-receiver.pid"
if [[ -f "${PID_FILE}" ]]; then
  pid="$(cat "${PID_FILE}")"
  kill "${pid}" 2>/dev/null || true
  sleep 1
  kill -9 "${pid}" 2>/dev/null || true
  rm -f "${PID_FILE}"
fi
pkill -f "udpsrc port=.*rtpopusdepay" 2>/dev/null || true
EOF

  cat > "${MIC_SCRIPT_DIR}/status.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
CONFIG_PATH="${HOME}/.config/noland-mic/config.env"
source "${CONFIG_PATH}"

if [[ -f /tmp/noland-mic-receiver.pid ]] && kill -0 "$(cat /tmp/noland-mic-receiver.pid)" 2>/dev/null; then
  echo "receiver: running"
else
  echo "receiver: stopped"
fi

if pactl list short sinks | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SINK}"; then
  echo "sink: ${VIRTUAL_MIC_SINK} present"
else
  echo "sink: ${VIRTUAL_MIC_SINK} missing"
fi

if pactl list short sources | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SOURCE}"; then
  echo "source: ${VIRTUAL_MIC_SOURCE} present"
else
  echo "source: ${VIRTUAL_MIC_SOURCE} missing"
fi
EOF

  cat > "${MIC_CONFIG_PATH}" <<EOF
VM_IP=
PORT=5002
SAMPLE_RATE=48000
CHANNELS=1
OPUS_BITRATE=64000
OPUS_FRAME_SIZE=2.5
JITTER_LATENCY_MS=10
VIRTUAL_MIC_SINK=cloud_mic_sink
VIRTUAL_MIC_SOURCE=Cloud_Mic
EOF

  cat > "${USER_HOME}/.config/systemd/user/noland-mic-receiver.service" <<'EOF'
[Unit]
Description=Noland Cloud Mic RTP Receiver
After=network-online.target pipewire.service pipewire-pulse.service
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
EnvironmentFile=%h/.config/noland-mic/config.env
ExecStartPre=%h/.local/share/noland-mic/scripts/create-virtual-mic.sh
ExecStart=%h/.local/share/noland-mic/scripts/start-receiver.sh
ExecStop=%h/.local/share/noland-mic/scripts/stop-receiver.sh

[Install]
WantedBy=default.target
EOF

  chmod +x "${MIC_SCRIPT_DIR}"/*.sh
  chown -R "$TARGET_USER:$TARGET_USER" "${MIC_DIR}" "${MIC_CONFIG_DIR}" "${USER_HOME}/.config/systemd/user/noland-mic-receiver.service"

  loginctl enable-linger "$TARGET_USER" >/dev/null 2>&1 || true
  if [[ -d "$runtime_dir" && -S "$bus_path" ]]; then
    run_user_systemctl daemon-reload
    run_user_systemctl enable --now noland-mic-receiver.service || true
  else
    log "User systemd session bus unavailable; receiver service installed but not started"
  fi
}

main() {
  if ! id "$TARGET_USER" >/dev/null 2>&1; then
    echo "Target user '$TARGET_USER' not found" >&2
    exit 2
  fi

  mkdir -p "${USER_HOME}/Desktop"
  ensure_packages
  install_chrome
  install_wine
  install_heroic_latest
  setup_shared_wine_prefix
  install_noland_mic_pipeline
  log "Post-provision setup complete"
}

main "$@"
