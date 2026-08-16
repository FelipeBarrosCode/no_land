#!/usr/bin/env python3
"""Generate and measure a deterministic impulse-like click train at 48 kHz mono."""

from __future__ import annotations

import argparse
import json
import math
import shutil
import signal
import statistics
import subprocess
import sys
import time
import wave
from array import array
from pathlib import Path
from typing import Any

RATE = 48_000
CHANNELS = 1
SAMPLE_WIDTH = 2


def log(message: str) -> None:
    print(f"[microphone-latency] {message}", file=sys.stderr, flush=True)


def generate_click_train(
    output: Path,
    count: int,
    lead_ms: float,
    spacing_ms: float,
    tail_ms: float,
    click_ms: float,
    frequency_hz: float,
    amplitude: float,
) -> dict[str, Any]:
    if count < 1:
        raise ValueError("count must be at least 1")
    if min(lead_ms, spacing_ms, tail_ms, click_ms) <= 0:
        raise ValueError("timings must be positive")
    if not 0 < amplitude <= 1:
        raise ValueError("amplitude must be in (0, 1]")
    total_ms = lead_ms + (count - 1) * spacing_ms + click_ms + tail_ms
    samples = array("h", [0]) * math.ceil(total_ms * RATE / 1000.0)
    click_samples = max(2, round(click_ms * RATE / 1000.0))
    centers_ms: list[float] = []
    for index in range(count):
        start_ms = lead_ms + index * spacing_ms
        start = round(start_ms * RATE / 1000.0)
        centers_ms.append((start + (click_samples - 1) / 2) * 1000.0 / RATE)
        for offset in range(click_samples):
            window = math.sin(math.pi * offset / (click_samples - 1)) ** 2
            carrier = math.sin(2 * math.pi * frequency_hz * offset / RATE)
            value = int(32767 * amplitude * window * carrier)
            position = start + offset
            if position < len(samples):
                samples[position] = max(-32768, min(32767, value))

    output.parent.mkdir(parents=True, exist_ok=True)
    if sys.byteorder != "little":
        samples.byteswap()
    with wave.open(str(output), "wb") as wav:
        wav.setnchannels(CHANNELS)
        wav.setsampwidth(SAMPLE_WIDTH)
        wav.setframerate(RATE)
        wav.writeframes(samples.tobytes())

    metadata = {
        "format": "PCM_S16LE",
        "sampleRate": RATE,
        "channels": CHANNELS,
        "count": count,
        "leadMs": lead_ms,
        "spacingMs": spacing_ms,
        "tailMs": tail_ms,
        "clickMs": click_ms,
        "frequencyHz": frequency_hz,
        "amplitude": amplitude,
        "clickCentersMs": centers_ms,
        "durationMs": len(samples) * 1000.0 / RATE,
    }
    metadata_path = Path(str(output) + ".json")
    metadata_path.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
    return metadata


def read_pcm16(path: Path, channel: int | None = None) -> tuple[array[int], int]:
    with wave.open(str(path), "rb") as wav:
        if wav.getsampwidth() != SAMPLE_WIDTH or wav.getcomptype() != "NONE":
            raise ValueError(f"{path} must be uncompressed 16-bit PCM WAV")
        rate = wav.getframerate()
        channels = wav.getnchannels()
        raw = array("h")
        raw.frombytes(wav.readframes(wav.getnframes()))
    if sys.byteorder != "little":
        raw.byteswap()
    if channels < 1:
        raise ValueError(f"{path} has no channels")
    if channel is not None and not 0 <= channel < channels:
        raise ValueError(f"channel {channel} is outside {path}'s {channels} channels")
    if channels > 1:
        mono = array("h")
        for offset in range(0, len(raw), channels):
            frame = raw[offset : offset + channels]
            if channel is None:
                mono.append(round(sum(frame) / len(frame)))
            else:
                mono.append(frame[channel])
        raw = mono
    elif channel not in (None, 0):
        raise ValueError(f"channel {channel} is outside mono file {path}")
    return raw, rate


