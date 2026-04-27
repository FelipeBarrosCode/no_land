#!/bin/bash
set -euo pipefail

TARGET_USER="${1:-user}"
USER_HOME="/home/${TARGET_USER}"
APP_DIR="${USER_HOME}/.local/share/applications"
BIN_DIR="${USER_HOME}/.local/bin"
WINE_PREFIX="${USER_HOME}/.wine-launchers"
PROTON_DIR="${USER_HOME}/.steam/root/compatibilitytools.d"
WINE_GE_DIR="${USER_HOME}/.local/share/wine-ge"
DXVK_DIR="${USER_HOME}/.local/share/dxvk"
VKD3D_DIR="${USER_HOME}/.local/share/vkd3d-proton"

GAME_COMPAT_UPDATED=0

log() {
  printf '[post-provision] %s\n' "$*"
}

run_user() {
  sudo -u "$TARGET_USER" -H bash -lc "$*"
}

ensure_packages() {
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -y
  apt-get install -y curl wget ca-certificates gnupg software-properties-common xdg-utils unzip python3 \
    tar xz-utils cabextract p7zip-full
}

install_chrome() {
  log "Installing Google Chrome"
  local deb_path="/tmp/google-chrome-stable_current_amd64.deb"
  wget -q "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb" -O "$deb_path"
  dpkg -i "$deb_path" || apt-get install -f -y
  rm -f "$deb_path"
  run_user "xdg-settings set default-web-browser google-chrome.desktop || true"
}

install_wine() {
  log "Installing latest Wine stable"
  dpkg --add-architecture i386
  mkdir -p /etc/apt/keyrings
  wget -qO /etc/apt/keyrings/winehq-archive.key https://dl.winehq.org/wine-builds/winehq.key

  local codename
  codename="$(. /etc/os-release && echo "${VERSION_CODENAME:-}")"
  if [[ -z "$codename" ]]; then
    codename="jammy"
  fi

  wget -qO "/etc/apt/sources.list.d/winehq-${codename}.sources" "https://dl.winehq.org/wine-builds/ubuntu/dists/${codename}/winehq-${codename}.sources" || true
  apt-get update -y
  apt-get install -y --install-recommends winehq-stable || apt-get install -y wine-stable
  apt-get install -y winetricks || true
}

fetch_latest_asset_url() {
  local repo="$1"
  local regex="$2"
  local api="https://api.github.com/repos/${repo}/releases/latest"

  curl -fsSL "$api" | python3 -c 'import json,re,sys
regex=re.compile(sys.argv[1])
d=json.load(sys.stdin)
for a in d.get("assets",[]):
    n=a.get("name","")
    if regex.search(n):
        print(a.get("browser_download_url",""))
        break
' "$regex"
}

extract_name_from_url() {
  local url="$1"
  local name
  name="${url##*/}"
  name="${name%.tar.gz}"
  name="${name%.tar.xz}"
  name="${name%.zip}"
  printf '%s\n' "$name"
}

install_proton_ge() {
  log "Updating Proton GE"
  mkdir -p "$PROTON_DIR"
  local url
  url="$(fetch_latest_asset_url "GloriousEggroll/proton-ge-custom" '^GE-Proton.*\\.tar\\.gz$' || true)"
  if [[ -z "$url" ]]; then
    log "Could not resolve latest Proton GE asset"
    return 0
  fi

  local version
  version="$(extract_name_from_url "$url")"
  if [[ -d "${PROTON_DIR}/${version}" ]]; then
    log "Proton GE already up to date (${version})"
    return 0
  fi

  local archive="/tmp/${version}.tar.gz"
  wget -q "$url" -O "$archive"
  run_user "mkdir -p '${PROTON_DIR}' && tar -xzf '${archive}' -C '${PROTON_DIR}'"
  rm -f "$archive"
  chown -R "$TARGET_USER:$TARGET_USER" "$PROTON_DIR"
  GAME_COMPAT_UPDATED=1
  log "Installed Proton GE ${version}"
}

