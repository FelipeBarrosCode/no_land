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
INSTALLER_DIR="${USER_HOME}/Downloads/game-launchers"
BOTTLES_APP_ID="com.usebottles.bottles"
BOTTLES_GAMES_DIR="/srv/games"

GAME_COMPAT_UPDATED=0
OS_ID=""
OS_VERSION=""
OS_CODENAME=""

log() {
  printf '[post-provision] %s\n' "$*"
}

run_user() {
  sudo -u "$TARGET_USER" -H bash -lc "$*"
}

detect_os() {
  . /etc/os-release
  OS_ID="${ID:-ubuntu}"
  OS_VERSION="${VERSION_ID:-}"
  OS_CODENAME="${VERSION_CODENAME:-}"
  log "Detected OS: ${OS_ID} ${OS_VERSION} (${OS_CODENAME})"
}

install_optional_package() {
  local package_name="$1"
  if apt-get install -y "$package_name"; then
    return 0
  fi

  log "Optional package unavailable: ${package_name}"
  return 1
}

install_first_available_package() {
  local package_name
  for package_name in "$@"; do
    if install_optional_package "$package_name"; then
      return 0
    fi
  done

  return 1
}

ensure_packages() {
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -y
  apt-get install -y curl wget ca-certificates gnupg software-properties-common xdg-utils unzip python3 \
    tar xz-utils cabextract p7zip-full flatpak zstd

  install_first_available_package libfuse2 libfuse2t64 || true
  install_first_available_package fuse3 fuse || true
}

install_bottles() {
  log "Installing Bottles via Flatpak"
  flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
  run_user "flatpak install -y flathub '${BOTTLES_APP_ID}'"
}

configure_bottles_access() {
  log "Configuring Bottles filesystem access"
  mkdir -p "${BOTTLES_GAMES_DIR}/epic" "${BOTTLES_GAMES_DIR}/ea" "${BOTTLES_GAMES_DIR}/ubisoft" "${BOTTLES_GAMES_DIR}/rockstar" "${BOTTLES_GAMES_DIR}/battlenet" "${BOTTLES_GAMES_DIR}/gog"
  chown -R "$TARGET_USER:$TARGET_USER" "${BOTTLES_GAMES_DIR}"
  chmod -R 775 "${BOTTLES_GAMES_DIR}"
  run_user "flatpak override --user '${BOTTLES_APP_ID}' --filesystem='${USER_HOME}/Downloads'"
  run_user "flatpak override --user '${BOTTLES_APP_ID}' --filesystem='${BOTTLES_GAMES_DIR}'"
}

download_launcher_installers() {
  log "Downloading launcher installers"
  run_user "mkdir -p '${INSTALLER_DIR}'"

  run_user "curl -fL 'https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/installer/download/EpicGamesLauncherInstaller.msi' -o '${INSTALLER_DIR}/EpicGamesLauncherInstaller.msi'"
  run_user "curl -fL 'https://origin-a.akamaihd.net/EA-Desktop-Client-Download/installer-releases/EAappInstaller.exe' -o '${INSTALLER_DIR}/EAappInstaller.exe'"
  run_user "curl -fL 'https://static3.cdn.ubi.com/orbit/launcher_installer/UbisoftConnectInstaller.exe' -o '${INSTALLER_DIR}/UbisoftConnectInstaller.exe'"
  run_user "curl -fL 'https://downloader.battle.net/download/getInstaller?os=win&installer=Battle.net-Setup.exe' -o '${INSTALLER_DIR}/BattleNet-Setup.exe'"
  run_user "curl -fL 'https://gamedownloads.rockstargames.com/public/installer/Rockstar-Games-Launcher.exe' -o '${INSTALLER_DIR}/Rockstar-Games-Launcher.exe'"
  run_user "curl -fL 'https://webinstallers.gog-statics.com/download/GOG_Galaxy_2.0.exe' -o '${INSTALLER_DIR}/GOG_Galaxy_2.0.exe'"
}

create_bottle_if_missing() {
  local bottle_name="$1"
  local bottles
  bottles="$(run_user "flatpak run --command=bottles-cli '${BOTTLES_APP_ID}' list bottles 2>/dev/null || true")"

  if printf '%s\n' "$bottles" | grep -Eq "(^|[[:space:]])${bottle_name}([[:space:]]|$)"; then
    log "Bottle already exists: ${bottle_name}"
    return 0
  fi

  log "Creating bottle: ${bottle_name}"
  run_user "flatpak run --command=bottles-cli '${BOTTLES_APP_ID}' new --bottle-name '${bottle_name}' --environment gaming --arch win64"
}

write_launcher_install_script() {
  log "Writing launcher installer helper script"
  run_user "cat > '${USER_HOME}/run-launcher-installers.sh' <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

BCLI='flatpak run --command=bottles-cli com.usebottles.bottles'
DIR='\$HOME/Downloads/game-launchers'

\$BCLI run -b epic -e \"\$DIR/EpicGamesLauncherInstaller.msi\"
\$BCLI run -b ea -e \"\$DIR/EAappInstaller.exe\"
\$BCLI run -b ubisoft -e \"\$DIR/UbisoftConnectInstaller.exe\"
\$BCLI run -b battlenet -e \"\$DIR/BattleNet-Setup.exe\"
\$BCLI run -b rockstar -e \"\$DIR/Rockstar-Games-Launcher.exe\"
\$BCLI run -b gog -e \"\$DIR/GOG_Galaxy_2.0.exe\"
EOF
chmod +x '${USER_HOME}/run-launcher-installers.sh'"
}

