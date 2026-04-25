#!/bin/bash
set -euo pipefail

# Cloud Mic Agent provisioning script for Ubuntu VMs
# This installs and configures the cloud-mic-agent as a systemd user service

USER_NAME="${1:-user}"
AGENT_VERSION="${2:-0.1.0}"
INSTALL_DIR="/home/$USER_NAME/.local/bin"
SERVICE_DIR="/home/$USER_NAME/.config/systemd/user"

# Detect WireGuard IP
WG_IP=""
if command -v wg >/dev/null 2>&1; then
    WG_IP=$(wg show wg0 listen-port 2>/dev/null | head -1 || true)
    if [ -z "$WG_IP" ]; then
        WG_IP=$(ip -4 addr show wg0 2>/dev/null | grep -oP '(?<=inet\s)\d+(\.\d+){3}' | head -1 || true)
    fi
fi

if [ -z "$WG_IP" ]; then
    echo "Warning: Could not detect WireGuard IP. Agent will bind to 127.0.0.1"
    WG_IP="127.0.0.1"
fi

echo "Installing cloud-mic-agent for user: $USER_NAME"
echo "WireGuard IP detected: $WG_IP"

# Ensure directories exist
mkdir -p "$INSTALL_DIR"
mkdir -p "$SERVICE_DIR"

# Copy binary (assumes binary is available at /tmp/cloud-mic-agent)
if [ -f "/tmp/cloud-mic-agent" ]; then
    cp "/tmp/cloud-mic-agent" "$INSTALL_DIR/cloud-mic-agent"
    chmod +x "$INSTALL_DIR/cloud-mic-agent"
    echo "Binary installed to $INSTALL_DIR/cloud-mic-agent"
else
    echo "Warning: /tmp/cloud-mic-agent not found. Please build and copy the binary first."
    echo "  cd vm-cloud-mic-agent && cargo build --release"
    echo "  scp target/release/cloud-mic-agent user@vm:/tmp/"
fi

# Create systemd user service
cat > "$SERVICE_DIR/cloud-mic-agent.service" <<EOF
[Unit]
Description=Cloud Mic Agent
After=pipewire.service pipewire-pulse.service

[Service]
Type=simple
Environment="CLOUD_MIC_AGENT_BIND=${WG_IP}:34779"
Environment="CLOUD_MIC_RTP_PORT=34778"
Environment="CLOUD_MIC_WG_IP=${WG_IP}"
Environment="RUST_LOG=info"
ExecStart=${INSTALL_DIR}/cloud-mic-agent
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF

echo "Systemd user service created"

# Enable and start service for the user
systemctl --user daemon-reload
systemctl --user enable cloud-mic-agent.service || true
systemctl --user start cloud-mic-agent.service || true

# Verify
sleep 1
if systemctl --user is-active cloud-mic-agent.service >/dev/null 2>&1; then
    echo "Cloud Mic Agent is running"
    curl -sf "http://${WG_IP}:34779/health" && echo "Health check: OK" || echo "Health check: FAILED"
else
    echo "Cloud Mic Agent failed to start. Check logs with:"
    echo "  systemctl --user status cloud-mic-agent.service"
fi

echo ""
echo "Installation complete."
echo "Manage with: systemctl --user {start|stop|restart|status} cloud-mic-agent.service"
