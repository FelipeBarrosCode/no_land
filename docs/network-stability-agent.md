# Network stability agent

No Land measures the real client-to-stream-host path with authenticated UDP probes while keeping all session telemetry in RAM on the gaming instance.

## Components

- `network-agent/`: standalone `noland-network-agent` daemon installed beside Sunshine.
- `src-tauri/src/network_monitor/`: client probe loop, measurement reporter, status event bridge, and three-second disconnect fail-safe.
- `src/features/moonlight/StreamWindowScreen.tsx`: native notification, warning sound, and non-blocking stream banner.

The client must originate probes because a gaming instance generally cannot initiate traffic through a user's NAT, CGNAT, or firewall. The client measures RTT with `std::time::Instant`; the instance owns the rolling history, metrics, classification, and BAD-state hysteresis.

Monitoring is observational. Agent installation, UDP, WebSocket, classification, notification, or UI failures do not stop or alter Moonlight streaming.

## Network path and ports

The systemd service resolves the IPv4 address assigned to `wg0` and binds only to that address:

- UDP `6201`: 48-byte authenticated probe and ACK packets.
- WebSocket `6202`: session registration, one-second measurement reports, statistics, and status changes.

No public firewall ports are opened. Do not bind these listeners to `0.0.0.0` on rented instances. The initial WebSocket authentication message registers an ephemeral token; it is not proof against an attacker who can already reach the private listener.

## Sampling and retention

- Probe interval: 200 ms (five probes per second).
- Probe timeout: one second.
- Measurement report: once per second, at most 100 samples.
- Classifier window: 30 seconds.
- Statistics window: 60 seconds.
- In-memory retention: five minutes or 1,500 samples per session.
- Warmup: at least five seconds and 25 samples.

The agent calculates current RTT, min/average/median/max, P95/P99, RFC3550-style EWMA jitter, loss percentage, spike ratio, and longest loss burst. A spike is RTT above `median + max(15 ms, jitter × 3)`.

## Classification

Latency quality and stability are calculated separately. Overall status is the worse result.

Default latency thresholds use median RTT:

- `GREAT`: below 30 ms
- `GOOD`: below 60 ms
- `POOR`: below 100 ms
- `BAD`: 100 ms or higher

Default stability boundaries:

| Signal | GREAT | GOOD | POOR | BAD |
| --- | ---: | ---: | ---: | ---: |
| Packet loss | ≤0.1% | ≤0.5% | ≤2% | >2% |
| Jitter | ≤3 ms | ≤7 ms | ≤15 ms | >15 ms |
| P95 − median | ≤10 ms | ≤20 ms | ≤40 ms | >40 ms |
| Spike ratio | ≤1% | ≤3% | ≤10% | >10% |

All boundaries are configurable through the `NOLAND_*` environment variables documented by `noland-network-agent --help`.

A candidate `BAD` classification must persist for five seconds before publication and notification. A BAD episode emits one alert. Ten continuous non-BAD seconds are required to publish recovery and re-arm alerting. Separately, the client emits one `CONNECTION_LOST` BAD event after three seconds without any valid ACK, because the server cannot report through a failed path.

## Tauri events

The client emits:

- `network-monitor://stats`: the latest agent `stats_update` payload.
- `network-monitor://status`: agent status changes and client fail-safe transitions.
- `network-monitor://error`: monitor transport or protocol errors.

`network_monitor_get_state` returns the latest in-memory `stats_update` to the frontend. No network history is persisted to disk.

## Remote deployment

Provisioning bundles `network-agent/Cargo.toml`, `Cargo.lock`, and `src/**`, builds a release binary on the instance, and installs:

- `/usr/local/bin/noland-network-agent`
- `/usr/local/libexec/noland-network-agent-bind-wg0`
- `/etc/systemd/system/noland-network-agent.service`

The hardened systemd service waits for `wg0`, runs without persistent state, and restarts independently. Provisioning is best-effort and non-blocking for No Land setup. Stream startup also schedules a background ensure operation so instances created by older clients can gain the agent without delaying play.

## Network impairment testing

Run impairment on a dedicated test instance/interface, not a production session. Replace `wg0` if the tested stream uses a different interface.

```sh
# Constant 60 ms delay
sudo tc qdisc replace dev wg0 root netem delay 60ms

# Variable latency
sudo tc qdisc replace dev wg0 root netem delay 20ms 20ms distribution normal

# Three percent packet loss
sudo tc qdisc replace dev wg0 root netem loss 3%

# Delay plus loss
sudo tc qdisc replace dev wg0 root netem delay 80ms 25ms loss 3%

# Remove impairment
sudo tc qdisc del dev wg0 root
```

Validate at minimum:

1. Stable 15 ms classifies GREAT after warmup.
2. Stable 60 ms remains stable but has POOR latency under the strict `<60 ms` boundary.
3. Three percent loss reaches BAD after confirmation.
4. A two-second disturbance does not notify.
5. A BAD episode longer than five seconds notifies once.
6. Ten healthy seconds re-arm alerts.
7. Complete loss triggers the client fail-safe in about three seconds.
