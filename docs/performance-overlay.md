# In-stream performance overlay

Each rented-instance card has a **Performance overlay** switch. It saves the
instance's choice and applies to a running stream on the next 250 ms UI tick,
without reconnecting. Settings → Default Performance Overlay supplies the value
for instances without an explicit choice.

The actual stream window is a native Tauri `Window`, not a `WebviewWindow`.
`StreamWindowScreen.tsx` is a legacy prototype and does not render in that window.
The production overlay uses a non-interactive AppKit text layer on macOS, a
click-through Win32 child on Windows, and a GStreamer `textoverlay` composited
into the video pipeline on Linux (the only reliable stacking above
X11/Wayland video sinks). Input continues to target the stream. Widgets are
owned by the stream window and are destroyed with it. Updates are dispatched on
the UI thread with at most one update outstanding; Linux text updates are
thread-safe GStreamer property writes routed through the runtime actor.

## Measurement definitions

| Display | Source / meaning |
| --- | --- |
| Resolution, codec, target FPS | Native negotiated video setup |
| Incoming FPS | Complete frames pulled from Moonlight's core queue |
| Decoded FPS | Matched decoder-output callbacks (Windows/Linux) |
| Submitted FPS | Submission to DXGI/GDI, GStreamer sink, or AV sample layer; not scanout |
| Video Mbps | Complete encoded video bytes, excluding audio, FEC and transport overhead |
| Stream RTT / RTT variation | Moonlight control-channel ENet estimates; variation is not probe jitter |
| Host processing | Host-provided per-frame processing latency, converted from tenths of milliseconds |
| Reassembly | Complete-frame enqueue minus first-packet receive time |
| Decode | Decoder submission to decoder output; includes time inside the platform pipeline |
| Render queue | Decoder output to render submission; excludes monitor scanout |
| Missing frames | Frame-number gaps before decoding; may include core queue discards, not a packet-loss percentage |
| Local drops | Session counts from adaptive, pacer, renderer/decoder and smoothing drop counters |
| FEC | Recovered data packets and failed FEC blocks in the latest native sampling interval |
| Probe RTT, jitter, loss, P95 | Existing network-agent UDP probes; loss and P95 use its 60-second window |

Video rates and averages use one-second windows, published with the existing
250 ms native polling cadence. No per-frame JSON or pixel data crosses IPC.
The bounded native timing ring runs during streaming even with the overlay hidden
so live toggles need no reconnect or configuration change.

macOS's `AVSampleBufferDisplayLayer` does not expose decoder completion. Decoded
FPS, decode time and render-queue time therefore show `—`, not zero. Missing host
timings and invalid timestamps also show `—`. A stalled measured rate becomes
zero after a full window; unavailable timings remain `—`.

Network-agent data must match the current monitor session, contain samples, and
follow an actual measurement report within three seconds. Agent heartbeats alone
do not refresh measurement age. Stream statistics older than two seconds are
replaced by a waiting message. Probe failures never prevent stream RTT and video
metrics from appearing.

## Persistence

Registered hosts store the choice in `preferencesOverride.window.showStatistics`.
Choices made before registration use `pendingStatistics`, keyed by the same host
ID, and move into the host override at registration. Toggle writes preserve all
other preferences. Latency-option resets preserve the overlay choice. Only the
matching active host is changed by a card toggle.

## Verification

- Native telemetry regression coverage: measurement windows, bitrate/FPS,
  sequence wrap/gaps, reset, stall, unavailable timestamps and opaque decoders.
- Rust coverage: C/Rust performance ABI size, persistence through registration
  and reload, instance isolation, preference preservation and probe freshness.
- Live platform checks still require a Sunshine stream: toggle repeatedly,
  resize/fullscreen, reconnect, change instance, and confirm mouse/keyboard input
  passes through the overlay. On Linux confirm the textoverlay anchor/font on
  X11 and Wayland, including sink fallback from glimagesink to xvimagesink or
  ximagesink; on Windows check DXGI and the GDI fallback.