setup_bottles_launchers() {
  log "Preparing Bottles launcher bottles"
  install_bottles
  configure_bottles_access
  download_launcher_installers

  create_bottle_if_missing "epic"
  create_bottle_if_missing "ea"
  create_bottle_if_missing "ubisoft"
  create_bottle_if_missing "battlenet"
  create_bottle_if_missing "rockstar"
  create_bottle_if_missing "gog"

  write_launcher_install_script
  log "Installers prepared. In desktop session run: bash ~/run-launcher-installers.sh"
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
  codename="${OS_CODENAME:-}"
  if [[ -z "$codename" ]]; then
    codename="jammy"
  fi

  if wget -qO "/etc/apt/sources.list.d/winehq-${codename}.sources" "https://dl.winehq.org/wine-builds/ubuntu/dists/${codename}/winehq-${codename}.sources"; then
    apt-get update -y
    apt-get install -y --install-recommends winehq-stable || apt-get install -y wine-stable
  else
    log "WineHQ source unavailable for ${codename}; falling back to distro wine"
    apt-get update -y
    apt-get install -y wine-stable || true
  fi

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
  run_user "mkdir -p '${PROTON_DIR}'"
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
  GAME_COMPAT_UPDATED=1
  log "Installed Proton GE ${version}"
}

install_wine_ge() {
  log "Updating Wine GE"
  run_user "mkdir -p '${WINE_GE_DIR}'"
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
  GAME_COMPAT_UPDATED=1
  log "Installed Wine GE ${version}"
}

install_dxvk() {
  log "Updating DXVK"
  run_user "mkdir -p '${DXVK_DIR}'"
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
  GAME_COMPAT_UPDATED=1
  log "Installed DXVK ${version}"
}

install_vkd3d_proton() {
  log "Updating VKD3D-Proton"
  run_user "mkdir -p '${VKD3D_DIR}'"
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
  run_user "mkdir -p '${BIN_DIR}' '${APP_DIR}'"

  local heroic_url
  heroic_url="$( (curl -fsSL https://api.github.com/repos/Heroic-Games-Launcher/HeroicGamesLauncher/releases/latest || true) | python3 -c 'import json,sys; d=json.load(sys.stdin); assets=d.get("assets",[]); u="";\nfor a in assets:\n n=a.get("name","")\n if n.endswith(".AppImage") and "arm" not in n.lower():\n  u=a.get("browser_download_url","")\n  break\nprint(u)')"

  if [[ -z "$heroic_url" ]]; then
    log "Could not detect latest Heroic release URL"
    return 1
  fi

  run_user "wget -q '${heroic_url}' -O '${BIN_DIR}/heroic' && chmod +x '${BIN_DIR}/heroic'"

  run_user "cat > '${APP_DIR}/heroic.desktop' <<EOF
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
"
}

setup_shared_wine_prefix() {
  log "Preparing shared Wine prefix"
  run_user "mkdir -p '${WINE_PREFIX}' '${APP_DIR}' '${USER_HOME}/Desktop'"
  run_user "WINEPREFIX='${WINE_PREFIX}' wineboot --init >/dev/null 2>&1 || true"

  run_user "cat > '${APP_DIR}/ubisoft-connect.desktop' <<EOF
[Desktop Entry]
Name=Ubisoft Connect (Install Manually)
Comment=Run installer in shared Wine prefix
Exec=google-chrome 'https://ubisoftconnect.com'
Terminal=false
Type=Application
Categories=Game;
EOF
"

  run_user "cat > '${APP_DIR}/ea-app.desktop' <<EOF
[Desktop Entry]
Name=EA App (Install Manually)
Comment=Run installer in shared Wine prefix
Exec=google-chrome 'https://www.ea.com/ea-app'
Terminal=false
Type=Application
Categories=Game;
EOF
"

  run_user "cat > '${USER_HOME}/Desktop/Heroic-Store-Setup.txt' <<EOF
Heroic is installed and ready.

Inside Heroic, sign in to:
- Epic Games
- GOG
- Amazon Games

Ubisoft Connect and EA App use the shared Wine prefix:
${WINE_PREFIX}
EOF
"

}

repair_user_permissions() {
  chown -R "$TARGET_USER:$TARGET_USER" "${USER_HOME}/.steam" "${USER_HOME}/.local" "${USER_HOME}/Desktop" "${WINE_PREFIX}" 2>/dev/null || true
}

main() {
  if ! id "$TARGET_USER" >/dev/null 2>&1; then
    echo "Target user '$TARGET_USER' not found" >&2
    exit 2
  fi

  detect_os
  run_user "mkdir -p '${USER_HOME}/Desktop'"
  ensure_packages
  install_chrome
  install_wine
  install_game_compat
  install_heroic_latest
  setup_bottles_launchers
  setup_shared_wine_prefix
  repair_user_permissions

  if [[ "$GAME_COMPAT_UPDATED" -eq 1 ]]; then
    log "Game compatibility layer updated; scheduling reboot in 1 minute"
    shutdown -r +1 "Noland: reboot after game compatibility updates" || true
  fi

  log "Post-provision setup complete"
}

main "$@"
