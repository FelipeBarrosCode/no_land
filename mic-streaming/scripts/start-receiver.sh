#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${HOME}/.config/noland-mic/config.env"
LOG_FILE="/tmp/noland-mic-receiver.log"
PID_FILE="/tmp/noland-mic-receiver.pid"

log() {
  printf '[start-receiver] %s\n' "$*"
}

fail() {
  echo "[start-receiver] ERROR: $*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

load_config() {
  [[ -f "${CONFIG_PATH}" ]] || fail "Config not found at ${CONFIG_PATH}"
  # shellcheck disable=SC1090
  source "${CONFIG_PATH}"
  : "${PORT:?PORT missing in config}"
  : "${JITTER_LATENCY_MS:?JITTER_LATENCY_MS missing in config}"
  : "${VIRTUAL_MIC_SINK:?VIRTUAL_MIC_SINK missing in config}"
}

already_running() {
  if [[ -f "${PID_FILE}" ]]; then
    local pid
    pid="$(cat "${PID_FILE}")"
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      log "Receiver already running (pid ${pid})"
      return 0
    fi
  fi
  return 1
}

main() {
  require_cmd gst-launch-1.0
  require_cmd pactl
  load_config

  "$(dirname "$0")/create-virtual-mic.sh"

  if ! pactl list short sinks | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SINK}"; then
    fail "Sink '${VIRTUAL_MIC_SINK}' does not exist"
  fi

  if already_running; then
    exit 0
  fi

  nohup gst-launch-1.0 -e \
    udpsrc port="${PORT}" caps="application/x-rtp,media=audio,clock-rate=48000,encoding-name=OPUS,payload=96" \
    ! rtpjitterbuffer latency="${JITTER_LATENCY_MS}" drop-on-latency=true \
    ! rtpopusdepay \
    ! opusdec \
    ! audioconvert \
    ! audioresample \
    ! pulsesink device="${VIRTUAL_MIC_SINK}" sync=false async=false \
    >"${LOG_FILE}" 2>&1 &

  echo "$!" > "${PID_FILE}"
  log "Receiver started (pid $(cat "${PID_FILE}"))"
  log "Logs: ${LOG_FILE}"
}

main "$@"
