#!/usr/bin/env bash
set -euo pipefail

PID_FILE="/tmp/noland-mic-receiver.pid"

log() {
  printf '[stop-receiver] %s\n' "$*"
}

if [[ -f "${PID_FILE}" ]]; then
  PID="$(cat "${PID_FILE}")"
  if [[ -n "${PID}" ]] && kill -0 "${PID}" 2>/dev/null; then
    kill "${PID}" || true
    sleep 1
    kill -9 "${PID}" 2>/dev/null || true
    log "Stopped receiver pid ${PID}"
  else
    log "No live receiver process found for pid file"
  fi
  rm -f "${PID_FILE}"
else
  log "Receiver pid file not found"
fi

pkill -f "udpsrc port=.*rtpopusdepay" 2>/dev/null || true
