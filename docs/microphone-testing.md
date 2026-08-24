# Microphone transport validation

These tools validate the current Noland microphone path without modifying application code:

- `scripts/microphone-loopback.sh` — localhost synthetic sine through the real RTP/Opus sender and receiver
- `scripts/microphone-netem.sh` — guarded Linux `tc netem` scenarios for the microphone UDP ports
- `scripts/microphone-impulse-latency.py` — deterministic click generation and latency analysis
- `scripts/microphone-soak.py` — memory, FD, status, packet-counter, and queue-depth monitoring

The current media contract used by the tools is 48 kHz mono, 10 ms Opus frames, RTP payload type 111, maximum 1200-byte datagrams, and separate RTP/RTCP ports. Production defaults are RTP `48200`, receiver RTCP `48201`, and sender RTCP receive `48202`.

## Prerequisites

### Build the actual binaries

From the repository root:

```bash
cargo build --release --manifest-path mic-sidecar/Cargo.toml
cargo build --release --manifest-path vm-cloud-mic-agent/Cargo.toml
```

The scripts also accept installed binaries on `PATH` or explicit `--sender-bin` / `--receiver-bin` paths.

### Linux receiver runtime

The receiver currently requires Linux, PipeWire/WirePlumber, and the GStreamer runtime plugins used by provisioning:

```bash
sudo apt-get install --no-install-recommends \
  pipewire pipewire-pulse wireplumber pulseaudio-utils \
  gstreamer1.0-tools gstreamer1.0-pipewire \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  iproute2 python3
```

The provisioned PipeWire topology must expose:

- private receiver sink: `noland_mic_sink`
- application-facing source: `noland_mic_source`

Check it without changing the graph:

```bash
pw-cli ls Node | grep -E 'noland_mic_(sink|source)'
```

The receiver writes runtime status atomically to `/run/noland/noland_remote_microphone.status.json`. The user running it needs write access to `/run/noland`. Production provisioning creates that runtime directory. For a disposable development host, create it explicitly for the current user:

```bash
sudo install -d -m 0750 -o "$(id -un)" -g "$(id -gn)" /run/noland
```

Do not run a test receiver beside `noland-mic-receiver.service`. The soak harness refuses to launch a second process named `noland-mic-receiver`. When it owns the receiver, it backs up any pre-existing status file, stops both children on exit, and restores the prior file.

## 1. Local synthetic RTP/Opus loopback

This is the fastest transport/decode health check. It uses the sender's real synthetic sine source, GStreamer Opus encoder/payloader, UDP RTP/RTCP, receiver jitter buffer/decoder, and explicit PipeWire sink. It does **not** validate physical microphone capture.

```bash
scripts/microphone-loopback.sh --duration 30
```

Useful overrides:

```bash
scripts/microphone-loopback.sh \
  --sender-bin mic-sidecar/target/release/noland-mic-sender \
  --receiver-bin vm-cloud-mic-agent/target/release/noland-mic-receiver \
  --output-dir .tmp/microphone-loopback-manual \
  --duration 60
```

The command fails when process health is bad, packets do not grow, the receiver stops reporting audio, error counters increase, or configured queue limits are exceeded. It retains:

- `soak.csv` — every sample
- `summary.json` — endpoint growth, queue maxima/slopes, and alerts
- `sender.jsonl` and `sender.stderr.log`
- `receiver.log`

A pass demonstrates packets were sent, accepted, decoded, and handed to the configured PipeWire sink. It does not prove that another application selected `noland_mic_source` or that the audio was perceptually clean; inspect or record that source separately when those properties matter.

## 2. Linux impairment scenarios

`microphone-netem.sh` never calls `sudo`. Mutating commands require the script itself to have been explicitly invoked through `sudo`; `plan` and `status` are read-only.

Preview the exact qdisc/filter operations:

```bash
scripts/microphone-netem.sh plan wifi --interface lo
```

Prefer `run`, which installs the impairment, executes one command as the original non-root sudo user (so its PipeWire session remains usable), and removes the qdisc on normal exit, error, `INT`, or `TERM`:

```bash
sudo scripts/microphone-netem.sh run wifi --interface lo -- \
  scripts/microphone-loopback.sh --duration 60
```

For WireGuard sender-side egress:

```bash
sudo scripts/microphone-netem.sh run congested --interface wg0 -- \
  scripts/microphone-soak.py \
    --sender-bin mic-sidecar/target/release/noland-mic-sender \
    --receiver-pid RECEIVER_PID \
    --host 10.77.0.1 \
    --output-dir .tmp/microphone-congested \
    --duration 300
```

Available scenarios:

| Scenario | Netem configuration |
| --- | --- |
| `mild` | 10 ms ± 3 ms delay, normal distribution, 0.2% loss |
| `wifi` | 35 ms ± 12 ms delay, 1% loss, 0.2% reorder with 50% correlation |
| `congested` | 80 ms ± 25 ms delay, 3% loss, 256 kbit/s rate |
| `outage` | 100% packet loss |

Safety details:

