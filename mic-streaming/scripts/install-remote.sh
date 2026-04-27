#!/usr/bin/env bash
set -euo pipefail

TARGET_USER="${1:-user}"
CONFIG_PATH="/home/${TARGET_USER}/.config/noland-mic/config.env"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

log() {
  printf '[install-remote] %s\n' "$*"
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

install_packages() {
  require_cmd apt-get
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -y
  sudo apt-get install -y \
    gstreamer1.0-tools \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-libav \
    gstreamer1.0-pulseaudio \
    pulseaudio-utils
}

install_config() {
  sudo -u "${TARGET_USER}" mkdir -p "/home/${TARGET_USER}/.config/noland-mic"
  if [[ ! -f "${CONFIG_PATH}" ]]; then
    sudo -u "${TARGET_USER}" cp "${ROOT_DIR}/config.env" "${CONFIG_PATH}"
    log "Wrote ${CONFIG_PATH}"
  else
    log "Config already present: ${CONFIG_PATH}"
  fi
}

main() {
  if ! id "${TARGET_USER}" >/dev/null 2>&1; then
    echo "Target user '${TARGET_USER}' not found" >&2
    exit 2
  fi

  install_packages
  install_config
  log "Done"
}

main "$@"
