#!/bin/bash
set -euo pipefail

# Production provisioning for the Noland microphone receiver on Ubuntu LTS.
# The authenticated SSH control plane allocates a session, writes/updates
# /etc/noland/microphone.toml, adjusts the peer-restricted UFW allocation when
# needed, and restarts only noland-mic-receiver.service. RTP itself is not an
# authentication boundary; WireGuard plus the source-IP firewall rule is.

USER_NAME="${1:-user}"
WG_INTERFACE="${NOLAND_MIC_WG_INTERFACE:-wg0}"
RTP_PORT="${NOLAND_MIC_RTP_PORT:-48200}"
RTCP_PORT="${NOLAND_MIC_RTCP_PORT:-48201}"
CLIENT_RTCP_PORT="${NOLAND_MIC_CLIENT_RTCP_PORT:-48202}"
FIREWALL_PORT_RANGE="${NOLAND_MIC_PORT_RANGE:-48200:48399}"
JITTER_MS="${NOLAND_MIC_JITTER_MS:-20}"
EXPECTED_SSRC="${NOLAND_MIC_EXPECTED_SSRC:-}"
SESSION_ID="${NOLAND_MIC_SESSION_ID:-}"

if ! id "$USER_NAME" >/dev/null 2>&1; then
    echo "Error: user '$USER_NAME' does not exist" >&2
    exit 1
fi

if [[ ! "$RTP_PORT" =~ ^[0-9]+$ || ! "$RTCP_PORT" =~ ^[0-9]+$ || ! "$CLIENT_RTCP_PORT" =~ ^[0-9]+$ || ! "$JITTER_MS" =~ ^[0-9]+$ ]]; then
    echo "Error: RTP, RTCP, client RTCP, and jitter values must be decimal integers" >&2
    exit 1
fi
RTP_PORT="$((10#$RTP_PORT))"
RTCP_PORT="$((10#$RTCP_PORT))"
CLIENT_RTCP_PORT="$((10#$CLIENT_RTCP_PORT))"
JITTER_MS="$((10#$JITTER_MS))"
if (( RTP_PORT < 1 || RTP_PORT > 65535 || RTCP_PORT < 1 || RTCP_PORT > 65535 || CLIENT_RTCP_PORT < 1 || CLIENT_RTCP_PORT > 65535 || RTP_PORT == RTCP_PORT )); then
    echo "Error: RTP, RTCP, and client RTCP ports must be valid; host RTP/RTCP must be distinct" >&2
    exit 1
fi
if (( JITTER_MS < 10 || JITTER_MS > 60 )); then
    echo "Error: NOLAND_MIC_JITTER_MS must be in 10..60" >&2
    exit 1
fi
if [[ -n "$EXPECTED_SSRC" && ! "$EXPECTED_SSRC" =~ ^[0-9]+$ ]]; then
    echo "Error: NOLAND_MIC_EXPECTED_SSRC must be an unsigned decimal integer" >&2
    exit 1
