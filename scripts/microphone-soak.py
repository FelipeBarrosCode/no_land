#!/usr/bin/env python3
"""Monitor Noland microphone processes and optionally run a synthetic RTP/Opus soak."""

from __future__ import annotations

import argparse
import atexit
import csv
import json
import os
import queue
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

DEFAULT_STATUS = "/run/noland/noland_remote_microphone.status.json"
STOP = threading.Event()


def log(message: str) -> None:
    print(f"[microphone-soak] {message}", file=sys.stderr, flush=True)


def on_signal(signum: int, _frame: Any) -> None:
    log(f"received signal {signum}; stopping")
    STOP.set()


@dataclass
class ProcessStats:
    rss_bytes: int | None = None
    fd_count: int | None = None
    threads: int | None = None


def process_stats(pid: int | None) -> ProcessStats:
    if not pid:
        return ProcessStats()
    proc = Path("/proc") / str(pid)
    if proc.exists():
        rss_bytes = None
        threads = None
        try:
            for line in (proc / "status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    rss_bytes = int(line.split()[1]) * 1024
                elif line.startswith("Threads:"):
                    threads = int(line.split()[1])
        except (OSError, ValueError, IndexError):
            pass
        try:
            fd_count = sum(1 for _ in (proc / "fd").iterdir())
        except OSError:
            fd_count = None
        return ProcessStats(rss_bytes, fd_count, threads)

    try:
        rss_kib = int(
            subprocess.check_output(
                ["ps", "-o", "rss=", "-p", str(pid)], text=True
            ).strip()
        )
    except (OSError, subprocess.CalledProcessError, ValueError):
        rss_kib = 0
    return ProcessStats(rss_bytes=rss_kib * 1024 if rss_kib else None)


def process_alive(pid: int | None) -> bool:
    if not pid:
        return False
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except ProcessLookupError:
        return False
    except OSError:
        return False


def named_processes(name: str) -> list[int]:
    matches: list[int] = []
    proc = Path("/proc")
    if proc.exists():
        for entry in proc.iterdir():
            if not entry.name.isdigit():
                continue
            try:
                if (entry / "comm").read_text().strip() == name:
                    matches.append(int(entry.name))
            except OSError:
                continue
        return matches
    try:
        output = subprocess.check_output(["pgrep", "-x", name], text=True)
        return [int(value) for value in output.split()]
    except (OSError, subprocess.CalledProcessError, ValueError):
        return []


def read_json(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text())
        return value if isinstance(value, dict) else None
    except (OSError, json.JSONDecodeError):
        return None


def nested(value: dict[str, Any] | None, *keys: str) -> Any:
    current: Any = value
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current


class ManagedReceiver:
    def __init__(self, binary: str, config: str, log_path: Path) -> None:
        self.binary = binary
        self.config = config
        self.log_path = log_path
        self.process: subprocess.Popen[str] | None = None
        self.log_file: Any = None

    def start(self) -> None:
        self.log_file = self.log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            [self.binary, "--config", self.config],
            stdin=subprocess.DEVNULL,
            stdout=self.log_file,
            stderr=self.log_file,
            text=True,
            start_new_session=True,
        )

    @property
    def pid(self) -> int | None:
        return self.process.pid if self.process else None

    def stop(self) -> None:
        terminate_process(self.process, "receiver")
        if self.log_file:
            self.log_file.close()


