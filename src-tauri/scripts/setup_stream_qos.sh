#!/usr/bin/env bash
set -euo pipefail

# Noland stream QoS (egress)
#
# Purpose:
# - Keep Sunshine/Moonlight latency stable under competing host traffic.
# - Prioritize Sunshine UDP stream/control/audio ports over bulk transfers.
#
# Defaults are conservative and intended for Ubuntu 22.
# Override with env vars when needed, for example:
#   RATE_KBIT=80000 STREAM_MIN_KBIT=25000 ./setup_stream_qos.sh

RATE_KBIT="${RATE_KBIT:-90000}"
STREAM_MIN_KBIT="${STREAM_MIN_KBIT:-20000}"
INTERACTIVE_MIN_KBIT="${INTERACTIVE_MIN_KBIT:-5000}"
BULK_MIN_KBIT="${BULK_MIN_KBIT:-1000}"

if [[ "${RATE_KBIT}" -lt 5000 ]]; then
  echo "RATE_KBIT is too low: ${RATE_KBIT}" >&2
  exit 1
fi

EGRESS_IF="$(ip route get 1.1.1.1 | awk '{for (i=1;i<=NF;i++) if ($i=="dev") {print $(i+1); exit}}')"
if [[ -z "${EGRESS_IF}" ]]; then
  echo "Failed to detect egress interface" >&2
  exit 1
fi

echo "Applying stream QoS on ${EGRESS_IF}"
echo "RATE_KBIT=${RATE_KBIT} STREAM_MIN_KBIT=${STREAM_MIN_KBIT}"

# Root scheduler with class hierarchy.
tc qdisc replace dev "${EGRESS_IF}" root handle 1: htb default 30
tc class replace dev "${EGRESS_IF}" parent 1: classid 1:1 htb rate "${RATE_KBIT}kbit" ceil "${RATE_KBIT}kbit"

# 1:10 stream, 1:20 interactive, 1:30 bulk/default
tc class replace dev "${EGRESS_IF}" parent 1:1 classid 1:10 htb rate "${STREAM_MIN_KBIT}kbit" ceil "${RATE_KBIT}kbit" prio 0
tc class replace dev "${EGRESS_IF}" parent 1:1 classid 1:20 htb rate "${INTERACTIVE_MIN_KBIT}kbit" ceil "${RATE_KBIT}kbit" prio 1
tc class replace dev "${EGRESS_IF}" parent 1:1 classid 1:30 htb rate "${BULK_MIN_KBIT}kbit" ceil "${RATE_KBIT}kbit" prio 2

# Queue management per class.
tc qdisc replace dev "${EGRESS_IF}" parent 1:10 handle 110: fq_codel
tc qdisc replace dev "${EGRESS_IF}" parent 1:20 handle 120: fq_codel
tc qdisc replace dev "${EGRESS_IF}" parent 1:30 handle 130: fq_codel

# Remove existing filters for deterministic/idempotent behavior.
tc filter del dev "${EGRESS_IF}" parent 1: protocol ip prio 1 2>/dev/null || true
tc filter del dev "${EGRESS_IF}" parent 1: protocol ip prio 2 2>/dev/null || true
tc filter del dev "${EGRESS_IF}" parent 1: protocol ip prio 3 2>/dev/null || true

# Sunshine stream/control/audio + RTSP to high-priority class.
for PORT in 47998 47999 48000; do
  tc filter add dev "${EGRESS_IF}" protocol ip parent 1: prio 1 u32 \
    match ip protocol 17 0xff match ip sport "${PORT}" 0xffff flowid 1:10
  tc filter add dev "${EGRESS_IF}" protocol ip parent 1: prio 1 u32 \
    match ip protocol 17 0xff match ip dport "${PORT}" 0xffff flowid 1:10
done

tc filter add dev "${EGRESS_IF}" protocol ip parent 1: prio 1 u32 \
  match ip protocol 6 0xff match ip dport 48010 0xffff flowid 1:10
tc filter add dev "${EGRESS_IF}" protocol ip parent 1: prio 1 u32 \
  match ip protocol 6 0xff match ip sport 48010 0xffff flowid 1:10

# DNS and ICMP in interactive class for responsiveness.
tc filter add dev "${EGRESS_IF}" protocol ip parent 1: prio 2 u32 \
  match ip protocol 17 0xff match ip dport 53 0xffff flowid 1:20
tc filter add dev "${EGRESS_IF}" protocol ip parent 1: prio 2 u32 \
  match ip protocol 1 0xff flowid 1:20

echo "QoS applied on ${EGRESS_IF}."
tc -s class show dev "${EGRESS_IF}"
tc -s qdisc show dev "${EGRESS_IF}"
