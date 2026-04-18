#!/usr/bin/env bash
set -euo pipefail

# Installs persistent systemd service for Noland stream QoS.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

sudo install -m 0755 "${SCRIPT_DIR}/setup_stream_qos.sh" /usr/local/bin/noland-setup-stream-qos.sh
sudo install -m 0755 "${SCRIPT_DIR}/rollback_stream_qos.sh" /usr/local/bin/noland-rollback-stream-qos.sh

sudo tee /etc/systemd/system/noland-stream-qos.service >/dev/null <<'EOF'
[Unit]
Description=Noland stream QoS (Sunshine traffic prioritization)
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
Environment=RATE_KBIT=90000
Environment=STREAM_MIN_KBIT=20000
Environment=INTERACTIVE_MIN_KBIT=5000
Environment=BULK_MIN_KBIT=1000
ExecStart=/usr/local/bin/noland-setup-stream-qos.sh
ExecStop=/usr/local/bin/noland-rollback-stream-qos.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now noland-stream-qos.service
sudo systemctl status --no-pager -n 30 noland-stream-qos.service

echo "Installed. Tune rates with: sudo systemctl edit noland-stream-qos.service"