def window_rms(samples: array[int], window_samples: int) -> list[float]:
    if not samples:
        return []
    squares = [int(value) * int(value) for value in samples]
    result: list[float] = []
    running = 0
    for index, value in enumerate(squares):
        running += value
        if index >= window_samples:
            running -= squares[index - window_samples]
        if index + 1 >= window_samples and (index + 1 - window_samples) % window_samples == 0:
            result.append(math.sqrt(running / window_samples))
    return result


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot calculate a percentile of no values")
    position = (len(ordered) - 1) * fraction
    low = math.floor(position)
    high = math.ceil(position)
    if low == high:
        return ordered[low]
    return ordered[low] * (high - position) + ordered[high] * (position - low)


def detect_events(
    envelope: list[float], threshold: float, count: int, spacing_ms: float, window_ms: float
) -> list[int]:
    minimum_gap_windows = max(1, round(spacing_ms * 0.5 / window_ms))
    candidates = [index for index, value in enumerate(envelope) if value >= threshold]
    selected: list[int] = []
    for candidate in sorted(candidates, key=envelope.__getitem__, reverse=True):
        if all(abs(candidate - prior) >= minimum_gap_windows for prior in selected):
            selected.append(candidate)
        if len(selected) >= count:
            break
    return sorted(selected)


def analyze_capture(
    capture: Path,
    metadata: dict[str, Any],
    playback_start_ms: float | None,
    min_latency_ms: float,
    max_latency_ms: float,
    window_ms: float,
    threshold_ratio: float,
    capture_channel: int | None = None,
    reference: Path | None = None,
    reference_channel: int | None = None,
) -> dict[str, Any]:
    samples, rate = read_pcm16(capture, capture_channel)
    window_samples = max(1, round(window_ms * rate / 1000.0))
    envelope = window_rms(samples, window_samples)
    if not envelope:
        raise ValueError("capture is too short")
    baseline = statistics.median(envelope)
    peak = max(envelope)
    threshold = baseline + (peak - baseline) * threshold_ratio
    if peak <= max(100.0, baseline * 1.5):
        raise ValueError(
            f"no usable click energy found (baseline RMS {baseline:.1f}, peak RMS {peak:.1f})"
        )

    click_centers = [float(value) for value in metadata["clickCentersMs"]]
    detections: list[dict[str, float | int]] = []
    latencies: list[float] = []
    timing_basis = "capture timeline only; no latency calculated without playback-start-ms or reference"
    if reference is not None:
        reference_samples, reference_rate = read_pcm16(reference, reference_channel)
        if reference_rate != rate:
            raise ValueError("capture and reference sample rates must match")
        reference_envelope = window_rms(reference_samples, window_samples)
        reference_baseline = statistics.median(reference_envelope)
        reference_peak = max(reference_envelope)
        reference_threshold = reference_baseline + (reference_peak - reference_baseline) * threshold_ratio
        reference_events = detect_events(
            reference_envelope,
            reference_threshold,
            int(metadata["count"]),
            float(metadata["spacingMs"]),
            window_ms,
        )
        capture_events = detect_events(
            envelope,
            threshold,
            int(metadata["count"]),
            float(metadata["spacingMs"]),
            window_ms,
        )
        for index, (reference_window, capture_window) in enumerate(zip(reference_events, capture_events)):
            reference_ms = (reference_window + 0.5) * window_ms
            detected_ms = (capture_window + 0.5) * window_ms
            latency_ms = detected_ms - reference_ms
            detections.append(
                {
                    "click": index + 1,
                    "referenceMs": reference_ms,
                    "detectedCaptureMs": detected_ms,
                    "latencyMs": latency_ms,
                    "rms": envelope[capture_window],
                }
            )
            latencies.append(latency_ms)
        timing_basis = "shared sample timeline: capture event minus reference event"
    elif playback_start_ms is not None:
        for index, click_ms in enumerate(click_centers):
            expected_ms = playback_start_ms + click_ms
            first_window = max(0, math.floor((expected_ms + min_latency_ms) / window_ms))
            last_window = min(
                len(envelope) - 1,
                math.ceil((expected_ms + max_latency_ms) / window_ms),
            )
            if first_window > last_window:
                continue
            candidate = max(range(first_window, last_window + 1), key=envelope.__getitem__)
            candidate_rms = envelope[candidate]
            if candidate_rms < threshold:
                continue
            detected_ms = (candidate + 0.5) * window_ms
            latency_ms = detected_ms - expected_ms
            detections.append(
                {
                    "click": index + 1,
                    "expectedCaptureMs": expected_ms,
                    "detectedCaptureMs": detected_ms,
                    "latencyMs": latency_ms,
                    "rms": candidate_rms,
                }
            )
            latencies.append(latency_ms)
        timing_basis = "process launch relative to recorder process launch; includes tool/process scheduling bias"
    else:
        selected = detect_events(
            envelope,
            threshold,
            int(metadata["count"]),
            float(metadata["spacingMs"]),
            window_ms,
        )
        for index, candidate in enumerate(selected):
            detections.append(
                {
                    "click": index + 1,
                    "detectedCaptureMs": (candidate + 0.5) * window_ms,
                    "rms": envelope[candidate],
                }
            )

    result: dict[str, Any] = {
        "capture": str(capture),
        "captureRate": rate,
        "captureDurationMs": len(samples) * 1000.0 / rate,
        "windowMs": window_ms,
        "baselineRms": baseline,
        "peakRms": peak,
        "thresholdRms": threshold,
        "detections": detections,
        "playbackStartMs": playback_start_ms,
        "captureChannel": capture_channel,
        "reference": str(reference) if reference else None,
        "referenceChannel": reference_channel,
        "timingBasis": timing_basis,
    }
    if latencies:
        result["latencyMs"] = {
            "count": len(latencies),
            "median": statistics.median(latencies),
            "p95": percentile(latencies, 0.95),
            "min": min(latencies),
            "max": max(latencies),
            "populationStdDev": statistics.pstdev(latencies),
        }
    return result


