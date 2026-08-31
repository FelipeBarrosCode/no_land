#!/usr/bin/env bash
# Build and start noland-state-agent on a disposable Linux instance.
set -euo pipefail

SRC="${1:-/opt/noland/state-agent}"
BIN="${2:-/usr/local/bin/noland-state-agent}"
TARGET_USER="${3:-}"

export NOLAND_STATE_ROOT="${NOLAND_STATE_ROOT:-/var/lib/noland/state}"
export NOLAND_RUN_ROOT="${NOLAND_RUN_ROOT:-/run/noland}"

require_ebpf_unit_support() {
  local cap_last systemd_version
  cap_last="$(cat /proc/sys/kernel/cap_last_cap 2>/dev/null || echo 0)"
  systemd_version="$(systemctl --version | awk 'NR == 1 { print $2 }')"
  if (( cap_last < 39 )); then
    echo "cannot install least-privilege eBPF unit: Linux 5.8+ with CAP_BPF/CAP_PERFMON is required" >&2
    return 1
  fi
  if (( systemd_version < 246 )); then
    echo "cannot install least-privilege eBPF unit: systemd 246+ is required for CAP_BPF" >&2
    return 1
  fi
}

find_bpf_object() {
  if [[ -n "${NOLAND_BPF_OBJECT:-}" && -f "$NOLAND_BPF_OBJECT" ]]; then
    printf '%s\n' "$NOLAND_BPF_OBJECT"
    return
  fi
  find "$SRC/target/release/build" -path '*/out/noland_observer.bpf.o' -type f -print -quit 2>/dev/null
}

mkdir -p "$NOLAND_STATE_ROOT" "$NOLAND_RUN_ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
cd "$SRC"
cargo build --release -p noland-state-agent
install -m 0755 "$SRC/target/release/noland-state-agent" "$BIN"

BPF_OBJECT="$(find_bpf_object)"
if [[ ! -f "$BPF_OBJECT" ]]; then
  echo "noland_observer.bpf.o not found; build on Linux with clang's BPF backend or set NOLAND_BPF_OBJECT" >&2
  exit 1
fi
install -d /usr/local/lib/noland
install -m 0644 "$BPF_OBJECT" /usr/local/lib/noland/noland_observer.bpf.o

if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemd is required to grant CAP_BPF and CAP_PERFMON without CAP_SYS_ADMIN" >&2
  exit 1
fi
if [[ -f "$SRC/systemd/noland-state-agent.service" ]]; then
    require_ebpf_unit_support
    if [[ -n "$TARGET_USER" ]]; then
      TARGET_GROUP="$(id -gn "$TARGET_USER")"
      sed \
        -e "s|^ExecStart=|User=$TARGET_USER\nGroup=$TARGET_GROUP\nExecStart=|" \
        -e "s|Environment=NOLAND_HOME=/home/user|Environment=NOLAND_HOME=/home/$TARGET_USER|" \
        "$SRC/systemd/noland-state-agent.service" > /etc/systemd/system/noland-state-agent.service
    else
      cp "$SRC/systemd/noland-state-agent.service" /etc/systemd/system/noland-state-agent.service
    fi
    systemctl daemon-reload
    systemctl enable --now noland-state-agent.service
else
  echo "missing systemd unit at $SRC/systemd/noland-state-agent.service" >&2
  exit 1
fi

if ! systemctl is-active --quiet noland-state-agent.service; then
  systemctl --no-pager --full status noland-state-agent.service >&2 || true
  exit 1
fi
if ! ss -xl 2>/dev/null | grep -q "$NOLAND_RUN_ROOT/state-agent.sock"; then
  echo "state-agent service is active but RPC socket is missing" >&2
  exit 1
fi

echo "STATE_AGENT_READY"
