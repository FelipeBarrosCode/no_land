#!/usr/bin/env bash
set -euo pipefail

# Why this script exists:
# - Configure low-latency PipeWire + WirePlumber drop-ins for Sunshine streaming.
# - Keep changes idempotent and isolated to dedicated fragment files.
# - Provide clear verification output and fallback latency profiles for VMs.

TARGET_USER="user"
PROFILE="aggressive"
FORCE_SINK_OVERRIDE=0
SINK_OVERRIDE=""
CANONICAL_SINK="sunshine_audio"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target-user)
      TARGET_USER="$2"
      shift 2
      ;;
    --profile)
      PROFILE="$2"
      shift 2
      ;;
    --force-sink-override)
      FORCE_SINK_OVERRIDE=1
      shift
      ;;
    --sink-override)
      SINK_OVERRIDE="$2"
      shift 2
      ;;
    *)
      echo "[lowlatency-audio] Unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

log() {
  echo "[lowlatency-audio] $*"
}

if ! id "$TARGET_USER" >/dev/null 2>&1; then
  echo "[lowlatency-audio] target user '$TARGET_USER' does not exist" >&2
  exit 2
fi

uid="$(id -u "$TARGET_USER")"
gid="$(id -g "$TARGET_USER")"
group_name="$(id -gn "$TARGET_USER" 2>/dev/null || true)"
if [[ -z "$group_name" ]]; then
  group_name="$gid"
fi

if [[ -z "$group_name" ]]; then
  echo "[lowlatency-audio] could not resolve primary group for '$TARGET_USER'" >&2
  exit 6
fi

log "Resolved account: user=${TARGET_USER} uid=${uid} group=${group_name} gid=${gid}"

user_home="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
if [[ -z "$user_home" ]]; then
  user_home="/home/$TARGET_USER"
fi

runtime_dir="/run/user/${uid}"
bus_path="${runtime_dir}/bus"

if [[ ! -d "$runtime_dir" || ! -S "$bus_path" ]]; then
  echo "[lowlatency-audio] user bus unavailable for ${TARGET_USER}." >&2
  echo "[lowlatency-audio] A user session must exist before restarting user services." >&2
  echo "[lowlatency-audio] expected XDG_RUNTIME_DIR=${runtime_dir}" >&2
  echo "[lowlatency-audio] expected DBUS_SESSION_BUS_ADDRESS=unix:path=${bus_path}" >&2
  exit 3
fi

run_user() {
  sudo -u "$TARGET_USER" env \
    XDG_RUNTIME_DIR="$runtime_dir" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=${bus_path}" \
    "$@"
}

case "$PROFILE" in
  aggressive)
    pulse_req="256/48000"
    ;;
  fallback1)
    pulse_req="512/48000"
    ;;
  fallback2)
    pulse_req="1024/48000"
    ;;
  *)
    echo "[lowlatency-audio] invalid profile '$PROFILE'. Use aggressive|fallback1|fallback2" >&2
    exit 4
    ;;
esac

log "Installing required packages"

# Check if packages are already installed
AUDIO_PACKAGES="pipewire pipewire-pulse wireplumber rtkit"
missing_packages=""
for pkg in $AUDIO_PACKAGES; do
    if ! dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed"; then
        missing_packages="$missing_packages $pkg"
    fi
done

if [[ -z "$missing_packages" ]]; then
    log "All audio packages already installed, skipping apt-get"
else
    log "Missing packages:$missing_packages"
    
    # Wait for dpkg lock (max 600 seconds for unattended-upgrades)
    log "Waiting for package manager lock..."
    lock_wait=600
    while sudo fuser /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock >/dev/null 2>&1; do
        sleep 1
        lock_wait=$((lock_wait - 1))
        if [[ $lock_wait -le 0 ]]; then
            echo "[lowlatency-audio] ERROR: Package manager lock timeout after 600 seconds" >&2
            echo "[lowlatency-audio] Try: sudo systemctl stop unattended-upgrades && sudo dpkg --configure -a" >&2
            exit 7
        fi
        if [[ $((600 - lock_wait)) -eq 30 ]] || [[ $((600 - lock_wait)) -eq 60 ]] || [[ $((600 - lock_wait)) -eq 120 ]] || [[ $((600 - lock_wait)) -eq 300 ]]; then
            log "Still waiting for package manager lock... ($((600 - lock_wait))s elapsed)"
        fi
    done
    log "Package manager lock released"
    
    sudo apt-get -o DPkg::Lock::Timeout=600 update
    sudo apt-get -o DPkg::Lock::Timeout=600 install -y $AUDIO_PACKAGES
fi

log "Ensuring low-latency config fragment directories"
sudo -u "$TARGET_USER" mkdir -p "$user_home/.config/pipewire/pipewire.conf.d"
sudo -u "$TARGET_USER" mkdir -p "$user_home/.config/pipewire/pipewire-pulse.conf.d"
sudo -u "$TARGET_USER" mkdir -p "$user_home/.config/wireplumber/wireplumber.conf.d"

log "Writing PipeWire rate/quantum fragment"
# This drop-in locks PipeWire to 48k and tight graph quantum for streaming.
cat <<'EOF' | sudo -u "$TARGET_USER" tee "$user_home/.config/pipewire/pipewire.conf.d/99-lowlatency.conf" >/dev/null
context.properties = {
    default.clock.rate          = 48000
    default.clock.allowed-rates = [ 48000 ]
    default.clock.quantum       = 256
    default.clock.min-quantum   = 128
    default.clock.max-quantum   = 256
    default.clock.quantum-limit = 256
}
EOF

