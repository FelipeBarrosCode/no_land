#!/usr/bin/env bash
set -euo pipefail

PID_FILE="/tmp/noland-mic-sender.pid"

log() {
  printf '[stop-sender] %s\n' "$*"
}

if [[ -f "${PID_FILE}" ]]; then
  PID="$(cat "${PID_FILE}")"
  if [[ -n "${PID}" ]] && kill -0 "${PID}" 2>/dev/null; then
    kill "${PID}" || true
    sleep 1
    kill -9 "${PID}" 2>/dev/null || true
    log "Stopped sender pid ${PID}"
  else
    log "No live sender process found for pid file"
  fi
  rm -f "${PID_FILE}"
else
  log "Sender pid file not found"
fi

pkill -f "rtpopuspay pt=96.*udpsink" 2>/dev/null || true
