#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${HOME}/.config/noland-mic/config.env"
LOG_FILE="/tmp/noland-mic-sender.log"
PID_FILE="/tmp/noland-mic-sender.pid"

log() {
  printf '[start-sender] %s\n' "$*"
}

fail() {
  echo "[start-sender] ERROR: $*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

load_config() {
  [[ -f "${CONFIG_PATH}" ]] || fail "Config not found at ${CONFIG_PATH}"
  # shellcheck disable=SC1090
  source "${CONFIG_PATH}"
  : "${VM_IP:?VM_IP missing in config}"
  : "${PORT:?PORT missing in config}"
  : "${SAMPLE_RATE:?SAMPLE_RATE missing in config}"
  : "${CHANNELS:?CHANNELS missing in config}"
  : "${OPUS_BITRATE:?OPUS_BITRATE missing in config}"
  : "${OPUS_FRAME_SIZE:?OPUS_FRAME_SIZE missing in config}"
}

already_running() {
  if [[ -f "${PID_FILE}" ]]; then
    local pid
    pid="$(cat "${PID_FILE}")"
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      log "Sender already running (pid ${pid})"
      return 0
    fi
  fi
  return 1
}

main() {
  require_cmd gst-launch-1.0
  load_config

  if already_running; then
    exit 0
  fi

  case "$(uname -s)" in
    Darwin)
      SRC_ELEM="osxaudiosrc"
      SRC_ARGS="device=default latency-time=5000 buffer-time=10000"
      ;;
    Linux)
      SRC_ELEM="pulsesrc"
      SRC_ARGS="device=default latency-time=5000 buffer-time=10000"
      ;;
    *)
      fail "Unsupported OS: $(uname -s)"
      ;;
  esac

  nohup gst-launch-1.0 -e \
    "${SRC_ELEM}" ${SRC_ARGS} \
    ! audioconvert \
    ! audioresample \
    ! "audio/x-raw,rate=${SAMPLE_RATE},channels=${CHANNELS}" \
    ! opusenc frame-size="${OPUS_FRAME_SIZE}" bitrate="${OPUS_BITRATE}" audio-type=voice \
    ! rtpopuspay pt=96 \
    ! udpsink host="${VM_IP}" port="${PORT}" sync=false async=false \
    >"${LOG_FILE}" 2>&1 &

  echo "$!" > "${PID_FILE}"
  log "Sender started (pid $(cat "${PID_FILE}"))"
  log "Streaming to ${VM_IP}:${PORT}"
  log "Logs: ${LOG_FILE}"
}

main "$@"