def load_metadata(stimulus: Path, metadata_path: Path | None) -> dict[str, Any]:
    path = metadata_path or Path(str(stimulus) + ".json")
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read stimulus metadata {path}: {error}") from error
    if not isinstance(value, dict) or "clickCentersMs" not in value:
        raise ValueError(f"invalid stimulus metadata: {path}")
    return value


def stop_recorder(process: subprocess.Popen[Any]) -> None:
    if process.poll() is not None:
        return
    process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def measure(args: argparse.Namespace) -> dict[str, Any]:
    for command in ("pw-record", "pw-play"):
        if not shutil.which(command):
            raise ValueError(f"{command} is required for measure mode")
    output_dir = Path(args.output_dir).expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    stimulus = output_dir / "stimulus.wav"
    capture = output_dir / "capture.wav"
    metadata = generate_click_train(
        stimulus,
        args.count,
        args.lead_ms,
        args.spacing_ms,
        args.tail_ms,
        args.click_ms,
        args.frequency_hz,
        args.amplitude,
    )
    record_log = (output_dir / "pw-record.log").open("w", encoding="utf-8")
    play_log = (output_dir / "pw-play.log").open("w", encoding="utf-8")
    recorder_started = time.monotonic()
    recorder = subprocess.Popen(
        [
            "pw-record",
            "--target",
            args.capture_target,
            "--rate",
            str(RATE),
            "--channels",
            str(CHANNELS),
            str(capture),
        ],
        stdout=record_log,
        stderr=subprocess.STDOUT,
    )
    try:
        time.sleep(args.record_warmup_ms / 1000.0)
        if recorder.poll() is not None:
            raise RuntimeError(f"pw-record exited with code {recorder.returncode}")
        playback_start_ms = (time.monotonic() - recorder_started) * 1000.0
        player = subprocess.run(
            ["pw-play", "--target", args.playback_target, str(stimulus)],
            stdout=play_log,
            stderr=subprocess.STDOUT,
            check=False,
        )
        if player.returncode != 0:
            raise RuntimeError(f"pw-play exited with code {player.returncode}")
        time.sleep(args.record_tail_ms / 1000.0)
    finally:
        stop_recorder(recorder)
        record_log.close()
        play_log.close()
    result = analyze_capture(
        capture,
        metadata,
        playback_start_ms,
        args.min_latency_ms,
        args.max_latency_ms,
        args.window_ms,
        args.threshold_ratio,
        capture_channel=None,
        reference=None,
        reference_channel=None,
    )
    result["measurement"] = {
        "captureTarget": args.capture_target,
        "playbackTarget": args.playback_target,
        "recordWarmupMs": args.record_warmup_ms,
        "warning": (
            "This is a process-timed estimate, not a hardware-calibrated one-way measurement. "
            "Recorder/player startup and PipeWire scheduling bias are included. Use repeated runs "
            "and a shared-clock hardware/reference-channel method for acceptance limits."
        ),
    }
    (output_dir / "latency-result.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n"
    )
    return result


