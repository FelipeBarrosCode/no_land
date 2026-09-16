# No Land Network Agent

`noland-network-agent` is a small, ephemeral network quality agent. It responds to authenticated UDP probes and accepts client-side RTT/loss reports over WebSocket. All session tokens and telemetry are held in RAM only.

## Run

```sh
cargo run --release
```

Defaults:

- UDP: `127.0.0.1:6201`
- WebSocket: `127.0.0.1:6202`
- Maximum sessions: 128 (oldest registration is evicted when full)
- UDP response rate: 20 packets/second/session

Use `cargo run -- --help` for all CLI options. Core settings can also be supplied through:

```text
NOLAND_UDP_ADDR
NOLAND_WS_ADDR
NOLAND_MAX_SESSIONS
NOLAND_UDP_RATE_LIMIT
```

Classifier thresholds have matching `NOLAND_*` environment variables shown by `--help`, including loss, jitter, p95-minus-median spread, spike percentage, and median latency boundaries.

## WebSocket protocol

The first client message registers or replaces an ephemeral session:

```json
{"type":"auth","sessionId":"d2719f0b-cbd0-4d49-a5d7-a49b08aa22d8","token":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
```

The token must be exactly 32 bytes encoded as 64 case-insensitive hexadecimal characters. Re-registering a session ID resets its samples, classifier state, and UDP rate limiter.

Clients normally submit one report per second, with at most 100 samples:

```json
{"type":"measurement_report","sessionId":"d2719f0b-cbd0-4d49-a5d7-a49b08aa22d8","samples":[{"sequence":501,"rttMs":18.31,"lost":false},{"sequence":503,"rttMs":null,"lost":true}]}
```

A successful sample requires a finite, non-negative `rttMs`; a lost sample requires `rttMs` to be null or omitted. `{"type":"get_state"}` requests an immediate `stats_update`. The server also evaluates active connections once per second. It sends `stats_update` after each report/evaluation and `status_changed` only when the published hysteresis state changes.

## UDP probe packet

Packets are exactly 48 bytes:

| Bytes | Value |
| --- | --- |
| 0..4 | ASCII `NLND` |
| 4 | Version `1` |
| 5 | Type: `1` probe, `2` ACK |
| 6..8 | Reserved, zero |
| 8..16 | Sequence, unsigned 64-bit big endian |
| 16..32 | Session UUID raw bytes |
| 32..48 | First 16 bytes of HMAC-SHA256 over bytes 0..32 |

The HMAC key is the registered 32-byte session token. An ACK copies sequence/session, changes the type to `2`, and recalculates the tag. Malformed, unknown-session, over-rate, and unauthenticated packets are ignored.

## Retention and classification

Each session retains at most five minutes and 1,500 samples in memory. Statistics include RTT distribution, RFC3550-style EWMA jitter, loss, spikes, and loss bursts. Classification uses the last 30 seconds and remains `WARMING_UP` until both five seconds and 25 samples are available. A candidate `BAD` state must persist for five seconds; recovery requires ten continuous non-`BAD` seconds.

## Security

The first WebSocket `auth` message is **session registration**, not proof of possession against a pre-provisioned credential. Anyone who can reach the WebSocket listener can replace a session token. Do not expose this service directly to an untrusted network. Bind both transports to a WireGuard address or another trusted/private interface and enforce network-level access controls.
