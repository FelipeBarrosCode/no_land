#!/usr/bin/env bash
# Install the state agent binary and systemd unit on a disposable Linux instance.
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
UNIT_SRC="$(cd "$(dirname "$0")/.." && pwd)/systemd/noland-state-agent.service"

if [[ ! -x "${PREFIX}/bin/noland-state-agent" ]]; then
  echo "place noland-state-agent at ${PREFIX}/bin/noland-state-agent first" >&2
  exit 1
fi

install -d /var/lib/noland/state /run/noland
install -m 0644 "${UNIT_SRC}" /etc/systemd/system/noland-state-agent.service
systemctl daemon-reload
systemctl enable --now noland-state-agent.service
systemctl --no-pager --full status noland-state-agent.service || true