install_wine_ge() {
  log "Updating Wine GE"
  mkdir -p "$WINE_GE_DIR"
  local url
  url="$(fetch_latest_asset_url "GloriousEggroll/wine-ge-custom" '.*\\.(tar\\.xz|tar\\.gz)$' || true)"
  if [[ -z "$url" ]]; then
    log "Could not resolve latest Wine GE asset"
    return 0
  fi

  local version
  version="$(extract_name_from_url "$url")"
  if [[ -d "${WINE_GE_DIR}/${version}" ]]; then
    log "Wine GE already up to date (${version})"
    return 0
  fi

  local archive_ext="${url##*.}"
  local archive="/tmp/${version}.tar.${archive_ext}"
  if [[ "$url" == *.tar.xz ]]; then
    archive="/tmp/${version}.tar.xz"
  elif [[ "$url" == *.tar.gz ]]; then
    archive="/tmp/${version}.tar.gz"
  fi
  wget -q "$url" -O "$archive"
  run_user "mkdir -p '${WINE_GE_DIR}'"
  if [[ "$archive" == *.tar.xz ]]; then
    run_user "tar -xJf '${archive}' -C '${WINE_GE_DIR}'"
  else
    run_user "tar -xzf '${archive}' -C '${WINE_GE_DIR}'"
  fi
  rm -f "$archive"
  chown -R "$TARGET_USER:$TARGET_USER" "$WINE_GE_DIR"
  GAME_COMPAT_UPDATED=1
  log "Installed Wine GE ${version}"
}

install_dxvk() {
  log "Updating DXVK"
  mkdir -p "$DXVK_DIR"
  local url
  url="$(fetch_latest_asset_url "doitsujin/dxvk" '^dxvk-.*\\.tar\\.gz$' || true)"
  if [[ -z "$url" ]]; then
    log "Could not resolve latest DXVK asset"
    return 0
  fi

  local version
  version="$(extract_name_from_url "$url")"
  if [[ -d "${DXVK_DIR}/${version}" ]]; then
    log "DXVK already up to date (${version})"
    return 0
  fi

  local archive="/tmp/${version}.tar.gz"
  wget -q "$url" -O "$archive"
  run_user "mkdir -p '${DXVK_DIR}' && tar -xzf '${archive}' -C '${DXVK_DIR}'"
  rm -f "$archive"
  chown -R "$TARGET_USER:$TARGET_USER" "$DXVK_DIR"
  GAME_COMPAT_UPDATED=1
  log "Installed DXVK ${version}"
}

install_vkd3d_proton() {
  log "Updating VKD3D-Proton"
  mkdir -p "$VKD3D_DIR"
  local url
  url="$(fetch_latest_asset_url "HansKristian-Work/vkd3d-proton" '^vkd3d-proton-.*\\.tar\\.zst$' || true)"
  if [[ -z "$url" ]]; then
    log "Could not resolve latest VKD3D-Proton asset"
    return 0
  fi

  local version
  version="${url##*/}"
  version="${version%.tar.zst}"
  if [[ -d "${VKD3D_DIR}/${version}" ]]; then
    log "VKD3D-Proton already up to date (${version})"
    return 0
  fi

  local archive="/tmp/${version}.tar.zst"
  wget -q "$url" -O "$archive"
  run_user "mkdir -p '${VKD3D_DIR}' && tar --zstd -xf '${archive}' -C '${VKD3D_DIR}'"
  rm -f "$archive"
  chown -R "$TARGET_USER:$TARGET_USER" "$VKD3D_DIR"
  GAME_COMPAT_UPDATED=1
  log "Installed VKD3D-Proton ${version}"
}

install_winetricks_runtime() {
  log "Installing compatibility runtimes into shared prefix"
  run_user "mkdir -p '${WINE_PREFIX}'"
  run_user "WINEPREFIX='${WINE_PREFIX}' wineboot --init >/dev/null 2>&1 || true"
  run_user "WINEPREFIX='${WINE_PREFIX}' winetricks -q vcrun2022 d3dcompiler_47 directx9 >/tmp/noland-winetricks.log 2>&1 || true"
}