fi
if [[ -n "$EXPECTED_SSRC" ]]; then
    if (( ${#EXPECTED_SSRC} > 10 )); then
        echo "Error: NOLAND_MIC_EXPECTED_SSRC must fit in an unsigned 32-bit integer" >&2
        exit 1
    fi
    EXPECTED_SSRC="$((10#$EXPECTED_SSRC))"
    if (( EXPECTED_SSRC > 4294967295 )); then
        echo "Error: NOLAND_MIC_EXPECTED_SSRC must fit in an unsigned 32-bit integer" >&2
        exit 1
    fi
fi
if [[ ! "$WG_INTERFACE" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    echo "Error: NOLAND_MIC_WG_INTERFACE contains unsupported characters" >&2
    exit 1
fi
if [[ ! "$FIREWALL_PORT_RANGE" =~ ^([0-9]+):([0-9]+)$ ]]; then
    echo "Error: NOLAND_MIC_PORT_RANGE must use MIN:MAX syntax" >&2
    exit 1
fi
FIREWALL_MIN="$((10#${BASH_REMATCH[1]}))"
FIREWALL_MAX="$((10#${BASH_REMATCH[2]}))"
FIREWALL_PORT_RANGE="${FIREWALL_MIN}:${FIREWALL_MAX}"
if (( FIREWALL_MIN < 1 || FIREWALL_MAX > 65535 || FIREWALL_MIN > FIREWALL_MAX || RTP_PORT < FIREWALL_MIN || RTP_PORT > FIREWALL_MAX || RTCP_PORT < FIREWALL_MIN || RTCP_PORT > FIREWALL_MAX )); then
    echo "Error: firewall range must be valid and contain both allocated ports" >&2
    exit 1
fi

uid="$(id -u "$USER_NAME")"
group_name="$(id -gn "$USER_NAME")"
home_dir="$(getent passwd "$USER_NAME" | cut -d: -f6)"
if [[ -z "$home_dir" || ! -d "$home_dir" ]]; then
    echo "Error: home directory for '$USER_NAME' is unavailable" >&2
    exit 1
fi

INSTALL_DIR="${home_dir}/.local/bin"
SERVICE_DIR="${home_dir}/.config/systemd/user"
PULSE_DROPIN_DIR="${home_dir}/.config/pipewire/pipewire-pulse.conf.d"
PULSE_DROPIN="${PULSE_DROPIN_DIR}/80-noland-microphone-source.conf"
LEGACY_PIPEWIRE_DROPIN="${home_dir}/.config/pipewire/pipewire.conf.d/80-noland-microphone.conf"
CONFIG_DIR="/etc/noland"
CONFIG_FILE="${CONFIG_DIR}/microphone.toml"
RUNTIME_DIR_BASE="/run/noland"
STATUS_FILE="${RUNTIME_DIR_BASE}/noland_remote_microphone.status.json"
runtime_dir="/run/user/${uid}"
bus_path="${runtime_dir}/bus"
source_fifo="${runtime_dir}/noland-mic-source.pcm"

run_user() {
    sudo -u "$USER_NAME" env \
        HOME="$home_dir" \
        XDG_RUNTIME_DIR="$runtime_dir" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=${bus_path}" \
        "$@"
}

run_user_systemctl() {
    run_user systemctl --user "$@"
}

stop_user_receiver_for_upgrade() {
    if command -v timeout >/dev/null 2>&1; then
        if timeout --signal=TERM --kill-after=2s 8s \
            sudo -u "$USER_NAME" env \
                HOME="$home_dir" \
                XDG_RUNTIME_DIR="$runtime_dir" \
                DBUS_SESSION_BUS_ADDRESS="unix:path=${bus_path}" \
                systemctl --user stop noland-mic-receiver.service; then
            return
        fi
    else
        run_user_systemctl --no-block stop noland-mic-receiver.service >/dev/null 2>&1 || true
        sleep 1
    fi

    run_user_systemctl kill --kill-who=all --signal=KILL noland-mic-receiver.service >/dev/null 2>&1 || true
}

WG_IP="${NOLAND_MIC_WG_IP:-}"
if [[ -z "$WG_IP" ]] && command -v ip >/dev/null 2>&1; then
    WG_IP="$(ip -o -4 addr show dev "$WG_INTERFACE" 2>/dev/null | awk 'NR == 1 {split($4, address, "/"); print address[1]}' || true)"
fi
if [[ -z "$WG_IP" ]]; then
    echo "Error: no IPv4 address found on ${WG_INTERFACE}; set NOLAND_MIC_WG_IP explicitly" >&2
    exit 1
fi

WG_CLIENT_IP="${NOLAND_MIC_PEER_IP:-}"
if [[ -z "$WG_CLIENT_IP" ]] && command -v wg >/dev/null 2>&1; then
    WG_CLIENT_IP="$(wg show "$WG_INTERFACE" allowed-ips 2>/dev/null | awk '{for (i = 2; i <= NF; i++) if ($i ~ /^[0-9.]+\/32$/) {sub(/\/32$/, "", $i); print $i; exit}}' || true)"
fi
if [[ -z "$WG_CLIENT_IP" ]] && [[ -f "/etc/wireguard/${WG_INTERFACE}.conf" ]]; then
    WG_CLIENT_IP="$(awk -F'= *' '/AllowedIPs/ {split($2, ips, ","); for (i in ips) {gsub(/^ +| +$/, "", ips[i]); if (ips[i] ~ /^[0-9.]+\/32$/) {sub(/\/32$/, "", ips[i]); print ips[i]; exit}}}' "/etc/wireguard/${WG_INTERFACE}.conf" || true)"
fi
if [[ -z "$WG_CLIENT_IP" ]]; then
    echo "Error: WireGuard peer /32 is unknown; set NOLAND_MIC_PEER_IP explicitly" >&2
    exit 1
fi

validate_ipv4() {
    local address="$1"
    local first second third fourth
    if [[ ! "$address" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]]; then
        return 1
    fi
    first="${BASH_REMATCH[1]}"
    second="${BASH_REMATCH[2]}"
    third="${BASH_REMATCH[3]}"
    fourth="${BASH_REMATCH[4]}"
    (( 10#$first <= 255 && 10#$second <= 255 && 10#$third <= 255 && 10#$fourth <= 255 ))
}
if ! validate_ipv4 "$WG_IP" || ! validate_ipv4 "$WG_CLIENT_IP"; then
    echo "Error: WireGuard bind and peer values must be valid IPv4 addresses" >&2
    exit 1
fi

if [[ -z "$SESSION_ID" ]]; then
    if [[ -r /proc/sys/kernel/random/uuid ]]; then
        SESSION_ID="$(cat /proc/sys/kernel/random/uuid)"
    else
        echo "Error: set NOLAND_MIC_SESSION_ID; kernel UUID generation is unavailable" >&2
        exit 1
    fi
fi
if [[ ! "$SESSION_ID" =~ ^[A-Za-z0-9._:-]+$ ]]; then
    echo "Error: session ID must contain only letters, digits, dot, underscore, colon, or hyphen" >&2
    exit 1
fi

cat <<SUMMARY
=== Noland Microphone Receiver Installation ===
User:             ${USER_NAME}
WireGuard bind:   ${WG_IP} (${WG_INTERFACE})
Expected peer:    ${WG_CLIENT_IP}
Session:          ${SESSION_ID}
RTP / RTCP:       ${RTP_PORT} / ${RTCP_PORT}
Client RTCP:       ${CLIENT_RTCP_PORT}
Firewall range:   ${FIREWALL_PORT_RANGE}/udp
Jitter latency:   ${JITTER_MS} ms
SUMMARY

export DEBIAN_FRONTEND=noninteractive
apt-get update -y
# Runtime-only packages used by the pipeline and verification. The receiver
# writes raw PCM to a PipeWire-Pulse recording-source FIFO; no playback plugin is used.
apt-get install -y --no-install-recommends \
    pipewire \
    pipewire-pulse \
    wireplumber \
    pulseaudio-utils \
    python3 \
    gstreamer1.0-tools \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    ufw

install -d -m 0755 -o "$USER_NAME" -g "$group_name" "$INSTALL_DIR"
install -d -m 0755 -o "$USER_NAME" -g "$group_name" "$SERVICE_DIR"
install -d -m 0755 -o "$USER_NAME" -g "$group_name" "$PULSE_DROPIN_DIR"
install -d -m 0755 "$CONFIG_DIR"

# Stop the receiver before replacing its binary and output transport.
loginctl enable-linger "$USER_NAME"
systemctl start "user@${uid}.service"
for _ in 1 2 3 4 5; do
    [[ -S "$bus_path" ]] && break
    sleep 1
done
if [[ ! -S "$bus_path" ]]; then
    echo "Error: systemd user bus did not become available at ${bus_path}" >&2
    exit 1
fi
recording_source_was_present=0
if run_user pactl list short sources 2>/dev/null | awk '{print $2}' | grep -x noland_remote_microphone >/dev/null; then
    recording_source_was_present=1
fi
stop_user_receiver_for_upgrade
default_sink_before="$(run_user pactl get-default-sink 2>/dev/null || true)"
previous_default_source="$(run_user pactl get-default-source 2>/dev/null || true)"
rm -f "$INSTALL_DIR/noland-mic-source-setup" "$INSTALL_DIR/noland-mic-source-cleanup"
rm -f /tmp/noland_remote_microphone.pcm /tmp/noland_remote_microphone.module
# Remove only the legacy microphone loopback. The persistent Sunshine playback
# sink belongs to output provisioning and must survive microphone upgrades.
rm -f "$LEGACY_PIPEWIRE_DROPIN"

if [[ -f /tmp/noland-mic-receiver ]]; then
    install -m 0755 -o "$USER_NAME" -g "$group_name" /tmp/noland-mic-receiver "$INSTALL_DIR/noland-mic-receiver"
elif [[ ! -x "$INSTALL_DIR/noland-mic-receiver" ]]; then
    echo "Error: /tmp/noland-mic-receiver is absent and no installed receiver exists" >&2
    exit 1
fi

echo "d ${RUNTIME_DIR_BASE} 0750 ${USER_NAME} ${group_name} -" > /etc/tmpfiles.d/noland-mic.conf
systemd-tmpfiles --create /etc/tmpfiles.d/noland-mic.conf

# Publish a real recording device backed by a PCM FIFO. This intentionally uses
# PipeWire-Pulse's source module and creates no Audio/Sink or monitor device.
cat > "$PULSE_DROPIN" <<EOF
pulse.cmd = [
    {
        cmd = "load-module"
        args = "module-pipe-source source_name=noland_remote_microphone file=${source_fifo} format=s16le rate=48000 channels=1 source_properties=device.description=Noland-Microphone"
        flags = [ nofail ]
    }
]
EOF
chown "$USER_NAME:$group_name" "$PULSE_DROPIN"
chmod 0644 "$PULSE_DROPIN"

cat > "$CONFIG_FILE" <<TOML
[network]
bind_address = "${WG_IP}"
rtp_port = ${RTP_PORT}
rtcp_port = ${RTCP_PORT}
interface = "${WG_INTERFACE}"
maximum_packet_size = 1200
recv_buffer_bytes = 524288

[audio]
sample_rate = 48000
channels = 1
frame_duration_ms = 10
source_fifo_path = "${source_fifo}"

[jitter]
initial_ms = ${JITTER_MS}
minimum_ms = 10
maximum_ms = 60

[session]
session_id = "${SESSION_ID}"
expected_peer_ip = "${WG_CLIENT_IP}"
client_rtcp_port = ${CLIENT_RTCP_PORT}
TOML
if [[ -n "$EXPECTED_SSRC" ]]; then
    echo "expected_ssrc = ${EXPECTED_SSRC}" >> "$CONFIG_FILE"
fi
chown root:"$group_name" "$CONFIG_FILE"
chmod 0640 "$CONFIG_FILE"

# Root-owned control helper invoked only through Noland's authenticated SSH
# channel. It serializes allocation, configures a short-lived endpoint, and
# controls only the receiver service; the PipeWire topology is never touched.
cat > /usr/local/sbin/noland-mic-session-control <<'PYTHON'
#!/usr/bin/env python3
import argparse
import fcntl
import ipaddress
import json
import os
import pathlib
import random
import re
import socket
import subprocess
import tempfile
import time

CONFIG = pathlib.Path("/etc/noland/microphone.toml")
LOCK = pathlib.Path("/run/noland/mic-session-control.lock")
STATUS = pathlib.Path("/run/noland/noland_remote_microphone.status.json")
SESSION_RE = re.compile(r'^[A-Za-z0-9._:-]+$')
USER_RE = re.compile(r'^[A-Za-z0-9_-]+$')
INTERFACE_RE = re.compile(r'^[A-Za-z0-9_.-]+$')


def fail(message):
    raise SystemExit(message)


def run_user_systemctl(user, *arguments, check=True, timeout_seconds=None):
    import pwd
    uid = pwd.getpwnam(user).pw_uid
    runtime = f"/run/user/{uid}"
    env = os.environ.copy()
    env.update({
        "HOME": pwd.getpwnam(user).pw_dir,
        "XDG_RUNTIME_DIR": runtime,
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path={runtime}/bus",
    })
    return subprocess.run(
        ["runuser", "-u", user, "--", "env"]
        + [f"{key}={value}" for key, value in env.items()]
        + ["systemctl", "--user", *arguments],
        check=check,
        text=True,
        capture_output=True,
        timeout=timeout_seconds,
    )


def stop_receiver_service(user):
    try:
        return run_user_systemctl(
            user,
            "stop",
            "noland-mic-receiver.service",
            check=False,
            timeout_seconds=7,
        )
    except subprocess.TimeoutExpired:
        run_user_systemctl(
            user,
            "kill",
            "--kill-who=all",
            "--signal=KILL",
            "noland-mic-receiver.service",
            check=False,
            timeout_seconds=3,
        )
        return None


def current_value(text, key, default=None):
    match = re.search(rf'^\s*{re.escape(key)}\s*=\s*"?([^"\n]+)"?\s*$', text, re.MULTILINE)
    return match.group(1).strip() if match else default


def port_pair_is_free(bind_address, rtp_port, rtcp_port):
    sockets = []
    try:
        for port in (rtp_port, rtcp_port):
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.bind((bind_address, port))
            sockets.append(sock)
        return True
    except OSError:
        return False
    finally:
        for sock in sockets:
            sock.close()


def allocate_pair(bind_address, minimum, maximum):
    candidates = [port for port in range(minimum, maximum) if port % 2 == 0 and port + 1 <= maximum]
    random.SystemRandom().shuffle(candidates)
    for port in candidates:
        if port_pair_is_free(bind_address, port, port + 1):
            return port, port + 1
    fail(f"no free RTP/RTCP pair in {minimum}:{maximum}")


def atomic_config(content, group_id):
    fd, temporary = tempfile.mkstemp(prefix="microphone.", suffix=".toml", dir=str(CONFIG.parent))
    try:
        os.write(fd, content.encode("utf-8"))
        os.fsync(fd)
        os.close(fd)
        os.chown(temporary, 0, group_id)
        os.chmod(temporary, 0o640)
        os.replace(temporary, CONFIG)
    finally:
        try:
            os.close(fd)
        except OSError:
            pass
        if os.path.exists(temporary):
            os.unlink(temporary)


def configure_firewall(interface, bind_address, peer_ip, minimum, maximum):
    if not shutil_which("ufw"):
        return
    port_range = f"{minimum}:{maximum}"
    subprocess.run([
        "ufw", "allow", "in", "on", interface, "from", peer_ip, "to", bind_address,
        "port", port_range, "proto", "udp", "comment", "Noland mic allocated range",
    ], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run([
        "ufw", "deny", "in", "on", interface, "to", bind_address,
        "port", port_range, "proto", "udp", "comment", "Deny other Noland mic sources",
    ], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def shutil_which(command):
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        candidate = pathlib.Path(directory) / command
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return None


def start(args):
    import pwd
    if not USER_RE.fullmatch(args.user):
        fail("invalid user")
    if not SESSION_RE.fullmatch(args.session_id):
        fail("invalid session id")
    if not INTERFACE_RE.fullmatch(args.interface):
        fail("invalid WireGuard interface")
    peer = str(ipaddress.ip_address(args.peer_ip))
    bind = str(ipaddress.ip_address(args.bind_address))
    if ipaddress.ip_address(peer).version != ipaddress.ip_address(bind).version:
        fail("peer and bind address families differ")
    if not (10 <= args.jitter_ms <= 60):
        fail("jitter must be in 10..60 ms")
    if not (1 <= args.client_rtcp_port <= 65535):
        fail("client RTCP port is invalid")
    if not (1 <= args.port_min < args.port_max <= 65535):
        fail("allocation range is invalid")
    account = pwd.getpwnam(args.user)
    group_id = account.pw_gid
    if not (0 <= args.ssrc <= 0xFFFFFFFF):
        fail("SSRC must fit in an unsigned 32-bit integer")

    stop_receiver_service(args.user)
    rtp_port, rtcp_port = allocate_pair(bind, args.port_min, args.port_max)
    source_fifo = f"/run/user/{account.pw_uid}/noland-mic-source.pcm"
    content = f'''[network]\nbind_address = "{bind}"\nrtp_port = {rtp_port}\nrtcp_port = {rtcp_port}\ninterface = "{args.interface}"\nmaximum_packet_size = 1200\nrecv_buffer_bytes = 524288\n\n[audio]\nsample_rate = 48000\nchannels = 1\nframe_duration_ms = 10\nsource_fifo_path = "{source_fifo}"\n\n[jitter]\ninitial_ms = {args.jitter_ms}\nminimum_ms = 10\nmaximum_ms = 60\n\n[session]\nsession_id = "{args.session_id}"\nexpected_peer_ip = "{peer}"\nclient_rtcp_port = {args.client_rtcp_port}\nexpected_ssrc = {args.ssrc}\n'''
    atomic_config(content, group_id)
    configure_firewall(args.interface, bind, peer, args.port_min, args.port_max)
    STATUS.unlink(missing_ok=True)
    result = run_user_systemctl(args.user, "start", "noland-mic-receiver.service", check=False)
    if result.returncode != 0:
        fail(f"receiver start failed: {result.stderr.strip()}")
    active = None
    stable_active_samples = 0
    for _ in range(60):
        active = run_user_systemctl(
            args.user,
            "is-active",
            "noland-mic-receiver.service",
            check=False,
        )
        state = active.stdout.strip()
        if active.returncode == 0 and state == "active":
            stable_active_samples += 1
            if stable_active_samples >= 4:
                break
        else:
            stable_active_samples = 0
        if state == "failed":
            status = run_user_systemctl(
                args.user,
                "--no-pager",
                "--full",
                "status",
                "noland-mic-receiver.service",
                check=False,
            )
            fail(
                f"receiver entered terminal state {state}: "
                f"{status.stdout.strip()} {status.stderr.strip()}"
            )
        time.sleep(0.25)
    else:
        state = active.stdout.strip() if active is not None else "unknown"
        status = run_user_systemctl(
            args.user,
            "--no-pager",
            "--full",
            "status",
            "noland-mic-receiver.service",
            check=False,
        )
        fail(
            f"receiver did not become active within 15 seconds (last state: {state}): "
            f"{status.stdout.strip()} {status.stderr.strip()}"
        )
    print(json.dumps({
        "sessionId": args.session_id,
        "host": bind,
        "rtpPort": rtp_port,
        "rtcpPort": rtcp_port,
        "payloadType": 111,
        "clockRate": 48000,
        "channels": 1,
        "frameMs": 10,
        "jitterMs": args.jitter_ms,
        "rtcpMux": False,
    }, separators=(",", ":")))


def stop(args):
    if not USER_RE.fullmatch(args.user) or not SESSION_RE.fullmatch(args.session_id):
        fail("invalid user or session id")
    text = CONFIG.read_text(encoding="utf-8") if CONFIG.exists() else ""
    active_session = current_value(text, "session_id", "")
    if active_session != args.session_id:
        fail("session id does not own the active receiver")
    stop_receiver_service(args.user)
    STATUS.unlink(missing_ok=True)
    print(json.dumps({"sessionId": args.session_id, "stopped": True}, separators=(",", ":")))


def main():
    parser = argparse.ArgumentParser(prog="noland-mic-session-control")
    subparsers = parser.add_subparsers(dest="command", required=True)
    start_parser = subparsers.add_parser("start")
    start_parser.add_argument("--user", required=True)
    start_parser.add_argument("--session-id", required=True)
    start_parser.add_argument("--peer-ip", required=True)
    start_parser.add_argument("--bind-address", required=True)
    start_parser.add_argument("--interface", default="wg0")
    start_parser.add_argument("--ssrc", required=True, type=int)
    start_parser.add_argument("--client-rtcp-port", required=True, type=int)
    start_parser.add_argument("--jitter-ms", type=int, default=20)
    start_parser.add_argument("--port-min", type=int, default=48200)
    start_parser.add_argument("--port-max", type=int, default=48399)
    stop_parser = subparsers.add_parser("stop")
    stop_parser.add_argument("--user", required=True)
    stop_parser.add_argument("--session-id", required=True)
    args = parser.parse_args()
    LOCK.parent.mkdir(parents=True, exist_ok=True)
    with LOCK.open("a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        start(args) if args.command == "start" else stop(args)


if __name__ == "__main__":
    main()
PYTHON
chmod 0755 /usr/local/sbin/noland-mic-session-control

cat > "$SERVICE_DIR/noland-mic-receiver.service" <<EOF
[Unit]
Description=Noland Microphone RTP/Opus Receiver
Documentation=file://${CONFIG_DIR}/README.control-plane
Wants=network-online.target pipewire-pulse.service
After=network-online.target pipewire-pulse.service
PartOf=pipewire-pulse.service
StartLimitIntervalSec=0

[Service]
Type=simple
ExecStart=${INSTALL_DIR}/noland-mic-receiver --config ${CONFIG_FILE}
Restart=on-failure
RestartSec=2s
KillMode=control-group
TimeoutStopSec=5s
SendSIGKILL=yes
# Prevent GStreamer from forking a plugin-scanner process on every start.
# The scanner tries to load libgstwebrtcds which is unavailable in the VM,
# producing spurious warnings. GST_REGISTRY_FORK=no skips the fork entirely;
# the in-process scan still runs but does not emit the external-scan failure.
Environment="GST_REGISTRY_FORK=no"
# This is already an unprivileged user service. Capability- and namespace-based
# sandbox directives are intentionally omitted because restricted Ubuntu VM and
# container kernels can reject them before ExecStart with 218/CAPABILITIES.
UMask=0077
LimitNOFILE=4096

[Install]
WantedBy=default.target
EOF
chown "$USER_NAME:$group_name" "$SERVICE_DIR/noland-mic-receiver.service"
chmod 0644 "$SERVICE_DIR/noland-mic-receiver.service"

cat > "$CONFIG_DIR/README.control-plane" <<EOF
Noland microphone session allocation
====================================
The authenticated SSH control plane is responsible for assigning session_id,
expected_peer_ip, client_rtcp_port, optional expected_ssrc, and the RTP/RTCP port pair in
${CONFIG_FILE}. It must keep the UFW source-IP/range rule synchronized and then
restart noland-mic-receiver.service as ${USER_NAME}. RTP packets are PT111 Opus,
48 kHz mono, and every UDP datagram must be <=1200 bytes. RTCP sender reports go
to ${RTCP_PORT}; receiver reports are returned to ${WG_CLIENT_IP}:${CLIENT_RTCP_PORT}.
WireGuard and UFW provide peer authentication; session_id is correlation data,
not packet authentication. Decoded S16LE PCM is written to ${source_fifo}, which
backs the PipeWire-Pulse recording device noland_remote_microphone. No playback sink is
created or modified by microphone provisioning.
EOF
chmod 0644 "$CONFIG_DIR/README.control-plane"

# Permit only the allocated/static range from the authenticated WireGuard peer,
# then deny every other source to that range on the WireGuard interface.
if ! ufw show added | grep -F "$WG_CLIENT_IP" | grep -F "$FIREWALL_PORT_RANGE" >/dev/null 2>&1; then
    ufw allow in on "$WG_INTERFACE" from "$WG_CLIENT_IP" to "$WG_IP" port "$FIREWALL_PORT_RANGE" proto udp comment 'Noland mic peer RTP RTCP'
fi
if ! ufw show added | grep -F "deny in on ${WG_INTERFACE}" | grep -F "$WG_IP" | grep -F "$FIREWALL_PORT_RANGE" >/dev/null 2>&1; then
    ufw deny in on "$WG_INTERFACE" to "$WG_IP" port "$FIREWALL_PORT_RANGE" proto udp comment 'Deny other Noland mic sources'
fi
if ! ufw status | grep -q '^Status: active$'; then
    echo "Warning: UFW rules were installed but UFW is inactive; enable it only after confirming SSH access"
fi

run_user_systemctl daemon-reload
run_user_systemctl enable pipewire-pulse.socket noland-mic-receiver.service
# Load the recording-source drop-in only when the source is absent. Reusing an
# existing source avoids disconnecting browsers and disturbing playback defaults.
if (( recording_source_was_present == 0 )); then
    run_user_systemctl restart pipewire-pulse.service
    sleep 2
else
    echo "[OK] Existing noland_remote_microphone source reused without restarting PipeWire-Pulse"
fi

if run_user pactl list short sources 2>/dev/null | awk '{print $2}' | grep -qx noland_remote_microphone; then
    run_user pactl set-source-mute noland_remote_microphone 0
    run_user pactl set-source-volume noland_remote_microphone 100%
fi
if [[ -n "$previous_default_source" && "$previous_default_source" != noland_mic_* && \
      "$previous_default_source" != "noland_remote_microphone" ]] && \
   run_user pactl list short sources | awk '{print $2}' | grep -qx "$previous_default_source"; then
    run_user pactl set-default-source "$previous_default_source"
elif run_user pactl list short sources | awk '{print $2}' | grep -qx noland_remote_microphone; then
    # Replace a legacy Noland recording default with the FIFO-backed source.
    run_user pactl set-default-source noland_remote_microphone
fi

run_user_systemctl restart noland-mic-receiver.service
sleep 2

echo
echo "=== Verification ==="
verification_failed=0
missing_elements=()
for element in filesink rtpbin rtpopusdepay opusdec; do
    if ! run_user gst-inspect-1.0 "$element" >/dev/null 2>&1; then
        missing_elements+=("$element")
    fi
done
if (( ${#missing_elements[@]} == 0 )); then
    echo "[OK] Required GStreamer elements are installed"
else
    echo "[FAIL] Missing GStreamer elements: ${missing_elements[*]}"
    verification_failed=1
fi
if run_user pactl list short sources 2>/dev/null | awk '{print $2}' | grep -qx noland_remote_microphone && \
   run_user pactl list sources 2>/dev/null | grep -q 'Description: Noland-Microphone'; then
    echo "[OK] Recording source noland_remote_microphone is published as Noland-Microphone"
else
    echo "[FAIL] PipeWire recording source or friendly description was not found"
    verification_failed=1
fi
if [[ -p "$source_fifo" ]]; then
    echo "[OK] Recording source FIFO exists at ${source_fifo}"
else
    echo "[FAIL] Recording source FIFO is unavailable"
    verification_failed=1
fi
if run_user pactl list short sinks 2>/dev/null | grep -q 'noland_mic_sink'; then
    echo "[WARN] Legacy noland_mic_sink remains inert until the next normal PipeWire restart"
else
    echo "[OK] Microphone provisioning exposes no playback sink"
fi
if run_user pactl get-source-mute noland_remote_microphone 2>/dev/null | grep -q 'Mute: no'; then
    echo "[OK] Noland microphone source is unmuted"
else
    echo "[FAIL] Noland microphone source is muted or unavailable"
    verification_failed=1
fi
default_sink_after="$(run_user pactl get-default-sink 2>/dev/null || true)"
if [[ -n "$default_sink_before" && "$default_sink_after" == "$default_sink_before" ]]; then
    echo "[OK] Playback default was not changed: ${default_sink_after}"
else
    echo "[FAIL] Playback default changed during microphone provisioning: before=${default_sink_before:-missing} after=${default_sink_after:-missing}"
    verification_failed=1
fi
if run_user_systemctl is-active noland-mic-receiver.service >/dev/null 2>&1; then
    echo "[OK] noland-mic-receiver is running"
else
    echo "[FAIL] noland-mic-receiver failed to start"
    run_user_systemctl --no-pager status noland-mic-receiver.service || true
    verification_failed=1
fi
if ss -uln | grep -q ":${RTP_PORT} " && ss -uln | grep -q ":${RTCP_PORT} "; then
    echo "[OK] RTP ${RTP_PORT}/udp and RTCP ${RTCP_PORT}/udp are listening"
else
    echo "[FAIL] One or both UDP listeners were not detected"
    verification_failed=1
fi
if [[ -f "$STATUS_FILE" ]]; then
    echo "[OK] Receiver status JSON exists at ${STATUS_FILE}"
else
    echo "[FAIL] Receiver status JSON is not available yet"
    verification_failed=1
fi

if (( verification_failed != 0 )); then
    echo
    echo "Installation verification failed; inspect the service status above." >&2
    exit 1
fi

# Written only after the unit, PipeWire topology, listeners, and status writer
# have all passed verification. The client uses this to force safe upgrades of
# older remote helpers before allocating a media session.
echo "8" > "$CONFIG_DIR/microphone-agent.version"
chmod 0644 "$CONFIG_DIR/microphone-agent.version"

echo
echo "Installation complete."
echo "Manage: systemctl --user {start|stop|restart|status} noland-mic-receiver.service"
echo "Logs:   journalctl --user -u noland-mic-receiver.service -f"
echo "Status: ${STATUS_FILE}"
echo "Mic:    Noland-Microphone (source_name=noland_remote_microphone)"
