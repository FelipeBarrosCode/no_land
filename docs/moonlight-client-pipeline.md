# Noland Moonlight client pipeline map

This document records the pre-optimization streaming architecture and the safety boundaries used by the client-only latency work.

## Moonlight core provenance

`src-tauri/native/moonlight-common-c` is a **vendored source tree**, not a Git submodule. It was added by Noland commit `ee08b05a6b392e0b2a55259896442fbcfada57a5` (`Added the core streaming integration`). A source comparison against upstream identifies `moonlight-stream/moonlight-common-c@703a06946861ff82cd33e5e13c59c1b017f7ded9` as the imported base. The vendored tree contains pre-existing Noland adaptations in `CMakeLists.txt`, `src/PlatformCrypto.c`, `src/PlatformCrypto.h`, `src/RtspConnection.c`, and `src/SdpGenerator.c`, and expands the upstream `enet` and `nanors` submodules into ordinary vendored files.

The latency/rendering phases did not modify `moonlight-common-c`. The later adaptive GameStream packet-size phase adds bounded diagnostic counter updates in `src/RtpVideoQueue.c` for successful data-shard recovery, definitive unrecoverable FEC blocks, accepted out-of-order packets, malformed packets, and corrupt recovered shards. It does not change packet formats, transport behavior, public ABI, or host requirements.

## Callback mode and top-level ownership

`nl_run_connection()` in `src-tauri/native/noland-moonlight/src/noland_moonlight.c` builds `DECODER_RENDERER_CALLBACKS`. It sets `CAPABILITY_PULL_RENDERER` and does not install `submitDecodeUnit`.

The native path is:

```text
React/Tauri UI thread
  -> Rust runtime actor (250 ms native polling cadence)
  -> noland-moonlight C ABI
  -> moonlight-common-c receive/depacketizer threads
  -> bounded Moonlight decode-unit queue (15 compressed frames)
  -> platform renderer pull thread/callback
  -> platform decoder and native presentation surface
```

Frames never traverse JavaScript, Tauri IPC, JSON, or the frontend event bus. Only bounded/coalesced events and aggregate statistics cross the C ABI.

## Queue chain and ownership

### Moonlight core

`VideoDepacketizer.c` owns a linked blocking `decodeUnitQueue` with a hard capacity of 15 complete compressed frames. On overflow, the incoming frame and queued backlog are discarded and the core requests decoder refresh/IDR recovery.

A platform renderer calls `LiWaitForNextVideoFrame()` or `LiPollNextVideoFrame()`. The returned `DECODE_UNIT*` is borrowed until exactly one call to `LiCompleteVideoFrame()`. Noland copies compressed bytes into the platform decoder before completion and never retains the pointer.

`LiGetPendingVideoFrames()` is applicable because Noland uses `CAPABILITY_PULL_RENDERER` rather than direct submit.

### Runtime event queue

`nl_runtime` owns a 64-entry event ring. Consecutive video-frame events coalesce. When full, the oldest event is discarded. This queue is diagnostic/control-plane state, not part of video presentation.

### Windows

```text
Moonlight 15-frame compressed queue
  -> nl_windows_frame_thread (one in-flight DECODE_UNIT)
  -> synchronous Media Foundation H.264 MFT
  -> decoded IMF sample
  -> D3D11 video processor or CPU BGRA conversion
  -> DXGI Present or GDI StretchDIBits
```

The same `nl_windows_frame_thread` pulls, decodes, renders, and presents. Media Foundation internal buffering is opaque. The DXGI flip-discard swap chain has five buffers. The existing colorspace FIFO and D3D11 input-view cache are not presentation queues.

A decoded `IMFSample` becomes eligible to present after `ProcessOutput()` succeeds in `nl_drain_decoder()`. It can be safely discarded before `nl_render_sample()`/`nl_render_software_sample()` by releasing the sample; decoder reference state has already advanced. Arbitrarily discarding a compressed H.264 `DECODE_UNIT` before decode is unsafe.

The current GPU path calls `Present(0, ...)` and may allow tearing. No existing explicit V-Sync or software pacer exists. Presentation belongs to the Windows frame thread.

### macOS

```text
Moonlight 15-frame compressed queue
  -> CVDisplayLink callback
  -> compressed Annex-B to AVCC copy
  -> CMSampleBuffer
  -> synchronous dispatch to AppKit main thread
  -> AVSampleBufferDisplayLayer enqueue
```

