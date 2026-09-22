#!/usr/bin/env bash
set -euo pipefail

export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

for _ in $(seq 1 60); do
    wg_address="$(ip -o -4 addr show dev wg0 2>/dev/null | awk 'NR == 1 { split($4, parts, "/"); print parts[1] }' || true)"
    if [[ -n "$wg_address" && "$wg_address" != "0.0.0.0" ]]; then
        exec /usr/local/bin/noland-network-agent \
            --udp-addr "${wg_address}:6201" \
            --ws-addr "${wg_address}:6202"
    fi
    sleep 2
done

echo "wg0 has no IPv4 address after 120 seconds" >&2
exit 1
