#!/usr/bin/env bash
# Build and start noland-state-agent on a disposable Linux instance.
set -euo pipefail

SRC="${1:-/opt/noland/state-agent}"
BIN="${2:-/usr/local/bin/noland-state-agent}"
export NOLAND_STATE_ROOT="${NOLAND_STATE_ROOT:-/var/lib/noland/state}"
export NOLAND_RUN_ROOT="${NOLAND_RUN_ROOT:-/run/noland}"

mkdir -p "$NOLAND_STATE_ROOT" "$NOLAND_RUN_ROOT"

if [[ ! -x "$BIN" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  fi
  cd "$SRC"
  cargo build --release -p noland-state-agent
  install -m 0755 "$SRC/target/release/noland-state-agent" "$BIN"
fi

if [[ -f /etc/systemd/system/noland-state-agent.service ]] || command -v systemctl >/dev/null 2>&1; then
  if [[ -f "$SRC/systemd/noland-state-agent.service" ]]; then
    cp "$SRC/systemd/noland-state-agent.service" /etc/systemd/system/noland-state-agent.service
    systemctl daemon-reload || true
    systemctl enable --now noland-state-agent.service || true
  fi
fi

if ! ss -xl 2>/dev/null | grep -q "$NOLAND_RUN_ROOT/state-agent.sock"; then
  if ! systemctl is-active --quiet noland-state-agent.service 2>/dev/null; then
    nohup "$BIN" >/var/log/noland-state-agent.log 2>&1 &
    sleep 1
  fi
fi

echo "STATE_AGENT_READY"