`CVDisplayLink` owns frame pulling and may drain multiple Moonlight frames per callback. `AVSampleBufferDisplayLayer` owns opaque decode and presentation queues. Noland can observe sample enqueue but not decoder-output availability or actual scanout. A compressed frame can be discarded safely only by requesting decoder refresh; there is no safe routine post-decode stale-drop hook in the current architecture.

`CACurrentMediaTime()` is used for layer back-pressure durations. `presentationTimeUs` is a stream-relative media timeline and is not numerically comparable to that clock without an explicit mapping.

### Linux

```text
Moonlight 15-frame compressed queue
  -> dedicated pthread
  -> GStreamer appsrc (8 MiB byte cap, nonblocking)
  -> parser
  -> hardware-first decoder (NVDEC/VAAPI/V4L2/etc.) or libav fallback
  -> one-buffer downstream-leaky decoded queue
  -> X11/Wayland video sink
```

The pull pthread copies each compressed access unit into a `GstBuffer`, submits it to `appsrc`, then completes the Moonlight frame. GStreamer owns decoder and sink worker threads. A decoded frame becomes eligible after the decoder and can be safely discarded by the downstream-leaky queue before the sink. The current sink has `sync = FALSE`, so stream PTS is metadata rather than an active presentation clock.

The decoded queue has a hard one-buffer limit and drops the oldest decoded buffer when full. Parser/decoder/sink internal queues are backend-specific.

## Platform implementations present

- Windows: Media Foundation H.264 decode, D3D11 video processing/DXGI presentation, software MF/GDI fallback.
- Apple: `AVSampleBufferDisplayLayer` with CoreMedia and `CVDisplayLink`; no explicit VideoToolbox output callback or Metal renderer.
- Linux: GStreamer with NVDEC/VAAPI/V4L2/OMX candidates, libav software fallback, and X11/Wayland sinks.
- Android/MediaCodec: not present.
- Separate Vulkan renderer: not present.

## Timing domains

- `receiveTimeUs`, `enqueueTimeUs`, and `LiGetMicroseconds()` share Moonlight's monotonic timing domain. Windows uses QPC; Apple uses `CLOCK_UPTIME_RAW`; Linux prefers `CLOCK_MONOTONIC_RAW`/`CLOCK_MONOTONIC`.
- `presentationTimeUs` is stream-relative with an epoch at the first captured frame. It must be mapped to a local presentation epoch before deadline comparison.
- Windows QPC, `CACurrentMediaTime()`, and GStreamer running time remain platform-local timing domains unless explicitly anchored.
- Wall clock is not used for stream duration/deadline decisions.

## Existing statistics and publication

`nl_stats_t` exposes lifecycle counts, RTT, compressed-frame counts, renderer submit/drop counts, audio/input counts, and last-frame Moonlight metadata. Rust reads it every 250 ms and publishes aggregate `moonlight://statistics` events. No UI subscriber is required for streaming.

The existing `renderer_submitted_frame_count` means the platform submit function returned `DR_OK`; it is not proof of decode completion, presentation, or scanout. Per-frame video events are coalesced in the fixed event ring and no longer format a per-frame message; packet-size adaptation uses only periodic aggregate snapshots.

## Current smoothness/latency modes

The embedded preference model has network and reconnection settings but no active frame-pacing or decoded-frame smoothing setting. Linux already uses a one-buffer downstream-leaky decoded queue as a fixed low-latency sink boundary. macOS retains one compressed Moonlight frame while draining a display-link callback. Neither is exposed as a user-selected smoothing mode.

The optimization defaults must therefore preserve these behaviors: adaptive dropping and decoder back-pressure policy remain disabled until measured, pacing preserves the existing mode, and the optional 0–3-frame smoothing reserve defaults to off.

## Safety decisions

1. Routine stale dropping is never performed on compressed reference-dependent `DECODE_UNIT`s.
2. Windows and Linux may drop only after decode, where frame release is explicit and reference state is preserved.
3. macOS adaptive post-decode dropping remains unavailable until the renderer exposes decoded-surface ownership (for example, an explicit VideoToolbox/Metal path).
4. The optional smoothing reserve is bounded to three decoded frames and cannot be silently enabled in the lowest-latency mode.
5. Renderer/GPU calls are never made while holding the Rust/UI lock.
6. Session teardown must wake pull threads, flush platform queues, reset timing epochs, and release queued surfaces before a reconnect presents frames.
