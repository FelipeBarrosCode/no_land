# Moonlight client-only latency optimizations

## Scope and compatibility

These changes stay in Noland's client adapter, platform decoders/renderers, Rust runtime actor, diagnostic UI, and a small video-stat instrumentation patch in the vendored client core. They do not modify Sunshine, require a new host capability, add a transport, send active MTU probes, or move frames through Tauri/React.

The adaptive packet-size phase updates `moonlight-common-c/src/RtpVideoQueue.c` only to populate already-public diagnostic counters. It does not change the core ABI, GameStream packet format, or host protocol. See [`moonlight-client-pipeline.md`](./moonlight-client-pipeline.md) for provenance and ownership.

## Source comparisons

### Native telemetry and adaptive late-drop policy

- Reference repository: `drunkitguy/artemis-apollo2`
- Reference branch/commits: `roadmap-latency`, including `361f9eaee11ec4c13c5294f69df5905aa25c2a0c` and branch tip `593d2ff8c2c8b3e73d0f3be8bf78531efd197cbe`
- Reference file: `MediaCodecDecoderRenderer.java`
- Noland equivalents: `noland_latency_telemetry.*`, `noland_frame_deadline_policy.*`, and platform renderer integration
- Behavior copied: bounded decoder/render timing, concrete back-pressure state, severe-lateness/late-streak gates, newer-decoded-frame requirement, cooldown, and smoothing-mode exclusion
- Behavior intentionally not copied: host/input trace echo and custom Artemis core changes
- Adaptation: Windows drops only post-decode `IMFSample`s. Linux currently observes post-decode backlog but leaves adaptive dropping inactive because its existing GStreamer path cannot prove the deadline, newer-frame, and back-pressure gates at one safe drop point. macOS leaves adaptive dropping inactive because `AVSampleBufferDisplayLayer` does not expose decoded output ownership.

### Windows frame pacing

- Reference repository: `FoggyBytes/StreamLight`
- Reference commit: `65ae7ff6ea493be19b49196b22f4846821f10118`
- Reference file: `app/streaming/video/ffmpeg-renderers/d3d11va.cpp`
- Safety reference: upstream Moonlight-Qt pacer
- Noland equivalent: `noland_video_renderer_windows.cpp`
- Behavior copied: explicit off/automatic/software/hardware-multiple modes, V-Sync-off effective mode, integer refresh/FPS hardware cadence, monotonic presentation measurement/waits, and software fallback for fractional automatic ratios
- Behavior intentionally not copied: StreamTweak/host integration and experimental fractional hardware cadence
- Adaptation: DXGI sync intervals are used only for supported integer ratios 1–4. Software waits use `LiGetMicroseconds()` and an interruptible stop event. The GDI fallback retains its existing immediate behavior.

### Optional decoded-frame reserve and reconnect

- Reference repository: `XITRIX/Moonlight-Switch`
- Reference tag: `v1.3.0` (`99c56b6` observed release commit)
- Reference files: `AVFrameHolder.*`, `MoonlightSession.cpp`, and `Settings.hpp`
- Noland equivalents: Windows fixed decoded-sample ring, Linux bounded downstream-leaky decoded queue, and the Rust runtime actor
- Behavior copied: explicit 0–3 reserve modes, oldest-decoded-frame overflow policy, natural last-image retention on underflow, queue flush on teardown/reconnect, and one reconnect after unexpected non-zero termination
- Behavior intentionally not copied: raw `AVFrame*` ownership and unbounded/recursive retry behavior
- Adaptation: Windows retains `IMFSample` references with `ComPtr`; Linux lets GStreamer own buffers and limits the decoded queue to one mandatory slot plus 0–3 reserve slots. Reconnect is actor-owned, generation-guarded, and always calls `LiStopConnection()` without requesting host-app termination before restarting the same request.

### Remote-safe and adaptive GameStream packet sizing

- Reference: upstream `moonlight-common-c` `STREAM_CONFIGURATION.packetSize`, `streamingRemotely`, SDP packet-size negotiation, and RTP/FEC statistics contracts
- Noland equivalents: `adaptive_packet_size.rs`, the Rust runtime actor, `resolve_remote_stream_config()`, and final selection in `nl_run_connection()`
- Static behavior with adaptation disabled: forced remote requests `STREAM_CFG_REMOTE` and the configured remote size (1024 by default); explicit local preserves its configured size; auto retains upstream classification
- Adaptive behavior when explicitly enabled: classify a managed tunnel/direct path, treat unverified private routes conservatively as remote, use interface or tunnel MTU only as a hint, apply a cached or ladder candidate before `LiStartConnection()`, observe aggregate RTP/FEC plus RTT/variance, and perform a controlled client reconnect only after three strong stable-RTT bad windows
- Explicit `STREAM_CFG_REMOTE` retains WAN optimizations while allowing the client-selected packet size; no `STREAM_CFG_LOCAL` workaround or core ABI extension is used
- The packet-size value is negotiated at stream setup and is never mutated in place during a running connection
- This controller is not true PMTU discovery. It sends no ICMP, DF-bit, raw-socket, UDP payload, Sunshine RPC, or sidecar probe. A connected UDP socket is used only to ask the OS which source/interface route would be selected.