class ManagedSender:
    def __init__(self, binary: str, stdout_path: Path, stderr_path: Path) -> None:
        self.binary = binary
        self.stdout_path = stdout_path
        self.stderr_path = stderr_path
        self.process: subprocess.Popen[str] | None = None
        self.stdout_file: Any = None
        self.stderr_file: Any = None
        self.responses: dict[str, queue.Queue[dict[str, Any]]] = {}
        self.lock = threading.Lock()
        self.next_id = 1
        self.reader: threading.Thread | None = None

    def start(self) -> None:
        self.stdout_file = self.stdout_path.open("w", encoding="utf-8")
        self.stderr_file = self.stderr_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            [self.binary, "daemon"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr_file,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
        self.reader = threading.Thread(target=self._read_stdout, daemon=True)
        self.reader.start()

    @property
    def pid(self) -> int | None:
        return self.process.pid if self.process else None

    def _read_stdout(self) -> None:
        assert self.process and self.process.stdout and self.stdout_file
        for line in self.process.stdout:
            self.stdout_file.write(line)
            self.stdout_file.flush()
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            response_id = value.get("id") if isinstance(value, dict) else None
            if response_id is None:
                continue
            with self.lock:
                destination = self.responses.get(str(response_id))
            if destination:
                destination.put(value)

    def request(self, command: dict[str, Any], timeout: float = 5.0) -> dict[str, Any]:
        if not self.process or self.process.poll() is not None or not self.process.stdin:
            raise RuntimeError("sender is not running")
        with self.lock:
            request_id = str(self.next_id)
            self.next_id += 1
            destination: queue.Queue[dict[str, Any]] = queue.Queue(maxsize=1)
            self.responses[request_id] = destination
        payload = {"id": request_id, **command}
        try:
            self.process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
            self.process.stdin.flush()
            response = destination.get(timeout=timeout)
        except (BrokenPipeError, queue.Empty) as error:
            raise RuntimeError(f"sender request {command.get('command')} failed: {error}") from error
        finally:
            with self.lock:
                self.responses.pop(request_id, None)
        if not response.get("ok"):
            raise RuntimeError(
                f"sender rejected {command.get('command')}: {response.get('error', 'unknown error')}"
            )
        result = response.get("result")
        return result if isinstance(result, dict) else {}

    def stop(self) -> None:
        if self.process and self.process.poll() is None:
            try:
                self.request({"command": "shutdown"}, timeout=2.0)
            except RuntimeError:
                pass
        terminate_process(self.process, "sender")
        if self.stdout_file:
            self.stdout_file.close()
        if self.stderr_file:
            self.stderr_file.close()


def terminate_process(process: subprocess.Popen[str] | None, label: str) -> None:
    if not process or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        log(f"{label} did not exit after SIGTERM; sending SIGKILL")
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass


def slope_per_hour(samples: list[tuple[float, float | int | None]]) -> float | None:
    values = [(x, float(y)) for x, y in samples if y is not None]
    if len(values) < 2 or values[-1][0] == values[0][0]:
        return None
    x_mean = statistics.fmean(x for x, _ in values)
    y_mean = statistics.fmean(y for _, y in values)
    denominator = sum((x - x_mean) ** 2 for x, _ in values)
    if denominator == 0:
        return None
    per_second = sum((x - x_mean) * (y - y_mean) for x, y in values) / denominator
    return per_second * 3600.0


def add_alert(alerts: set[str], message: str) -> None:
    if message not in alerts:
        alerts.add(message)
        log(f"ALERT: {message}")


def wait_for_receiver(
    process: subprocess.Popen[str], status_path: Path, session_id: str, timeout: float
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and not STOP.is_set():
        if process.poll() is not None:
            raise RuntimeError(f"receiver exited during startup with code {process.returncode}")
        status = read_json(status_path)
        if status and status.get("sessionId") == session_id:
            return status
        time.sleep(0.1)
    raise RuntimeError(f"receiver status did not become ready at {status_path} within {timeout}s")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description=(
            "Monitor memory, file descriptors, status, packet counters, and queue depth. "
            "It can attach to existing PIDs or launch the two Noland binaries with a synthetic sine source."
        )
    )
    sender = result.add_mutually_exclusive_group()
    sender.add_argument("--sender-bin", help="launch this noland-mic-sender in daemon mode")
    sender.add_argument("--sender-pid", type=int, help="attach OS monitoring to an existing sender")
    receiver = result.add_mutually_exclusive_group()
    receiver.add_argument("--receiver-bin", help="launch this noland-mic-receiver")
    receiver.add_argument("--receiver-pid", type=int, help="attach to an existing receiver")
    result.add_argument("--receiver-config", help="config required with --receiver-bin")
    result.add_argument("--status-file", default=DEFAULT_STATUS)
    result.add_argument("--output-dir", required=True)
    result.add_argument("--duration", type=float, default=3600.0, help="seconds; 0 runs until interrupted")
    result.add_argument("--interval", type=float, default=5.0)
    result.add_argument("--startup-timeout", type=float, default=15.0)
    result.add_argument("--startup-grace", type=float, default=5.0)
    result.add_argument("--host", default="127.0.0.1")
    result.add_argument("--rtp-port", type=int, default=48200)
    result.add_argument("--rtcp-port", type=int, default=48201)
    result.add_argument("--rtcp-listen-port", type=int, default=48202)
    result.add_argument("--bitrate-kbps", type=int, default=32)
    result.add_argument("--packet-loss-percent", type=int, default=5)
    result.add_argument("--fec", action=argparse.BooleanOptionalAction, default=True)
    result.add_argument("--ssrc", type=int)
    result.add_argument("--session-id", default=f"local-soak-{os.getpid()}")
    result.add_argument("--allow-idle", action="store_true", help="do not alert when receiver packet growth stops")
    result.add_argument("--max-rss-growth-mib", type=float, default=32.0)
    result.add_argument("--max-fd-growth", type=int, default=5)
    result.add_argument("--max-sender-queue-ms", type=float, default=25.0)
    result.add_argument("--max-ring-depth-samples", type=int, default=1920)
    result.add_argument("--max-receiver-buffer-ms", type=float, default=60.0)
    result.add_argument("--fail-on-alert", action="store_true")
    return result


def validate_args(args: argparse.Namespace) -> None:
    if not any((args.sender_bin, args.sender_pid, args.receiver_bin, args.receiver_pid)):
        raise ValueError("specify at least one sender or receiver PID/binary")
    if args.receiver_bin and not args.receiver_config:
        raise ValueError("--receiver-config is required with --receiver-bin")
    if args.duration < 0 or args.interval <= 0:
        raise ValueError("--duration must be >= 0 and --interval must be > 0")
    for name in ("rtp_port", "rtcp_port", "rtcp_listen_port"):
        value = getattr(args, name)
        if not 1 <= value <= 65535:
            raise ValueError(f"--{name.replace('_', '-')} must be between 1 and 65535")
    if len({args.rtp_port, args.rtcp_port, args.rtcp_listen_port}) != 3:
        raise ValueError("RTP, RTCP, and RTCP-listen ports must differ")
    if not 6 <= args.bitrate_kbps <= 128:
        raise ValueError("--bitrate-kbps must be between 6 and 128")
    if not 0 <= args.packet_loss_percent <= 100:
        raise ValueError("--packet-loss-percent must be between 0 and 100")


def run(args: argparse.Namespace) -> int:
    validate_args(args)
    output_dir = Path(args.output_dir).expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    status_path = Path(args.status_file)
    managed_receiver: ManagedReceiver | None = None
    managed_sender: ManagedSender | None = None
    receiver_pid = args.receiver_pid
    sender_pid = args.sender_pid
    status_backup: Path | None = None
    status_existed = False
    alerts: set[str] = set()
    samples: list[dict[str, Any]] = []

    if args.receiver_bin:
        existing = named_processes("noland-mic-receiver")
        if existing:
            raise RuntimeError(
                "refusing to launch a second receiver while PIDs "
                + ", ".join(map(str, existing))
                + " are running; stop the service or use --receiver-pid"
            )
        try:
            status_path.parent.mkdir(parents=True, exist_ok=True)
        except PermissionError as error:
            raise RuntimeError(
                f"cannot create {status_path.parent}; provision /run/noland for the receiver user first"
            ) from error
        if not os.access(status_path.parent, os.W_OK):
            raise RuntimeError(f"status directory is not writable: {status_path.parent}")
        if status_path.exists():
            status_existed = True
            status_backup = output_dir / "receiver-status.preexisting.json"
            shutil.copy2(status_path, status_backup)
            status_path.unlink()
            log(f"backed up pre-existing status to {status_backup}")

        def restore_status() -> None:
            try:
                status_path.unlink(missing_ok=True)
                if status_existed and status_backup:
                    shutil.copy2(status_backup, status_path)
                    log(f"restored pre-existing status at {status_path}")
            except OSError as error:
                log(f"warning: failed restoring status file: {error}")

        atexit.register(restore_status)
        managed_receiver = ManagedReceiver(
            args.receiver_bin, args.receiver_config, output_dir / "receiver.log"
        )
        managed_receiver.start()
        atexit.register(managed_receiver.stop)
        receiver_pid = managed_receiver.pid
        if managed_receiver.process is None:
            raise RuntimeError("receiver process did not start")
        wait_for_receiver(
            managed_receiver.process, status_path, args.session_id, args.startup_timeout
        )
        log(f"launched receiver PID {receiver_pid}")

    if args.sender_bin:
        managed_sender = ManagedSender(
            args.sender_bin,
            output_dir / "sender.jsonl",
            output_dir / "sender.stderr.log",
        )
        managed_sender.start()
        atexit.register(managed_sender.stop)
        sender_pid = managed_sender.pid
        config: dict[str, Any] = {
            "sessionId": args.session_id,
            "host": args.host,
            "rtpPort": args.rtp_port,
            "rtcpPort": args.rtcp_port,
            "rtcpListenPort": args.rtcp_listen_port,
            "bitrate": args.bitrate_kbps * 1000,
            "frameMs": 10,
            "fec": args.fec,
            "packetLossPercent": args.packet_loss_percent,
            "dtx": False,
            "source": "sine",
        }
        if args.ssrc is not None:
            config["ssrc"] = args.ssrc
        managed_sender.request({"command": "startSession", "config": config}, args.startup_timeout)
        log(f"launched synthetic sender PID {sender_pid}")

    fields = [
        "elapsed_s",
        "unix_ms",
        "sender_pid",
        "sender_alive",
        "sender_rss_bytes",
        "sender_fd_count",
        "sender_threads",
        "sender_state",
        "sender_health",
        "sender_session_active",
        "sender_last_error",
        "sender_ring_depth_samples",
        "sender_appsrc_queue_ms",
        "sender_opus_packets_sent",
        "sender_bytes_sent",
        "sender_capture_errors",
        "sender_pipeline_errors",
        "sender_overruns",
        "sender_underruns",
        "receiver_pid",
        "receiver_alive",
        "receiver_rss_bytes",
        "receiver_fd_count",
        "receiver_threads",
        "receiver_receiving_audio",
        "receiver_received_packets",
        "receiver_lost_packets",
        "receiver_late_packets",
        "receiver_packet_loss_percent",
        "receiver_jitter_ms",
        "receiver_buffer_depth_ms",
        "receiver_decoded_buffers",
        "receiver_pipewire_errors",
        "receiver_healthy",
        "receiver_last_packet_ms_ago",
        "receiver_status_age_ms",
    ]

    csv_path = output_dir / "soak.csv"
    start = time.monotonic()
    next_sample = start
    with csv_path.open("w", newline="", encoding="utf-8") as csv_file:
        writer = csv.DictWriter(csv_file, fieldnames=fields)
        writer.writeheader()
        while not STOP.is_set() and (args.duration == 0 or time.monotonic() - start < args.duration):
            now = time.monotonic()
            if now < next_sample:
                STOP.wait(next_sample - now)
                continue
            elapsed = now - start
            next_sample += args.interval

            sender_status: dict[str, Any] | None = None
            sender_metrics: dict[str, Any] | None = None
            if managed_sender:
                try:
                    sender_status = managed_sender.request({"command": "getStatus"})
                    sender_metrics = managed_sender.request({"command": "getMetrics"})
                except RuntimeError as error:
                    add_alert(alerts, str(error))

            receiver_status = read_json(status_path)
            sender_os = process_stats(sender_pid)
            receiver_os = process_stats(receiver_pid)
            sender_is_alive = process_alive(sender_pid)
            receiver_is_alive = process_alive(receiver_pid)
            if sender_pid and not sender_is_alive:
                add_alert(alerts, f"sender PID {sender_pid} exited")
            if receiver_pid and not receiver_is_alive:
                add_alert(alerts, f"receiver PID {receiver_pid} exited")
            if managed_sender and elapsed >= args.startup_grace:
                if sender_status and sender_status.get("health") != "healthy":
                    add_alert(alerts, f"sender health is {sender_status.get('health')}")
                if sender_status and not sender_status.get("sessionActive"):
                    add_alert(alerts, "sender session is not active")
            if receiver_status and elapsed >= args.startup_grace:
                if nested(receiver_status, "health", "healthy") is False:
                    add_alert(alerts, "receiver health.healthy is false")
                if not args.allow_idle and receiver_status.get("receivingAudio") is False:
                    add_alert(alerts, "receiver is not receiving audio")
            if args.receiver_bin and receiver_status is None:
                add_alert(alerts, f"receiver status is unreadable at {status_path}")

            status_age_ms = None
            try:
                status_age_ms = max(0.0, (time.time() - status_path.stat().st_mtime) * 1000.0)
            except OSError:
                pass

            row = {
                "elapsed_s": round(elapsed, 3),
                "unix_ms": int(time.time() * 1000),
                "sender_pid": sender_pid,
                "sender_alive": sender_is_alive if sender_pid else None,
                "sender_rss_bytes": sender_os.rss_bytes,
                "sender_fd_count": sender_os.fd_count,
                "sender_threads": sender_os.threads,
                "sender_state": sender_status.get("state") if sender_status else None,
                "sender_health": sender_status.get("health") if sender_status else None,
                "sender_session_active": sender_status.get("sessionActive") if sender_status else None,
                "sender_last_error": sender_status.get("lastError") if sender_status else None,
                "sender_ring_depth_samples": sender_metrics.get("ringDepthSamples") if sender_metrics else None,
                "sender_appsrc_queue_ms": sender_metrics.get("appsrcQueueMs") if sender_metrics else None,
                "sender_opus_packets_sent": sender_metrics.get("opusPacketsSent") if sender_metrics else None,
                "sender_bytes_sent": sender_metrics.get("bytesSent") if sender_metrics else None,
                "sender_capture_errors": sender_metrics.get("captureErrors") if sender_metrics else None,
                "sender_pipeline_errors": sender_metrics.get("pipelineErrors") if sender_metrics else None,
                "sender_overruns": sender_metrics.get("overruns") if sender_metrics else None,
                "sender_underruns": sender_metrics.get("underruns") if sender_metrics else None,
                "receiver_pid": receiver_pid,
                "receiver_alive": receiver_is_alive if receiver_pid else None,
                "receiver_rss_bytes": receiver_os.rss_bytes,
                "receiver_fd_count": receiver_os.fd_count,
                "receiver_threads": receiver_os.threads,
                "receiver_receiving_audio": receiver_status.get("receivingAudio") if receiver_status else None,
                "receiver_received_packets": receiver_status.get("receivedPackets") if receiver_status else None,
                "receiver_lost_packets": receiver_status.get("lostPackets") if receiver_status else None,
                "receiver_late_packets": receiver_status.get("latePackets") if receiver_status else None,
                "receiver_packet_loss_percent": receiver_status.get("packetLossPercent") if receiver_status else None,
                "receiver_jitter_ms": receiver_status.get("jitterMs") if receiver_status else None,
                "receiver_buffer_depth_ms": receiver_status.get("bufferDepthMs") if receiver_status else None,
                "receiver_decoded_buffers": receiver_status.get("decodedBuffers") if receiver_status else None,
                "receiver_pipewire_errors": receiver_status.get("pipewireErrors") if receiver_status else None,
                "receiver_healthy": nested(receiver_status, "health", "healthy"),
                "receiver_last_packet_ms_ago": receiver_status.get("lastPacketMsAgo") if receiver_status else None,
                "receiver_status_age_ms": round(status_age_ms, 3) if status_age_ms is not None else None,
            }
            writer.writerow(row)
            csv_file.flush()
            samples.append(row)
            log(
                "sample "
                f"t={elapsed:.1f}s sender_rss={row['sender_rss_bytes']} sender_fd={row['sender_fd_count']} "
                f"appsrc_ms={row['sender_appsrc_queue_ms']} ring={row['sender_ring_depth_samples']} "
                f"receiver_rss={row['receiver_rss_bytes']} receiver_fd={row['receiver_fd_count']} "
                f"packets={row['receiver_received_packets']} buffer_ms={row['receiver_buffer_depth_ms']}"
            )

    summary: dict[str, Any] = {
        "samples": len(samples),
        "durationSeconds": round(time.monotonic() - start, 3),
        "csv": str(csv_path),
        "alerts": sorted(alerts),
    }
    if samples:
        elapsed_values = [float(row["elapsed_s"]) for row in samples]
        for prefix in ("sender", "receiver"):
            rss = [(elapsed_values[i], row[f"{prefix}_rss_bytes"]) for i, row in enumerate(samples)]
            fds = [(elapsed_values[i], row[f"{prefix}_fd_count"]) for i, row in enumerate(samples)]
            valid_rss = [value for _, value in rss if value is not None]
            valid_fds = [value for _, value in fds if value is not None]
            summary[prefix] = {
                "rssFirstBytes": valid_rss[0] if valid_rss else None,
                "rssLastBytes": valid_rss[-1] if valid_rss else None,
                "rssMaxBytes": max(valid_rss) if valid_rss else None,
                "rssObservedSlopeBytesPerHour": slope_per_hour(rss),
                "fdFirst": valid_fds[0] if valid_fds else None,
                "fdLast": valid_fds[-1] if valid_fds else None,
                "fdMax": max(valid_fds) if valid_fds else None,
                "fdObservedSlopePerHour": slope_per_hour(fds),
            }
            if len(valid_rss) >= 2 and valid_rss[-1] - valid_rss[0] > args.max_rss_growth_mib * 1024 * 1024:
                add_alert(
                    alerts,
                    f"{prefix} endpoint RSS growth exceeded {args.max_rss_growth_mib:g} MiB",
                )
            if len(valid_fds) >= 2 and valid_fds[-1] - valid_fds[0] > args.max_fd_growth:
                add_alert(alerts, f"{prefix} endpoint FD growth exceeded {args.max_fd_growth}")

        queue_specs = (
            ("sender_appsrc_queue_ms", args.max_sender_queue_ms),
            ("sender_ring_depth_samples", args.max_ring_depth_samples),
            ("receiver_buffer_depth_ms", args.max_receiver_buffer_ms),
        )
        summary["queues"] = {}
        for field, limit in queue_specs:
            values = [float(row[field]) for row in samples if row[field] not in (None, "")]
            series = [(elapsed_values[i], samples[i][field]) for i in range(len(samples))]
            summary["queues"][field] = {
                "first": values[0] if values else None,
                "last": values[-1] if values else None,
                "max": max(values) if values else None,
                "observedSlopePerHour": slope_per_hour(series),
                "limit": limit,
            }
            if values and max(values) > limit:
                add_alert(alerts, f"{field} exceeded {limit:g}")

        if not args.allow_idle:
            packets = [
                int(row["receiver_received_packets"])
                for row in samples
                if row["receiver_received_packets"] not in (None, "")
            ]
            if len(packets) >= 2 and packets[-1] <= packets[0]:
                add_alert(alerts, "receiver packet counter did not grow")

        for field in ("sender_capture_errors", "sender_pipeline_errors", "receiver_pipewire_errors"):
            values = [int(row[field]) for row in samples if row[field] not in (None, "")]
            if values and values[-1] > values[0]:
                add_alert(alerts, f"{field} increased from {values[0]} to {values[-1]}")

    summary["alerts"] = sorted(alerts)
    summary_path = output_dir / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(json.dumps(summary, indent=2, sort_keys=True))

    if args.fail_on_alert and alerts:
        return 2
    return 0


def main() -> int:
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, on_signal)
    args = parser().parse_args()
    try:
        return run(args)
    except (ValueError, RuntimeError, OSError) as error:
        log(f"error: {error}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
