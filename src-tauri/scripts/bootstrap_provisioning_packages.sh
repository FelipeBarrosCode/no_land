#!/usr/bin/env bash
# One serialized package transaction per provisioning run. Installers retain
# their standalone fallbacks but find their prerequisites already installed.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1
packages=(ca-certificates curl python3 x11-utils x11-xserver-utils xclip alsa-utils
  pipewire pipewire-pulse wireplumber rtkit pulseaudio-utils wireguard-tools
  iproute2 ufw libelf1 zlib1g gstreamer1.0-tools gstreamer1.0-pipewire
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good)
if [[ "${NOLAND_PREBUILT_AGENTS:-0}" != 1 ]]; then
  packages+=(build-essential clang llvm pkg-config libelf-dev zlib1g-dev
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev)
fi
if [[ "${NOLAND_PACKAGE_PLAN_ONLY:-0}" == 1 ]]; then
  printf '%s\n' "${packages[@]}"
  exit 0
fi
exec 9>/run/lock/noland-provision-packages.lock
flock -w 600 9
missing=()
for package in "${packages[@]}"; do
  if [[ "$(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true)" != 'install ok installed' ]]; then
    missing+=("$package")
  fi
done
if (( ${#missing[@]} )); then
  apt-get -o DPkg::Lock::Timeout=600 -o Acquire::Retries=3 update
  apt-get -o DPkg::Lock::Timeout=600 -o Acquire::Retries=3 install -y --no-install-recommends "${missing[@]}"
fi
echo NOLAND_PROVISION_PACKAGES_READY
touch /run/noland-provision-packages-ready
