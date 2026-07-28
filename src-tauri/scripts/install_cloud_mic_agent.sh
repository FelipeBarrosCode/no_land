#!/bin/bash
set -euo pipefail

# Noland Microphone Receiver provisioning script for Ubuntu VMs
# Installs and configures noland-mic-receiver as a systemd user service.

USER_NAME="${1:-user}"
INSTALL_DIR="/home/$USER_NAME/.local/bin"
SERVICE_DIR="/home/$USER_NAME/.config/systemd/user"
CONFIG_DIR="/etc/noland"
RUNTIME_DIR_BASE="/run/noland"
FIFO_PATH="${RUNTIME_DIR_BASE}/noland_remote_microphone.pcm"
MODULE_ID_FILE="${RUNTIME_DIR_BASE}/noland_remote_microphone.module"

uid="$(id -u "$USER_NAME")"
group_name="$(id -gn "$USER_NAME")"
runtime_dir="/run/user/${uid}"
bus_path="${runtime_dir}/bus"

run_user_systemctl() {
    sudo -u "$USER_NAME" env \
        XDG_RUNTIME_DIR="$runtime_dir" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=${bus_path}" \
        systemctl --user "$@"
}

# Detect WireGuard IP
WG_IP=""
if command -v ip >/dev/null 2>&1; then
    WG_IP=$(ip -4 addr show wg0 2>/dev/null | grep -oP '(?<=inet\s)\d+(\.\d+){3}' | head -1 || true)
fi

if [ -z "$WG_IP" ]; then
    echo "Warning: Could not detect WireGuard IP. Receiver will bind to 127.0.0.1"
    WG_IP="127.0.0.1"
fi

echo "=== Noland Microphone Receiver Installation ==="
echo "User:         $USER_NAME"
echo "WireGuard IP: $WG_IP"
echo ""

export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y pipewire pipewire-pulse wireplumber pulseaudio-utils

# --- Create directories ---
mkdir -p "$INSTALL_DIR"
mkdir -p "$SERVICE_DIR"
mkdir -p "$CONFIG_DIR"
mkdir -p "$RUNTIME_DIR_BASE"
chown -R "$USER_NAME:$group_name" "/home/$USER_NAME/.local" "/home/$USER_NAME/.config"
chown "$USER_NAME:$group_name" "$RUNTIME_DIR_BASE"

# --- Install binary ---
if [ -f "/tmp/noland-mic-receiver" ]; then
    cp "/tmp/noland-mic-receiver" "$INSTALL_DIR/noland-mic-receiver"
    chmod +x "$INSTALL_DIR/noland-mic-receiver"
    echo "Binary installed to $INSTALL_DIR/noland-mic-receiver"
else
    echo "Warning: /tmp/noland-mic-receiver not found."
    echo "  Build: cd vm-cloud-mic-agent && cargo build --release"
    echo "  Copy:  scp target/release/noland-mic-receiver user@vm:/tmp/"
fi

# --- Write configuration ---
cat > "$CONFIG_DIR/microphone.toml" <<TOML
[network]
bind_address = "${WG_IP}"
port = 48020
interface = "wg0"
maximum_packet_size = 1200
recv_buffer_bytes = 524288

[audio]
sample_rate = 48000
channels = 1
frame_duration_ms = 10
pipewire_node_name = "noland_remote_microphone"
pipewire_description = "Noland Remote Microphone"

[jitter]
initial_ms = 20.0
minimum_ms = 10.0
maximum_ms = 40.0
reorder_window_packets = 64

[security]
require_active_session = false
require_packet_authentication = false
session_timeout_seconds = 5
TOML

chown "$USER_NAME:$group_name" "$CONFIG_DIR/microphone.toml"
echo "Configuration written to $CONFIG_DIR/microphone.toml"

# --- Create PipeWire source helper scripts ---
cat > "$INSTALL_DIR/noland-mic-source-setup" <<'EOF'
#!/bin/bash
set -euo pipefail
RUNTIME_DIR_BASE="/run/noland"
FIFO_PATH="/run/noland/noland_remote_microphone.pcm"
MODULE_ID_FILE="/run/noland/noland_remote_microphone.module"
mkdir -p "$RUNTIME_DIR_BASE"
rm -f "$FIFO_PATH"
mkfifo "$FIFO_PATH"
chmod 660 "$FIFO_PATH"
existing_module="$(pactl list short modules 2>/dev/null | awk '/module-pipe-source/ && /source_name=noland_remote_microphone/ {print $1; exit}')"
if [[ -n "$existing_module" ]]; then
  pactl unload-module "$existing_module" >/dev/null 2>&1 || true