log "Writing pipewire-pulse low-latency fragment (${PROFILE})"
# This drop-in sets pulse request/quantum for low-latency client compatibility.
cat <<EOF | sudo -u "$TARGET_USER" tee "$user_home/.config/pipewire/pipewire-pulse.conf.d/10-lowlatency.conf" >/dev/null
pulse.properties = {
    pulse.min.req     = ${pulse_req}
    pulse.default.req = ${pulse_req}
    pulse.min.quantum = ${pulse_req}
}
EOF

log "Writing WirePlumber ALSA low-latency fragment"
# This drop-in applies ALSA period tuning and fixed 48k rate at the policy layer.
cat <<'EOF' | sudo -u "$TARGET_USER" tee "$user_home/.config/wireplumber/wireplumber.conf.d/10-alsa-lowlatency.conf" >/dev/null
monitor.alsa.rules = [
  {
    matches = [
      { device.name = "~alsa_card.*" }
    ]
    actions = {
      update-props = {
        api.alsa.period-size = 128
        api.alsa.period-num  = 2
        audio.rate           = 48000
      }
    }
  }
]
EOF

log "Setting ownership for user config fragments"
sudo chown -R "$TARGET_USER":"$group_name" "$user_home/.config/pipewire" "$user_home/.config/wireplumber"

log "Ensuring realtime limits for audio group"
# Dedicated limits fragment so we do not touch unrelated PAM limits files.
cat <<'EOF' | sudo tee /etc/security/limits.d/audio.conf >/dev/null
@audio   -  rtprio   95
@audio   -  memlock  unlimited
EOF

if ! getent group audio >/dev/null; then
  log "Creating missing audio group"
  sudo groupadd audio
fi

log "Ensuring ${TARGET_USER} is in audio group"
sudo usermod -aG audio "$TARGET_USER"

if command -v cpupower >/dev/null 2>&1; then
  log "Setting CPU governor to performance"
  sudo cpupower frequency-set -g performance >/dev/null || true
else
  log "cpupower not available; skipping governor tuning"
fi

log "Restarting target user PipeWire services"
if ! run_user systemctl --user daemon-reload >/dev/null 2>&1; then
  echo "[lowlatency-audio] failed to access user systemd session for ${TARGET_USER}" >&2
  echo "[lowlatency-audio] ensure a user login/session exists before running this step" >&2
  exit 5
fi

run_user systemctl --user restart pipewire
run_user systemctl --user restart pipewire-pulse
run_user systemctl --user restart wireplumber

sunshine_conf="${user_home}/.config/sunshine/sunshine.conf"
target_sink="${CANONICAL_SINK}"
target_monitor="${CANONICAL_SINK}.monitor"

# Backward-compatible override path, but default is always canonical sink.
if [[ "$FORCE_SINK_OVERRIDE" -eq 1 && -n "$SINK_OVERRIDE" ]]; then
  target_sink="$SINK_OVERRIDE"
  target_monitor="${SINK_OVERRIDE}.monitor"
fi

log "Ensuring canonical null sink exists: ${target_sink}"
if ! run_user pactl list short sinks | awk '{print $2}' | grep -qx "$target_sink"; then
  run_user pactl load-module module-null-sink \
    sink_name="$target_sink" \
    sink_properties="device.description=Noland Audio" \
    rate=48000 channels=2 >/dev/null
fi

log "Setting default sink/source: sink=${target_sink} source=${target_monitor}"
run_user pactl set-default-sink "$target_sink" || true
run_user pactl set-default-source "$target_monitor" || true

if [[ -f "$sunshine_conf" ]]; then
  log "Applying Sunshine audio sink: ${target_sink}"
  if grep -Eq '^[[:space:]]*audio_sink[[:space:]]*=' "$sunshine_conf"; then
    run_user sed -i -E "s|^[[:space:]]*audio_sink[[:space:]]*=.*$|audio_sink = ${target_sink}|" "$sunshine_conf"
  else
    printf '\naudio_sink = %s\n' "$target_sink" | run_user tee -a "$sunshine_conf" >/dev/null
  fi
  run_user systemctl --user restart sunshine >/dev/null 2>&1 || true
fi

log "===== Verification ====="
if run_user pw-top -b -n 1 >/tmp/noland-pw-top.txt 2>/tmp/noland-pw-top.err; then
  echo "-- pw-top (single snapshot) --"
  cat /tmp/noland-pw-top.txt
else
  echo "-- pw-top unavailable; trying pw-metadata --"
  run_user pw-metadata -n settings 0 2>/dev/null || true
fi

echo "-- pactl info --"
run_user pactl info || true

echo "-- pactl list short sinks --"
run_user pactl list short sinks || true

echo "-- pactl list short sources --"
run_user pactl list short sources || true

echo "-- user groups --"
id "$TARGET_USER"

echo "-- rtprio guidance --"
echo "Run as ${TARGET_USER}: ulimit -r (should reflect realtime limits after new login/session)."

if command -v cpupower >/dev/null 2>&1; then
  echo "-- CPU governor --"
  cpupower frequency-info 2>/dev/null | sed -n '1,20p' || true
else
  echo "-- CPU governor --"
  if [[ -r /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]]; then
    cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor
  else
    echo "Unavailable"
  fi
fi

echo "[lowlatency-audio] profile=${PROFILE} target_user=${TARGET_USER} force_sink_override=${FORCE_SINK_OVERRIDE}"
