#!/usr/bin/env bash
# Run a local synthetic sine -> RTP/Opus -> PipeWire receiver validation.
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
SENDER_BIN="${NOLAND_MIC_SENDER_BIN:-}"
RECEIVER_BIN="${NOLAND_MIC_RECEIVER_BIN:-}"
DURATION=15
INTERVAL=1
RTP_PORT=48200
RTCP_PORT=48201
RTCP_LISTEN_PORT=48202
SESSION_ID="local-loopback-$$"
OUTPUT_DIR=""
SKIP_NODE_CHECK=0
CONFIG_FILE=""

usage() {
    cat <<'EOF'
Usage: scripts/microphone-loopback.sh [options]

Runs the actual noland-mic-sender synthetic source through RTP/Opus on
127.0.0.1 into noland-mic-receiver. The receiver must run on Linux with the
provisioned PipeWire nodes noland_mic_sink and noland_mic_source.

Options:
  --sender-bin PATH          noland-mic-sender executable
  --receiver-bin PATH        noland-mic-receiver executable
  --duration SECONDS         validation duration (default: 15)
  --interval SECONDS         monitoring interval (default: 1)
  --rtp-port PORT            RTP port (default: 48200)
  --rtcp-port PORT           receiver RTCP port (default: 48201)
  --rtcp-listen-port PORT    sender RTCP receive port (default: 48202)
  --output-dir DIR           retain logs/results here
  --skip-pipewire-node-check skip the pw-cli name preflight
  -h, --help                 show this help

Environment alternatives:
  NOLAND_MIC_SENDER_BIN, NOLAND_MIC_RECEIVER_BIN
EOF
}