## Unified configuration

`StreamPreferences.latency` contains:

```text
telemetryEnabled
adaptiveLateFrameDropEnabled
adaptivePacketSizeEnabled
decoderBackpressurePolicyEnabled
pacingMode: off | automatic | software | hardwareMultiple
frameBufferMode: off | oneFrame | twoFrames | threeFrames
autoReconnectOnUnexpectedTermination
remoteStreamMode: auto | forceRemote | forceLocal
remotePacketSize
lateFrameToleranceUs
vsyncEnabled
```

`VideoPreferences.clientRefreshRateX100` carries display refresh independently from stream FPS (for example, 12000 for 120 Hz). A zero value preserves compatibility by falling back to `stream FPS * 100`; callers that know the actual display rate should provide it so integer-multiple hardware pacing can resolve 60→120/240 and 120→240 correctly.

Safety defaults:

- telemetry: enabled only in debug builds;
- adaptive late drop: off;
- adaptive GameStream packet sizing: off pending live path/impairment validation;
- adaptive decoder/back-pressure policy: off;
- pacing: off, preserving the previous presenter behavior;
- smoothing reserve: off;
- reconnect: enabled, but hard-limited to one immediate attempt per failure episode; configuration validation currently requires `maximumAttempts = 1` and both delay values to be zero when reconnect is enabled; migration normalizes the previously shipped `3 / 500 ms / 5000 ms` defaults to that supported contract; a successful failure reconnect must remain stable for 30 seconds before the allowance resets;
- remote mode: derived from the existing network mode; Noland's default managed remote/tunnel configuration resolves to forced remote and 1024 bytes;
- late tolerance: derived as half a negotiated stream frame when zero.

Configuration invariants:

- a non-off smoothing reserve and adaptive late drop cannot both be enabled;
- V-Sync off requires effective pacing off;
- forced-remote packet sizes must be 960–1392 and divisible by 16; the default remains 1024;
- adaptive candidates are restricted to the fixed ladder `1392, 1280, 1152, 1088, 1024, 960`;
- a user stop clears desired-running and pending packet-reconnect state before native teardown, suppressing reconnect.

## Telemetry

The native collector owns a fixed 1,200-record ring (five seconds at 240 FPS). Each POD record has validity bits and may contain:

- first-packet receive time;
- Moonlight complete-frame enqueue time;
- decoder submission time;
- decoder output time where exposed;
- stream presentation timestamp;
- render submission time;
- queue depths, back-pressure, lateness, and local drop reason.

The collector samples upstream RTP/FEC counters at the existing 250 ms Rust polling cadence. Lightweight network sampling remains active when the optional per-frame timing ring is disabled. Unsigned subtraction provides wrap-safe interval deltas. No packet or frame JSON is created in the hot path. The existing video-frame event no longer formats a per-frame string and remains coalesced in the fixed 64-event native ring.

The stream window listens to aggregate `moonlight://statistics` events. The diagnostic panel appears when native timing telemetry or adaptive packet sizing is active. It shows stream/display rates, pacing, queues, decode/render dwell, RTP/FEC deltas, local drop reasons, back-pressure, smoothing budget, reconnect totals, path classification/MTU hint, controller state/confidence, and selected packet size.

`renderSubmitTimeUs` means the platform's render submission point (`Present`, GDI submission, pre-sink buffer, or AV sample-layer enqueue). It is not labeled scanout time.

## Queue budgets

- Moonlight complete compressed frames: 15 (upstream core).
- Native control/event ring: 64 coalesced events.
- Telemetry records: 1,200.
- Windows decoded smoothing reserve: 0–3 `IMFSample` references, plus the current output while deciding/rendering.
- Linux decoded queue: one mandatory downstream-leaky slot plus 0–3 optional reserve slots.
- macOS: opaque `AVSampleBufferDisplayLayer` queue; no additional Noland smoothing queue is enabled.

Maximum configured reserve budget is reported as:

```text
reserve frames * 1,000 / stream FPS milliseconds
```

