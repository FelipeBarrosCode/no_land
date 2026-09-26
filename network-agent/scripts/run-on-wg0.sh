#!/usr/bin/env bash
set -euo pipefail

export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

for _ in $(seq 1 60); do
    wg_address="$(ip -o -4 addr show dev wg0 2>/dev/null | awk 'NR == 1 { split($4, parts, "/"); print parts[1] }' || true)"
    wg_port="$(wg show wg0 listen-port 2>/dev/null || true)"
    instance_id="$(cat /etc/noland-network-agent/instance-id 2>/dev/null || true)"
    if [[ -n "$wg_address" && "$wg_address" != "0.0.0.0" && "$wg_port" =~ ^[0-9]+$ && "$wg_port" -gt 0 && -n "$instance_id" ]]; then
        exec /usr/local/bin/noland-network-agent \
            --udp-addr "0.0.0.0:6201" \
            --ws-addr "${wg_address}:6202" \
            --instance-id "$instance_id" \
            --kernel-wireguard-addr "127.0.0.1:${wg_port}"
    fi
    sleep 2
done

echo "wg0 or the No Land instance identity was not ready after 120 seconds" >&2
exit 1