def add_generation_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--count", type=int, default=8)
    parser.add_argument("--lead-ms", type=float, default=500.0)
    parser.add_argument("--spacing-ms", type=float, default=1000.0)
    parser.add_argument("--tail-ms", type=float, default=500.0)
    parser.add_argument("--click-ms", type=float, default=8.0)
    parser.add_argument("--frequency-hz", type=float, default=2500.0)
    parser.add_argument("--amplitude", type=float, default=0.9)


def add_analysis_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--min-latency-ms", type=float, default=0.0)
    parser.add_argument("--max-latency-ms", type=float, default=500.0)
    parser.add_argument("--window-ms", type=float, default=5.0)
    parser.add_argument("--threshold-ratio", type=float, default=0.30)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Generate Opus-tolerant impulse-like clicks, analyze a received WAV, or run an "
            "approximate PipeWire process-timed measurement."
        )
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    generate_parser = subparsers.add_parser("generate", help="write a deterministic click-train WAV and metadata")
    generate_parser.add_argument("--output", required=True)
    add_generation_options(generate_parser)

    analyze_parser = subparsers.add_parser("analyze", help="detect clicks in a captured WAV")
    analyze_parser.add_argument("--capture", required=True)
    analyze_parser.add_argument("--stimulus", required=True)
    analyze_parser.add_argument("--metadata")
    analyze_parser.add_argument("--capture-channel", type=int, help="zero-based channel; default downmixes")
    analyze_parser.add_argument("--reference", help="shared-clock reference WAV; may be the same multichannel file")
    analyze_parser.add_argument("--reference-channel", type=int, help="zero-based reference channel")
    analyze_parser.add_argument(
        "--playback-start-ms",
        type=float,
        help="playback process start relative to capture timeline; omit to report detections only",
    )
    analyze_parser.add_argument("--json-output")
    add_analysis_options(analyze_parser)

    measure_parser = subparsers.add_parser(
        "measure", help="run pw-record/pw-play and report an explicitly approximate latency estimate"
    )
    measure_parser.add_argument("--playback-target", required=True)
    measure_parser.add_argument("--capture-target", default="noland_mic_source")
    measure_parser.add_argument("--output-dir", required=True)
    measure_parser.add_argument("--record-warmup-ms", type=float, default=1500.0)
    measure_parser.add_argument("--record-tail-ms", type=float, default=1000.0)
    add_generation_options(measure_parser)
    add_analysis_options(measure_parser)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "generate":
            output = Path(args.output).expanduser().resolve()
            result = generate_click_train(
                output,
                args.count,
                args.lead_ms,
                args.spacing_ms,
                args.tail_ms,
                args.click_ms,
                args.frequency_hz,
                args.amplitude,
            )
            print(json.dumps({"output": str(output), "metadata": result}, indent=2, sort_keys=True))
            return 0
        if args.command == "analyze":
            stimulus = Path(args.stimulus).expanduser().resolve()
            metadata = load_metadata(
                stimulus, Path(args.metadata).expanduser().resolve() if args.metadata else None
            )
            result = analyze_capture(
                Path(args.capture).expanduser().resolve(),
                metadata,
                args.playback_start_ms,
                args.min_latency_ms,
                args.max_latency_ms,
                args.window_ms,
                args.threshold_ratio,
                capture_channel=args.capture_channel,
                reference=Path(args.reference).expanduser().resolve() if args.reference else None,
                reference_channel=args.reference_channel,
            )
            if args.json_output:
                Path(args.json_output).write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
            print(json.dumps(result, indent=2, sort_keys=True))
            if args.playback_start_ms is None and not args.reference:
                log("no --playback-start-ms or --reference supplied; detections are not reported as latency")
            return 0
        result = measure(args)
        print(json.dumps(result, indent=2, sort_keys=True))
        log("reported latency is a process-timed estimate; read measurement.warning in the JSON")
        return 0
    except (ValueError, RuntimeError, OSError, wave.Error) as error:
        log(f"error: {error}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