At 60 FPS, three reserve frames are at most 50 ms of configured reserve; at 120 FPS, at most 25 ms. This is a budget, not a claim that every frame experiences the full delay.

## Reconnect state and cleanup

The Rust actor retains the current native request and tracks:

- desired-running/user-stop intent;
- monotonically increasing session generation;
- one-attempt-used state;
- reconnect-in-flight state.

Only a non-zero `NL_EVENT_TERMINATED` from the current streaming generation is eligible. Cleanup is:

```text
connection terminated
  -> Reconnecting
  -> LiInterruptConnection/LiStopConnection
  -> renderer/decoder/pacer/smoothing cleanup callbacks
  -> discard old-generation events
  -> start same request with a new generation
  -> Streaming on NL_EVENT_CONNECTED, otherwise Idle
```

A graceful host termination, explicit Stop/Disconnect, replacement stream, or shutdown does not reconnect. Every terminal connection still runs native cleanup even when no reconnect is attempted.

## Platform limitations

### Windows

- Current decoder support remains H.264 Media Foundation only.
- The runtime accepts an independent display refresh value, but current window orchestration does not yet auto-detect monitor refresh; zero falls back to stream FPS.
- Hardware cadence uses DXGI-supported sync intervals 1–4.
- Fractional automatic cadence uses the software fallback.
- GDI fallback has telemetry but no new software pacing.
- Media Foundation calls and hardware-VSync `Present` cannot themselves be interrupted; explicit software waits are interruptible.

### Linux

- Decoder and pre-sink timing are available through GStreamer pad probes.
- `sync=FALSE` is intentionally preserved because no validated Moonlight-PTS to GStreamer-running-time mapping exists yet.
- Adaptive deadline dropping remains disabled on this backend. The existing bounded downstream-leaky decoded queue continues to shed old decoded output under sink pressure.
- Pre-sink timing is sink submission, not confirmed compositor scanout.

### macOS

- `AVSampleBufferDisplayLayer` keeps decode/output ownership opaque, so decoder-output timing and safe post-decode stale dropping are unavailable.
- Noland records sample-layer enqueue and concrete `readyForMoreMediaData` back-pressure durations.
- No Metal/VideoToolbox renderer or unverified analogous pacing algorithm was introduced.
- Existing macOS 15 SDK deprecation warnings for `CVDisplayLink` and `AVSampleBufferDisplayLayer` APIs remain; migration is separate work.

## Validation performed

Automated on macOS:

- native C/C++/Objective-C build;
- native smoke test;
- deterministic deadline policy tests at 60/120/144/240 FPS;
- telemetry wrap, missing-timestamp, reset, disabled-path, and 1,200-record bound tests;
- Rust Moonlight domain/runtime/controller tests, including path/cache bounds, MTU mapping, congestion suppression, generation guards, packet-field selection, controlled-reconnect state transitions, and manual-stop eligibility;
- Cargo type/ABI integration check;
- frontend TypeScript production build.

Windows C++17 syntax validation was performed with MinGW. Linux compilation and live Windows/Linux presentation tests require their target build environments.

No live Sunshine stream, adaptive packet-size reconnect lifecycle test, active path probe, 60/120+ FPS display run, network impairment run, or before/after latency measurement was performed in this development environment. Therefore this document does not claim a measured latency/packet-loss improvement, a discovered PMTU, or confirmed live-host regression result.

## Repeatable live validation matrix

Use the same host, app, codec, bitrate, and FPS for every comparison. Capture the aggregate diagnostics at one-second intervals and note visible corruption/freezes and A/V sync.

1. Baseline: no added latency/jitter/loss.
2. Constant WAN latency: approximately 40 ms path delay, low jitter, no loss.
3. Mild jitter: 3–5 ms, no loss.
4. Strong jitter bursts: 15–25 ms, 0–1% loss.
5. Random loss: 1%.
6. Burst loss.
7. Temporary bandwidth clamp below configured bitrate, then restore.
8. Local decoder/GPU/presentation stall for 2–3 frame intervals.
9. Connectivity interruption long enough to trigger termination, then restore; verify one attempt and no host-app quit.
10. Manual Disconnect during normal streaming and during reconnect; verify zero additional attempts.
11. Direct path versus managed WireGuard/VPN path with identical stream settings.
12. 60→60, 60→120, 60→240, 120→240, and 60→165 display/stream combinations on Windows.
13. H.264, HEVC, and AV1 where each platform renderer actually supports the codec.

Capture at least median/p95/max render-queue dwell, decoder time, back-pressure time, rendered FPS, late/stale/pacer/smoothing drops, RTP/FEC deltas, reconnect result, resolved remote mode, and packet size. Do not declare success from average FPS alone.