fail() {
    printf 'microphone-loopback: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [[ -n "${CONFIG_FILE}" && -f "${CONFIG_FILE}" ]]; then
        rm -f -- "${CONFIG_FILE}"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

resolve_binary() {
    local requested="$1"
    shift
    if [[ -n "${requested}" ]]; then
        [[ -x "${requested}" ]] || fail "binary is not executable: ${requested}"
        printf '%s\n' "${requested}"
        return
    fi
    local candidate
    for candidate in "$@"; do
        if [[ "${candidate}" != */* ]]; then
            if command -v "${candidate}" >/dev/null 2>&1; then
                command -v "${candidate}"
                return
            fi
        elif [[ -x "${candidate}" ]]; then
            printf '%s\n' "${candidate}"
            return
        fi
    done
    return 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sender-bin) [[ $# -ge 2 ]] || fail "--sender-bin requires a value"; SENDER_BIN="$2"; shift 2 ;;
        --receiver-bin) [[ $# -ge 2 ]] || fail "--receiver-bin requires a value"; RECEIVER_BIN="$2"; shift 2 ;;
        --duration) [[ $# -ge 2 ]] || fail "--duration requires a value"; DURATION="$2"; shift 2 ;;
        --interval) [[ $# -ge 2 ]] || fail "--interval requires a value"; INTERVAL="$2"; shift 2 ;;
        --rtp-port) [[ $# -ge 2 ]] || fail "--rtp-port requires a value"; RTP_PORT="$2"; shift 2 ;;
        --rtcp-port) [[ $# -ge 2 ]] || fail "--rtcp-port requires a value"; RTCP_PORT="$2"; shift 2 ;;
        --rtcp-listen-port) [[ $# -ge 2 ]] || fail "--rtcp-listen-port requires a value"; RTCP_LISTEN_PORT="$2"; shift 2 ;;
        --output-dir) [[ $# -ge 2 ]] || fail "--output-dir requires a value"; OUTPUT_DIR="$2"; shift 2 ;;
        --skip-pipewire-node-check) SKIP_NODE_CHECK=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done

[[ "$(uname -s)" == "Linux" ]] || fail "the receiver/PipeWire loopback currently requires Linux"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
command -v pw-cli >/dev/null 2>&1 || fail "pw-cli is required (install PipeWire tools)"

SENDER_BIN="$(resolve_binary "${SENDER_BIN}" \
    noland-mic-sender \
    "${REPO_ROOT}/mic-sidecar/target/release/noland-mic-sender" \
    "${REPO_ROOT}/mic-sidecar/target/debug/noland-mic-sender")" \
    || fail "noland-mic-sender not found; build it or pass --sender-bin"
RECEIVER_BIN="$(resolve_binary "${RECEIVER_BIN}" \
    noland-mic-receiver \
    "${REPO_ROOT}/vm-cloud-mic-agent/target/release/noland-mic-receiver" \
    "${REPO_ROOT}/vm-cloud-mic-agent/target/debug/noland-mic-receiver")" \
    || fail "noland-mic-receiver not found; build it or pass --receiver-bin"

if [[ "${SKIP_NODE_CHECK}" -eq 0 ]]; then
    PIPEWIRE_NODES="$(pw-cli ls Node 2>/dev/null)" || fail "cannot query PipeWire; ensure the user PipeWire service is running"
    grep -q 'noland_mic_sink' <<<"${PIPEWIRE_NODES}" \
        || fail "PipeWire node noland_mic_sink is absent; provision the Noland microphone loopback first"
    grep -q 'noland_mic_source' <<<"${PIPEWIRE_NODES}" \
        || fail "PipeWire node noland_mic_source is absent; provision the Noland microphone loopback first"
fi

python3 - "${RTP_PORT}" "${RTCP_PORT}" "${RTCP_LISTEN_PORT}" <<'PY'
import socket
import sys

ports = [int(value) for value in sys.argv[1:]]
if len(set(ports)) != len(ports) or any(port < 1 or port > 65535 for port in ports):
    raise SystemExit("ports must be distinct integers between 1 and 65535")
sockets = []
try:
    for port in ports:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("127.0.0.1", port))
        sockets.append(sock)
except OSError as error:
    raise SystemExit(f"UDP port preflight failed: {error}")
finally:
    for sock in sockets:
        sock.close()
PY

if [[ -z "${OUTPUT_DIR}" ]]; then
    OUTPUT_DIR="${REPO_ROOT}/.tmp/microphone-loopback-$(date -u +%Y%m%dT%H%M%SZ)"
fi
mkdir -p -- "${OUTPUT_DIR}"
CONFIG_FILE="$(mktemp "${OUTPUT_DIR}/receiver-config.XXXXXX.toml")"
cat >"${CONFIG_FILE}" <<EOF
[network]
bind_address = "127.0.0.1"
rtp_port = ${RTP_PORT}
rtcp_port = ${RTCP_PORT}
interface = "lo"
maximum_packet_size = 1200
recv_buffer_bytes = 524288

[audio]
sample_rate = 48000
channels = 1
frame_duration_ms = 10
pipewire_sink_name = "noland_mic_sink"

[jitter]
initial_ms = 20
minimum_ms = 10
maximum_ms = 60

[session]
session_id = "${SESSION_ID}"
expected_peer_ip = "127.0.0.1"
client_rtcp_port = ${RTCP_LISTEN_PORT}
EOF
chmod 0600 "${CONFIG_FILE}"

printf 'microphone-loopback: sender=%s\n' "${SENDER_BIN}" >&2
printf 'microphone-loopback: receiver=%s\n' "${RECEIVER_BIN}" >&2
printf 'microphone-loopback: output=%s\n' "${OUTPUT_DIR}" >&2

python3 "${SCRIPT_DIR}/microphone-soak.py" \
    --sender-bin "${SENDER_BIN}" \
    --receiver-bin "${RECEIVER_BIN}" \
    --receiver-config "${CONFIG_FILE}" \
    --session-id "${SESSION_ID}" \
    --host 127.0.0.1 \
    --rtp-port "${RTP_PORT}" \
    --rtcp-port "${RTCP_PORT}" \
    --rtcp-listen-port "${RTCP_LISTEN_PORT}" \
    --duration "${DURATION}" \
    --interval "${INTERVAL}" \
    --startup-grace 3 \
    --output-dir "${OUTPUT_DIR}" \
    --fail-on-alert

printf 'microphone-loopback: PASS; retained logs and metrics in %s\n' "${OUTPUT_DIR}"