- Only IPv4 UDP destination ports `48200,48201,48202` are filtered by default; override with `--ports` for an allocated session.
- A `prio` root qdisc is installed, with netem only on the filtered band. Other traffic is not intentionally impaired, although replacing a root `noqueue` qdisc still changes interface scheduling structure.
- The tool refuses to replace an existing non-`noqueue` root qdisc.
- Persistent `apply` writes ownership state under `/run/noland-mic-netem` and prints the matching `clear` command.
- `clear` verifies the expected handles before deleting anything.
- `run` requires `runuser` (from util-linux) and drops privileges for the test command; only qdisc setup/cleanup remains root.
- On a non-loopback interface, shaping is egress-only. Apply at both endpoints when both RTP and RTCP directions need impairment.

Inspect or clear a persistent scenario:

```bash
scripts/microphone-netem.sh status --interface wg0
sudo scripts/microphone-netem.sh clear --interface wg0
```

## 3. Impulse/latency measurement

Latency claims depend on the clock and reference method. The utility supports two modes and identifies the timing basis in JSON.

### Preferred: shared-clock reference channel

Generate an Opus-tolerant train of short, windowed clicks:

```bash
scripts/microphone-impulse-latency.py generate \
  --output .tmp/mic-latency/stimulus.wav
```

Route each click both to the sender's selected capture input and to a reference channel recorded on the **same audio clock** as the received `noland_mic_source`. A physical splitter/loopback or a verified PipeWire/ALSA graph can provide this. Record reference and received channels into one stereo, uncompressed PCM S16LE WAV. For example, if channel 0 is reference and channel 1 is received:

```bash
scripts/microphone-impulse-latency.py analyze \
  --stimulus .tmp/mic-latency/stimulus.wav \
  --capture .tmp/mic-latency/shared-clock.wav \
  --capture-channel 1 \
  --reference .tmp/mic-latency/shared-clock.wav \
  --reference-channel 0 \
  --json-output .tmp/mic-latency/result.json
```

The reported latency is the received event's sample position minus the reference event's sample position. Verify channel routing and inspect the WAV before treating the result as an acceptance measurement. The detector reports the individual clicks, median, p95, range, and population standard deviation; missed or spurious detections should invalidate the run.

### Convenient but approximate: process-timed PipeWire run

If a PipeWire playback target feeds the exact capture device selected by `noland-mic-sender`, an end-to-end estimate can be automated:

```bash
scripts/microphone-impulse-latency.py measure \
  --playback-target YOUR_SENDER_INPUT_TARGET \
  --capture-target noland_mic_source \
  --output-dir .tmp/mic-latency-estimate
```

Before using this mode, confirm the playback target actually appears as and feeds the sender's chosen input:

```bash
noland-mic-sender list-devices --json
```

A PulseAudio/PipeWire monitor or ALSA loopback may work on a given host, but CPAL backend/device exposure varies. The tool does not create or silently reconfigure that graph.

`measure` timestamps player launch relative to recorder launch. Therefore its absolute value includes process startup and PipeWire scheduling bias. It is useful for repeatable regression comparisons on an unchanged host, not as a hardware-calibrated one-way latency claim. Keep `latency-result.json`, `stimulus.wav`, `capture.wav`, and the two PipeWire command logs with the test record.

To detect clicks without claiming latency, omit both a reference and playback start:

```bash
scripts/microphone-impulse-latency.py analyze \
  --stimulus .tmp/mic-latency/stimulus.wav \
  --capture .tmp/mic-latency/capture.wav
```

## 4. Soak monitoring

### Managed synthetic soak

To launch both binaries with synthetic audio, use the loopback wrapper with a longer duration:

```bash
scripts/microphone-loopback.sh \
  --duration 21600 \
  --interval 10 \
  --output-dir .tmp/microphone-soak-6h
```

The sender runs in daemon mode, allowing the monitor to request `getStatus` and `getMetrics` on every interval. The CSV includes:

- sender/receiver RSS, FD count, and thread count from `/proc` on Linux
- sender state, health, session state, capture/pipeline errors
- sender `ringDepthSamples`, `appsrcQueueMs`, packet and byte counters
- receiver packet/loss/late/jitter/decode counters and health flags
- receiver `bufferDepthMs`, PipeWire errors, last-packet age, and status-file age

`summary.json` reports first/last/max values and an observed least-squares slope per hour. A positive short-run slope is not, by itself, proof of a leak; use a long steady-state window and inspect the time series.

### Attach to installed processes

Attaching can collect OS and receiver-status metrics without restarting production services:

```bash
scripts/microphone-soak.py \
  --sender-pid SENDER_PID \
  --receiver-pid RECEIVER_PID \
  --status-file /run/noland/noland_remote_microphone.status.json \
  --output-dir /var/tmp/noland-microphone-soak \
  --duration 21600 \
  --interval 10
```

An attached sender's IPC stream cannot be safely hijacked, so sender status and queue fields remain empty in attach mode. Use managed sender mode when those metrics are required. To monitor an intentionally idle receiver, pass `--allow-idle`; otherwise lack of packet growth is an alert.

Thresholds are configurable:

```text
--max-rss-growth-mib 32
--max-fd-growth 5
--max-sender-queue-ms 25
--max-ring-depth-samples 1920
--max-receiver-buffer-ms 60
```

Use `--fail-on-alert` in automation. These are test thresholds, not leak diagnoses: retain the CSV and correlate growth with workload phase, allocator behavior, logs, and packet/error counters.
