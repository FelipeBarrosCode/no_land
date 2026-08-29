# Adaptive GameStream packet-size controller

## Scope

Noland adapts the GameStream video `packetSize` selected before `LiStartConnection()`. Sunshine receives that value through the existing GameStream SDP setup and requires no modification.

This controller is **not active PMTU discovery**:

- it sends no ICMP probes;
- it sends no UDP probe payloads;
- it does not use a DF-bit binary search;
- it does not require a Sunshine RPC or Noland VM sidecar;
- it never mutates packet size in place during an active stream.

A connected, payload-free UDP socket is used only to ask the local OS which source address and interface it would route toward the destination. Interface and managed-tunnel MTUs are hints rather than claims about end-to-end PMTU.

The feature is controlled by:

```text
StreamPreferences.latency.adaptivePacketSizeEnabled
```

It defaults to `false` pending live path and impairment validation.

## Startup selection

At stream start the Rust runtime:

1. Resolves the existing Noland remote mode.
2. Looks for a fresh active managed GotaTun `status.json` whose `allowedIps` contains the Moonlight destination.
3. Verifies the referenced WireGuard configuration fingerprint before reading its configured tunnel MTU.
4. For direct paths, treats loopback/link-local destinations as local, public destinations as remote, and unverified private/ULA destinations conservatively as remote. A private address is not considered proof of an on-link LAN; `ForceLocal` remains available when the caller has positive local-path knowledge.
5. Derives a route source address and, where supported, interface name and interface MTU without transmitting a probe payload.
6. Hashes safe path identity into a SHA-256 fingerprint.
7. Loads a matching non-expired cache entry or chooses a conservative initial candidate.
8. Applies the value before `LiStartConnection()`.

Candidates are fixed and 16-byte aligned:

```text
1392, 1280, 1152, 1088, 1024, 960
```

Normal WAN and unknown paths begin at 1024. A managed tunnel with its current default MTU of 1280 also begins at 1024. An explicitly or positively classified local direct path with an interface MTU of at least 1500 may begin at 1392. The displayed value is an interface/tunnel **MTU hint**, not an end-to-end path MTU.

Explicit `STREAM_CFG_REMOTE` is retained for remote/tunnel paths. In this vendored Moonlight core, automatic address-based capping occurs only for `STREAM_CFG_AUTO`, so explicit remote mode preserves WAN behavior while honoring Noland's selected packet size. Noland does not mislabel a WAN/tunnel path as local to bypass the cap.

## Runtime observation

The native layer samples the existing `RTP_VIDEO_STATS` counters at the 250 ms runtime cadence. Video queue instrumentation populates:

- successfully recovered data shards;
- definitive unrecoverable FEC block events;
- accepted out-of-order packets;
- malformed video packets;
- corrupt recovered FEC shards.

Network counters remain active when the optional per-frame timing ring is disabled.

The Rust controller accumulates bounded counters into elapsed 500 ms windows. A window's packet-size suspicion score uses strong evidence such as:

- an unrecoverable FEC block;
- invalid FEC output;
- a significant malformed-packet rate;
- a sustained high FEC recovery ratio.

Out-of-order packets alone cannot trigger a size change.

RTT and RTT-variance baselines are established and maintained only from healthy windows. Loss-bearing startup windows cannot become the baseline. Until a healthy RTT baseline exists, packet evidence cannot advance the bad-window streak. If RTT or variance later grows materially, the window is classified as congestion-like and does not advance the packet-size bad-window streak. This is conservative classification, not proof that a remaining event was caused by MTU.

## Downshift and reconnect

A downshift requires three consecutive strong, non-congestion windows after the validation cooldown. It moves one step down the fixed ladder and never below 960.

Because packet size is negotiated during setup, the runtime uses a two-phase controlled reconnect:

```text
Streaming
  -> packet-size decision
  -> Reconnecting
  -> stop native Moonlight connection without quitting host app
  -> drain old-generation teardown events
  -> enqueue bounded internal restart command
  -> apply target packet size to retained request
  -> increment session generation
  -> LiStartConnection()
  -> Streaming after matching CONNECTED
```

The internal restart command shares the runtime actor's bounded FIFO. A manual `Stop` already queued ahead of it clears desired-running and pending reconnect state, causing the internal command to be ignored. Packet-size reconnects do not consume the separate unexpected-termination reconnect allowance. That failure policy supports exactly one immediate attempt per episode; unsupported multi-attempt/delay settings are rejected during preference validation. A successful failure reconnect must remain stable for 30 seconds before a later termination receives a fresh one-shot allowance, preventing rapid reconnect flapping.

No automatic upshift occurs during the same stream.

## Cache

Cache location:

```text
<app-data>/moonlight/network-path-cache.json
```

Properties:

- schema version 1;
- atomic writes;
- maximum 128 entries;
- 30-day TTL;
- no private keys, credentials, or full host secrets;
- path identity stored as a SHA-256 fingerprint;
- selected/last-good/last-bad packet sizes;
- bounded confidence and successful-session count;
- MTU hint and upshift-probe timestamp.

A committed downshift is persisted immediately using a unique same-directory temporary file and replace-existing operation so a process failure does not retry the known-bad larger candidate. Cache reads are capped at 1 MiB before JSON deserialization. After 30 healthy seconds, the selected value is recorded as good once per connection generation and confidence increases.

An upshift may only be selected for a future session when all conditions hold:

- confidence is at least 0.8;
- at least four successful sessions were recorded;
- at least seven days elapsed since the prior upshift probe;
- the current MTU-derived cap permits the next ladder value.

## Diagnostics

Aggregate runtime statistics expose:

```text
adaptivePacketSizeEnabled
packetSizeControllerState
packetPathLabel
packetPathMtuHint
packetSizeLastGood
packetSizeBadWindowCount
packetSizeConfidence
packetPathFingerprint
adaptivePacketReconnectCount
requestedPacketSize
resolvedRemoteStreamMode
```

The UI receives these through the existing coalesced `moonlight://statistics` snapshot. There is no per-packet or per-frame adaptive IPC.

## Validation still required

Before enabling the feature by default, validate with an unmodified Sunshine host:

1. Managed WireGuard MTU 1280 starts at 1024.
2. Known local direct MTU 1500 starts at 1392.
3. Stable WAN RTT with injected fragmentation/loss-like behavior produces a bounded downshift.
4. Rising RTT/jitter under bandwidth congestion suppresses packet-size reconnects.
5. Exactly three eligible bad windows are required.
6. Manual disconnect before the internal restart prevents reconnection.
7. Packet-size reconnect preserves the running host application.
8. Old-generation events and frames never appear after reconnect.
9. Cache values remain separated across route/source/interface/tunnel changes.
10. H.264, HEVC, and AV1 startup remain compatible where supported.
11. Compare direct and tunnel RTP/FEC results at identical bitrate/FPS.

Do not claim a discovered PMTU or packet-loss improvement without live measurements. A future acknowledged VM-side UDP probe service would be a separate, host-dependent PLPMTUD phase.
