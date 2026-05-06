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
ENABLE_GITHUB_GAME_COMPAT="${ENABLE_GITHUB_GAME_COMPAT:-0}"
ENABLE_OPTIONAL_GAMING_STACK="${NOLAND_ENABLE_OPTIONAL_GAMING_STACK:-0}"
OS_ID=""
OS_VERSION=""
OS_CODENAME=""

log() {
  printf '[post-provision] %s\n' "$*"
}

phase() {
  printf '[post-provision][phase] %s\n' "$*"
}

run_user() {
  runuser -u "$TARGET_USER" -- bash -lc "$*"
}

run_root() {
  bash -lc "$*"
}

ensure_clean_wine_prefix() {
  run_user "mkdir -p '${WINE_PREFIX}'"

  if grep -q '/root' "${WINE_PREFIX}/user.reg" 2>/dev/null || grep -q '/root' "${WINE_PREFIX}/system.reg" 2>/dev/null; then
    log "Detected /root references in Wine prefix; rebuilding ${WINE_PREFIX}"
    run_user "rm -rf '${WINE_PREFIX}' && mkdir -p '${WINE_PREFIX}'"
  fi

  run_user "HOME='${USER_HOME}' WINEPREFIX='${WINE_PREFIX}' wineboot --init >/dev/null 2>&1 || true"
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
  if run_root "apt-get install -y '${package_name}'"; then
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
  run_root "DEBIAN_FRONTEND=noninteractive apt-get update -y"
  run_root "DEBIAN_FRONTEND=noninteractive apt-get install -y curl wget ca-certificates gnupg software-properties-common xdg-utils unzip python3 tar xz-utils cabextract p7zip-full flatpak zstd"

  install_first_available_package libfuse2 libfuse2t64 || true
  install_first_available_package fuse3 fuse || true
}

install_bottles() {
  log "Installing Bottles via Flatpak"
  run_user "flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo"
  run_user "flatpak install -y --user flathub '${BOTTLES_APP_ID}'"
}

configure_bottles_access() {
  log "Configuring Bottles filesystem access"
  run_root "mkdir -p '${BOTTLES_GAMES_DIR}/epic' '${BOTTLES_GAMES_DIR}/ea' '${BOTTLES_GAMES_DIR}/ubisoft' '${BOTTLES_GAMES_DIR}/battlenet' '${BOTTLES_GAMES_DIR}/gog'"
  run_root "chown -R '${TARGET_USER}:${TARGET_USER}' '${BOTTLES_GAMES_DIR}'"
  run_root "chmod -R 775 '${BOTTLES_GAMES_DIR}'"
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

BCLI='flatpak run --user --command=bottles-cli com.usebottles.bottles'
DIR='\$HOME/Downloads/game-launchers'

\$BCLI run -b epic -e \"\$DIR/EpicGamesLauncherInstaller.msi\"
\$BCLI run -b ea -e \"\$DIR/EAappInstaller.exe\"
\$BCLI run -b ubisoft -e \"\$DIR/UbisoftConnectInstaller.exe\"
\$BCLI run -b battlenet -e \"\$DIR/BattleNet-Setup.exe\"
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
  create_bottle_if_missing "gog"

  write_launcher_install_script
  log "Installers prepared. In desktop session run: bash ~/run-launcher-installers.sh"
}

install_chrome() {
  log "Installing Google Chrome"
  local deb_path="/tmp/google-chrome-stable_current_amd64.deb"
  wget -q "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb" -O "$deb_path"
  run_root "dpkg -i '${deb_path}'" || run_root "apt-get install -f -y"
  rm -f "$deb_path"
  run_user "xdg-settings set default-web-browser google-chrome.desktop || true"
}

install_wine() {
  log "Installing latest Wine stable"
  run_root "dpkg --add-architecture i386"
  run_root "mkdir -p /etc/apt/keyrings"
  run_root "wget -qO /etc/apt/keyrings/winehq-archive.key https://dl.winehq.org/wine-builds/winehq.key"

  local codename
  codename="${OS_CODENAME:-}"
  if [[ -z "$codename" ]]; then
    codename="jammy"
  fi

  if run_root "wget -qO '/etc/apt/sources.list.d/winehq-${codename}.sources' 'https://dl.winehq.org/wine-builds/ubuntu/dists/${codename}/winehq-${codename}.sources'"; then
    run_root "apt-get update -y"
    run_root "apt-get install -y --install-recommends winehq-stable" || run_root "apt-get install -y wine-stable"
  else
    log "WineHQ source unavailable for ${codename}; falling back to distro wine"
    run_root "apt-get update -y"
    run_root "apt-get install -y wine-stable" || true
  fi

  run_root "apt-get install -y winetricks" || true
}

