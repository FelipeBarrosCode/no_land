#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${HOME}/.config/noland-mic/config.env"

log() {
  printf '[create-virtual-mic] %s\n' "$*"
}

fail() {
  echo "[create-virtual-mic] ERROR: $*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

load_config() {
  [[ -f "${CONFIG_PATH}" ]] || fail "Config not found at ${CONFIG_PATH}"
  # shellcheck disable=SC1090
  source "${CONFIG_PATH}"
  : "${VIRTUAL_MIC_SINK:?VIRTUAL_MIC_SINK missing in config}"
  : "${VIRTUAL_MIC_SOURCE:?VIRTUAL_MIC_SOURCE missing in config}"
}

ensure_sink() {
  if pactl list short sinks | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SINK}"; then
    log "Sink '${VIRTUAL_MIC_SINK}' already exists"
    return
  fi

  pactl load-module module-null-sink \
    sink_name="${VIRTUAL_MIC_SINK}" \
    sink_properties="device.description=Cloud_Mic_Sink" \
    rate=48000 channels=1 >/dev/null
  log "Created sink '${VIRTUAL_MIC_SINK}'"
}

ensure_source() {
  if pactl list short sources | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SOURCE}"; then
    log "Source '${VIRTUAL_MIC_SOURCE}' already exists"
    return
  fi

  pactl load-module module-remap-source \
    master="${VIRTUAL_MIC_SINK}.monitor" \
    source_name="${VIRTUAL_MIC_SOURCE}" \
    source_properties="device.description=Cloud_Mic" >/dev/null
  log "Created source '${VIRTUAL_MIC_SOURCE}'"
}

main() {
  require_cmd pactl
  load_config
  ensure_sink
  ensure_source
  pactl set-default-source "${VIRTUAL_MIC_SOURCE}" || true
  log "Cloud_Mic virtual source is ready"
}

main "$@"
