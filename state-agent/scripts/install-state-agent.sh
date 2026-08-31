#!/usr/bin/env bash
# Install the state agent binary and systemd unit on a disposable Linux instance.
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
AGENT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNIT_SRC="${AGENT_ROOT}/systemd/noland-state-agent.service"

find_bpf_object() {
  if [[ -n "${NOLAND_BPF_OBJECT:-}" && -f "$NOLAND_BPF_OBJECT" ]]; then
    printf '%s\n' "$NOLAND_BPF_OBJECT"
    return
  fi
  find "${AGENT_ROOT}/target/release/build" -path '*/out/noland_observer.bpf.o' -type f -print -quit 2>/dev/null
}

require_ebpf_unit_support() {
  local cap_last systemd_version
  cap_last="$(cat /proc/sys/kernel/cap_last_cap 2>/dev/null || echo 0)"
  systemd_version="$(systemctl --version | awk 'NR == 1 { print $2 }')"
  if (( cap_last < 39 )); then
    echo "cannot install least-privilege eBPF unit: Linux 5.8+ with CAP_BPF/CAP_PERFMON is required" >&2
    exit 1
  fi
  if (( systemd_version < 246 )); then
    echo "cannot install least-privilege eBPF unit: systemd 246+ is required for CAP_BPF" >&2
    exit 1
  fi
}

if [[ ! -x "${PREFIX}/bin/noland-state-agent" ]]; then
  echo "place noland-state-agent at ${PREFIX}/bin/noland-state-agent first" >&2
  exit 1
fi

require_ebpf_unit_support
BPF_OBJECT="$(find_bpf_object)"
if [[ ! -f "$BPF_OBJECT" ]]; then
  echo "noland_observer.bpf.o not found; build on Linux with clang's BPF backend or set NOLAND_BPF_OBJECT" >&2
  exit 1
fi
install -d /var/lib/noland/state /run/noland /usr/local/lib/noland
install -m 0644 "$BPF_OBJECT" /usr/local/lib/noland/noland_observer.bpf.o
install -m 0644 "${UNIT_SRC}" /etc/systemd/system/noland-state-agent.service
systemctl daemon-reload
systemctl enable --now noland-state-agent.service
systemctl --no-pager --full status noland-state-agent.service || true