fetch_latest_asset_url() {
  local repo="$1"
  local regex="$2"
  local api="https://api.github.com/repos/${repo}/releases/latest"
  local payload
  payload="$(curl --retry 3 --retry-delay 2 --retry-all-errors -fsSL \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    -H 'User-Agent: noland-post-provision' \
    "$api" 2>/tmp/noland-github-api.err || true)"

  if [[ -z "$payload" ]]; then
    log "GitHub API request failed for ${repo}"
    if [[ -s /tmp/noland-github-api.err ]]; then
      log "GitHub API error: $(tr '\n' ' ' < /tmp/noland-github-api.err)"
    fi
    return 1
  fi

  printf '%s' "$payload" | awk -v pattern="$regex" '
    /"name"[[:space:]]*:/ {
      name=$0
      sub(/^.*"name"[[:space:]]*:[[:space:]]*"/, "", name)
      sub(/".*/, "", name)
    }
    /"browser_download_url"[[:space:]]*:/ {
      url=$0
      sub(/^.*"browser_download_url"[[:space:]]*:[[:space:]]*"/, "", url)
      sub(/".*/, "", url)
      if (name ~ pattern) {
        print url
        exit
      }
    }
  '
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
  ensure_clean_wine_prefix
  run_user "WINEPREFIX='${WINE_PREFIX}' wineboot --init >/dev/null 2>&1 || true"
  run_user "WINEPREFIX='${WINE_PREFIX}' winetricks -q vcrun2022 d3dcompiler_47 directx9 >/tmp/noland-winetricks.log 2>&1 || true"
}

install_game_compat() {
  log "Installing/updating game compatibility layer"

  if [[ "$ENABLE_GITHUB_GAME_COMPAT" == "1" ]]; then
    install_proton_ge
    install_wine_ge
    install_dxvk
    install_vkd3d_proton
  else
    log "Skipping GitHub-based compatibility assets (set ENABLE_GITHUB_GAME_COMPAT=1 to enable)"
  fi

  install_winetricks_runtime
}

install_heroic_latest() {
  log "Installing latest Heroic AppImage"
  run_user "mkdir -p '${BIN_DIR}' '${APP_DIR}'"

  local heroic_url
  heroic_url="$(fetch_latest_asset_url "Heroic-Games-Launcher/HeroicGamesLauncher" '^(?!.*arm).*\.AppImage$' || true)"

  if [[ -z "$heroic_url" ]]; then
    log "Could not detect latest Heroic release URL"
    log "Skipping Heroic AppImage install (non-fatal)"
    return 0
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
  ensure_clean_wine_prefix
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

configure_desktop_favorites() {
  log "Configuring desktop favorites for Chrome, Steam, and Bottles"

  run_user "mkdir -p '${USER_HOME}/.local/bin' '${USER_HOME}/.config/autostart'"

  run_user "cat > '${USER_HOME}/.local/bin/noland-pin-favorites.sh' <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

required=(
  'google-chrome.desktop'
  'steam.desktop'
  'com.usebottles.bottles.desktop'
)

if ! command -v gsettings >/dev/null 2>&1; then
  exit 0
fi

updated_raw="['google-chrome.desktop', 'steam.desktop', 'com.usebottles.bottles.desktop']"

gsettings set org.gnome.shell favorite-apps "$updated_raw" || exit 0
rm -f "$HOME/.config/autostart/noland-pin-favorites.desktop"
EOF
chmod +x '${USER_HOME}/.local/bin/noland-pin-favorites.sh'"

  run_user "cat > '${USER_HOME}/.config/autostart/noland-pin-favorites.desktop' <<EOF
[Desktop Entry]
Type=Application
Name=Noland Pin Favorites
Comment=Pin Chrome, Steam, and Bottles
Exec=${USER_HOME}/.local/bin/noland-pin-favorites.sh
Terminal=false
X-GNOME-Autostart-enabled=true
NoDisplay=true
EOF
"
}

repair_user_permissions() {
  run_root "chown -R '${TARGET_USER}:${TARGET_USER}' '${USER_HOME}/.steam' '${USER_HOME}/.local' '${USER_HOME}/Desktop' '${WINE_PREFIX}'" 2>/dev/null || true

  # Repair common Lutris paths when preinstalled images create root-owned files.
  run_root "chown -R '${TARGET_USER}:${TARGET_USER}' '${USER_HOME}/.config/lutris' '${USER_HOME}/.local/share/lutris' '${USER_HOME}/.cache/lutris' '${USER_HOME}/Games' '/srv/games/ea'" 2>/dev/null || true
  run_root "find '${USER_HOME}/.config/lutris' '${USER_HOME}/.local/share/lutris' '${USER_HOME}/.cache/lutris' '${USER_HOME}/Games' '/srv/games/ea' -type d -exec chmod 775 {} +" 2>/dev/null || true
  run_root "find '${USER_HOME}/.config/lutris' '${USER_HOME}/.local/share/lutris' '${USER_HOME}/.cache/lutris' '${USER_HOME}/Games' '/srv/games/ea' -type f -exec chmod 664 {} +" 2>/dev/null || true
}

repair_wine_dosdevices_links() {
  log "Repairing malformed Wine dosdevices links"

  local scan_roots=()
  [[ -d "${USER_HOME}/Games" ]] && scan_roots+=("${USER_HOME}/Games")
  [[ -d "${USER_HOME}/.wine" ]] && scan_roots+=("${USER_HOME}/.wine")
  [[ -d "${WINE_PREFIX}" ]] && scan_roots+=("${WINE_PREFIX}")

  local root
  for root in "${scan_roots[@]}"; do
    while IFS= read -r dosdevices; do
      local prefix_dir
      prefix_dir="$(dirname "$dosdevices")"

      run_user "find '${dosdevices}' -maxdepth 1 -type l -name '*::*' -delete" || true

      if [[ -L "${dosdevices}/d:" ]]; then
        local d_target
        d_target="$(readlink "${dosdevices}/d:" 2>/dev/null || true)"
        if [[ "${d_target}" == /dev/* ]]; then
          log "Removing block-device D: mapping in ${dosdevices} -> ${d_target}"
          run_user "rm -f '${dosdevices}/d:'" || true
        fi
      fi

      run_user "if [[ -d '${prefix_dir}/drive_c' ]]; then ln -sfn ../drive_c '${dosdevices}/c:'; fi" || true
      run_user "ln -sfn / '${dosdevices}/z:'" || true
    done < <(find "$root" -type d -name dosdevices 2>/dev/null)
  done
}

main() {
  if [[ "${EUID}" -ne 0 ]]; then
    echo "This script must be run as root" >&2
    exit 1
  fi

  if ! id "$TARGET_USER" >/dev/null 2>&1; then
    echo "Target user '$TARGET_USER' not found" >&2
    exit 2
  fi

  phase "1/6 Detect OS and prepare user directories"
  detect_os
  run_user "mkdir -p '${USER_HOME}/Desktop'"

  phase "2/6 Install base system packages"
  ensure_packages

  phase "3/6 Install Chrome and Wine"
  install_chrome
  install_wine

  phase "4/6 Optional gaming stack"
  if [[ "$ENABLE_OPTIONAL_GAMING_STACK" == "1" ]]; then
    log "NOLAND_ENABLE_OPTIONAL_GAMING_STACK=1 -> running optional gaming setup"
    install_game_compat
    install_heroic_latest
    setup_bottles_launchers
    setup_shared_wine_prefix
    configure_desktop_favorites
  else
    log "Skipping optional gaming stack (set NOLAND_ENABLE_OPTIONAL_GAMING_STACK=1 to enable)"
  fi

  phase "5/6 Repair permissions and Wine links"
  repair_user_permissions
  repair_wine_dosdevices_links

  phase "6/6 Finalization"
  if [[ "$GAME_COMPAT_UPDATED" -eq 1 ]]; then
    log "Game compatibility layer updated; scheduling reboot in 1 minute"
    run_root "shutdown -r +1 'Noland: reboot after game compatibility updates'" || true
  fi

  log "Post-provision setup complete"
}

main "$@"