install_game_compat() {
  log "Installing/updating game compatibility layer"
  install_proton_ge
  install_wine_ge
  install_dxvk
  install_vkd3d_proton
  install_winetricks_runtime
}

install_heroic_latest() {
  log "Installing latest Heroic AppImage"
  mkdir -p "$BIN_DIR" "$APP_DIR"

  local heroic_url
  heroic_url="$( (curl -fsSL https://api.github.com/repos/Heroic-Games-Launcher/HeroicGamesLauncher/releases/latest || true) | python3 -c 'import json,sys; d=json.load(sys.stdin); assets=d.get("assets",[]); u="";\nfor a in assets:\n n=a.get("name","")\n if n.endswith(".AppImage") and "arm" not in n.lower():\n  u=a.get("browser_download_url","")\n  break\nprint(u)')"

  if [[ -z "$heroic_url" ]]; then
    log "Could not detect latest Heroic release URL"
    return 1
  fi

  wget -q "$heroic_url" -O "${BIN_DIR}/heroic"
  chmod +x "${BIN_DIR}/heroic"
  chown "$TARGET_USER:$TARGET_USER" "${BIN_DIR}/heroic"

  cat > "${APP_DIR}/heroic.desktop" <<EOF
[Desktop Entry]
Name=Heroic Games Launcher
Comment=Epic, GOG and Amazon launcher
Exec=${BIN_DIR}/heroic
Icon=heroic
Terminal=false
Type=Application
Categories=Game;
StartupNotify=true
EOF
  chown "$TARGET_USER:$TARGET_USER" "${APP_DIR}/heroic.desktop"
}

setup_shared_wine_prefix() {
  log "Preparing shared Wine prefix"
  run_user "mkdir -p '${WINE_PREFIX}' '${APP_DIR}'"
  run_user "WINEPREFIX='${WINE_PREFIX}' wineboot --init >/dev/null 2>&1 || true"

  cat > "${APP_DIR}/ubisoft-connect.desktop" <<EOF
[Desktop Entry]
Name=Ubisoft Connect (Install Manually)
Comment=Run installer in shared Wine prefix
Exec=google-chrome 'https://ubisoftconnect.com'
Terminal=false
Type=Application
Categories=Game;
EOF

  cat > "${APP_DIR}/ea-app.desktop" <<EOF
[Desktop Entry]
Name=EA App (Install Manually)
Comment=Run installer in shared Wine prefix
Exec=google-chrome 'https://www.ea.com/ea-app'
Terminal=false
Type=Application
Categories=Game;
EOF

  cat > "${USER_HOME}/Desktop/Heroic-Store-Setup.txt" <<EOF
Heroic is installed and ready.

Inside Heroic, sign in to:
- Epic Games
- GOG
- Amazon Games

Ubisoft Connect and EA App use the shared Wine prefix:
${WINE_PREFIX}
EOF

  chown "$TARGET_USER:$TARGET_USER" "${APP_DIR}/ubisoft-connect.desktop" "${APP_DIR}/ea-app.desktop" "${USER_HOME}/Desktop/Heroic-Store-Setup.txt"
}

main() {
  if ! id "$TARGET_USER" >/dev/null 2>&1; then
    echo "Target user '$TARGET_USER' not found" >&2
    exit 2
  fi

  mkdir -p "${USER_HOME}/Desktop"
  ensure_packages
  install_chrome
  install_wine
  install_game_compat
  install_heroic_latest
  setup_shared_wine_prefix

  if [[ "$GAME_COMPAT_UPDATED" -eq 1 ]]; then
    log "Game compatibility layer updated; scheduling reboot in 1 minute"
    shutdown -r +1 "Noland: reboot after game compatibility updates" || true
  fi

  log "Post-provision setup complete"
}

main "$@"
