#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${HOME}/.config/noland-mic/config.env"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

log() {
  printf '[install-local] %s\n' "$*"
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

install_macos() {
  require_cmd brew
  log "Installing GStreamer packages with Homebrew"
  brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav
}

install_linux() {
  require_cmd sudo
  require_cmd apt-get
  log "Installing GStreamer packages with apt"
  sudo apt-get update -y
  sudo apt-get install -y \
    gstreamer1.0-tools \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-libav \
    gstreamer1.0-pulseaudio
}

ensure_config() {
  mkdir -p "$(dirname "${CONFIG_PATH}")"
  if [[ ! -f "${CONFIG_PATH}" ]]; then
    cp "${ROOT_DIR}/config.env" "${CONFIG_PATH}"
    log "Created config at ${CONFIG_PATH}"
  else
    log "Config already exists at ${CONFIG_PATH}"
  fi
}

verify_bins() {
  require_cmd gst-launch-1.0
  log "Installed successfully. gst-launch-1.0 is available"
}

main() {
  case "$(uname -s)" in
    Darwin)
      install_macos
      ;;
    Linux)
      install_linux
      ;;
    *)
      echo "Unsupported OS: $(uname -s)" >&2
      exit 1
      ;;
  esac

  ensure_config
  verify_bins
  log "Done"
}

main "$@"
