#!/usr/bin/env bash
set -euo pipefail

# One-command rollback for Noland stream QoS.

EGRESS_IF="$(ip route get 1.1.1.1 | awk '{for (i=1;i<=NF;i++) if ($i=="dev") {print $(i+1); exit}}')"
if [[ -z "${EGRESS_IF}" ]]; then
  echo "Failed to detect egress interface" >&2
  exit 1
fi

echo "Removing QoS from ${EGRESS_IF}"
tc qdisc del dev "${EGRESS_IF}" root 2>/dev/null || true

echo "Current qdisc state:"
tc qdisc show dev "${EGRESS_IF}"