fi
if [[ -f "$MODULE_ID_FILE" ]]; then
  old_module_id="$(cat "$MODULE_ID_FILE" 2>/dev/null || true)"
  if [[ -n "$old_module_id" ]]; then
    pactl unload-module "$old_module_id" >/dev/null 2>&1 || true
  fi
  rm -f "$MODULE_ID_FILE"
fi
module_id="$(pactl load-module module-pipe-source source_name=noland_remote_microphone file="$FIFO_PATH" format=s16le rate=48000 channels=1 source_properties=device.description='Noland Remote Microphone')"
echo "$module_id" > "$MODULE_ID_FILE"
pactl set-default-source noland_remote_microphone >/dev/null 2>&1 || true
EOF
chmod +x "$INSTALL_DIR/noland-mic-source-setup"
chown "$USER_NAME:$group_name" "$INSTALL_DIR/noland-mic-source-setup"

cat > "$INSTALL_DIR/noland-mic-source-cleanup" <<'EOF'
#!/bin/bash
set -euo pipefail
FIFO_PATH="/run/noland/noland_remote_microphone.pcm"
MODULE_ID_FILE="/run/noland/noland_remote_microphone.module"
if [[ -f "$MODULE_ID_FILE" ]]; then
  module_id="$(cat "$MODULE_ID_FILE" 2>/dev/null || true)"
  if [[ -n "$module_id" ]]; then
    pactl unload-module "$module_id" >/dev/null 2>&1 || true
  fi
  rm -f "$MODULE_ID_FILE"
fi
rm -f "$FIFO_PATH"
EOF
chmod +x "$INSTALL_DIR/noland-mic-source-cleanup"
chown "$USER_NAME:$group_name" "$INSTALL_DIR/noland-mic-source-cleanup"

# --- Create systemd user service ---
cat > "$SERVICE_DIR/noland-mic-receiver.service" <<EOF
[Unit]
Description=Noland Remote Microphone Receiver
After=pipewire.service pipewire-pulse.service wireplumber.service
Wants=pipewire.service pipewire-pulse.service wireplumber.service

[Service]
Type=simple
ExecStartPre=${INSTALL_DIR}/noland-mic-source-setup
ExecStart=/usr/bin/bash -lc 'exec ${INSTALL_DIR}/noland-mic-receiver --config ${CONFIG_DIR}/microphone.toml --bind ${WG_IP} --port 48020 > ${FIFO_PATH}'
ExecStopPost=${INSTALL_DIR}/noland-mic-source-cleanup
Restart=always
RestartSec=1
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=${RUNTIME_DIR_BASE}
LimitNOFILE=4096

[Install]
WantedBy=default.target
EOF

echo "Systemd user service created"

# --- Enable and start ---
if [[ -d "$runtime_dir" && -S "$bus_path" ]]; then
    run_user_systemctl daemon-reload
    run_user_systemctl enable noland-mic-receiver.service || true
    run_user_systemctl restart noland-mic-receiver.service || true
else
    echo "Warning: user systemd session bus unavailable for $USER_NAME"
fi

# --- Verify ---
sleep 2
echo ""
if [[ -d "$runtime_dir" && -S "$bus_path" ]] && run_user_systemctl is-active noland-mic-receiver.service >/dev/null 2>&1; then
    echo "[OK] noland-mic-receiver is running"
else
    echo "[FAIL] noland-mic-receiver failed to start"
fi

# Check UDP socket
if ss -uln | grep -q ":48020 "; then
    echo "[OK] UDP port 48020 is listening"
else
    echo "[WARN] UDP port 48020 not detected (may need root for ss/netstat)"
fi

# Check Pulse/PipeWire source
if [[ -d "$runtime_dir" && -S "$bus_path" ]] && sudo -u "$USER_NAME" env XDG_RUNTIME_DIR="$runtime_dir" DBUS_SESSION_BUS_ADDRESS="unix:path=${bus_path}" pactl list short sources 2>/dev/null | grep -q 'noland_remote_microphone'; then
    echo "[OK] PipeWire microphone source is published"
else
    echo "[WARN] PipeWire microphone source not detected"
fi

echo ""
echo "Installation complete."
echo "Manage:  systemctl --user {start|stop|restart|status} noland-mic-receiver.service"
echo "Logs:    journalctl --user -u noland-mic-receiver.service -f"
echo "Check:   pactl list short sources | grep noland_remote_microphone"
