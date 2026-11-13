#!/usr/bin/env bash
# Runs only on the cloud PC as part of provisioning. Applications never run here.
set -euo pipefail
user=${1:?Desktop user required}
source_sha=${2:?Bundled source digest required}
[[ "$user" =~ ^[a-zA-Z0-9_-]+$ && "$source_sha" =~ ^[a-f0-9]{64}$ ]]
[[ $(id -u) = 0 && $(id -u "$user") != 0 ]]
[[ $(uname -m) = x86_64 ]] || { echo 'The runtime currently supports x86-64 cloud PCs only.' >&2; exit 1; }
source_root=$(cd "$(dirname "$0")/../.." && pwd)
export DEBIAN_FRONTEND=noninteractive
export PATH="/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

install -d -m 755 /opt/noland/runtime /opt/noland/runtime/images /var/cache/noland-runtime /etc/noland-runtime /usr/local/libexec
for directory in /opt/noland/runtime /opt/noland/runtime/images /var/cache/noland-runtime /etc/noland-runtime; do
  [[ ! -L "$directory" ]] || { echo "Runtime installation directory must not be a symlink: $directory" >&2; exit 1; }
done
exec 9>/var/lock/noland-runtime-install.lock
flock -w 1800 9 || { echo 'Another cloud-runtime installation is still active.' >&2; exit 1; }

# Only parent-side launcher/build prerequisites. Modern app libraries stay in
# the separate image; never replace the host libc, NVIDIA driver or Sunshine.
apt-get -o DPkg::Lock::Timeout=300 update
apt-get -o DPkg::Lock::Timeout=300 install -y --no-install-recommends \
  ca-certificates curl python3 python3-gi gir1.2-gtk-3.0 bubblewrap xauth \
  sudo xdg-user-dirs libglib2.0-bin build-essential pkg-config xdotool wmctrl x11-utils

work=$(mktemp -d /opt/noland/runtime/.install-XXXXXXXX)
image_stage=''
cleanup() {
  rm -rf -- "$work"
  if [[ -n "$image_stage" ]]; then rm -rf -- "$image_stage"; fi
}
trap cleanup EXIT

if [[ ! -x /usr/local/bin/noland-runtime ]] || [[ $(cat /opt/noland/runtime/companion-source.sha256 2>/dev/null || true) != "$source_sha" ]]; then
  if ! command -v cargo >/dev/null; then
    curl --fail --location --proto '=https' --proto-redir '=https' --connect-timeout 30 --max-time 180 \
      https://sh.rustup.rs -o "$work/rustup.sh"
    sh "$work/rustup.sh" -y --profile minimal --default-toolchain stable --no-modify-path
  fi
  cargo build --locked --release --manifest-path "$source_root/runtime-agent/Cargo.toml"
  install -m 755 "$source_root/runtime-agent/target/release/noland-runtime" "$work/noland-runtime"
  "$work/noland-runtime" --version
  install -m 755 "$work/noland-runtime" /usr/local/bin/noland-runtime.new
  mv -f /usr/local/bin/noland-runtime.new /usr/local/bin/noland-runtime
  printf '%s\n' "$source_sha" > /opt/noland/runtime/companion-source.sha256
fi

# Fixed-function X11 credential helper; no root application-execution endpoint.
cat > "$work/session" <<'HELPER'
#!/bin/sh
set -eu
[ "$#" = 0 ] || exit 64
exec /usr/bin/xauth -f /etc/X11/.Xauthority-noland extract - :0
HELPER
printf '%s ALL=(root) NOPASSWD: /usr/local/libexec/noland-runtime-session ""\n' "$user" > "$work/sudoers"
printf '%s ALL=(root) NOPASSWD: /usr/local/libexec/noland-runtime-maintain *\n' "$user" >> "$work/sudoers"
chmod 440 "$work/sudoers"
visudo -cf "$work/sudoers"
install -o root -g root -m 755 "$work/session" /usr/local/libexec/noland-runtime-session
install -o root -g root -m 755 "$source_root/scripts/runtime/maintain-runtime.py" /usr/local/libexec/noland-runtime-maintain
install -o root -g root -m 644 "$source_root/runtime-images/ubuntu24/prerequisites.json" /etc/noland-runtime/prerequisites.json
install -o root -g root -m 440 "$work/sudoers" /etc/sudoers.d/noland-runtime

# Official Ubuntu base, pinned to the digest published in Canonical's
# https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/SHA256SUMS.
# No No Land signing key, manifest server, or separate release is needed.
base_file=ubuntu-base-24.04.3-base-amd64.tar.gz
base_sha=6bc2cde3930ad088b3bb46fa45279e96d25bc3810f209850ecbe4722711874f9
image_sha=$({ printf '%s' "$base_sha"; cat "$source_root/runtime-images/ubuntu24/packages.txt" "$source_root/runtime-images/ubuntu24/glibc-probe.c"; printf 'cloud-image-v1'; } | sha256sum | cut -d' ' -f1)
image_id="ubuntu24-${image_sha:0:16}"
image="/opt/noland/runtime/images/$image_id"

# Install the icon even if the subsequent download fails, so the companion can
# explain the problem instead of making the entire cloud desktop inaccessible.
sudo -n -H -u "$user" /usr/local/bin/noland-runtime create-shortcut

if [[ $(cat "$image/etc/noland-runtime-complete" 2>/dev/null || true) != "$image_sha" ]]; then
  [[ ! -e "$image" ]] || { echo "Incomplete image at $image; it was not overwritten." >&2; exit 1; }
  free=$(df -PB1 /opt/noland/runtime | awk 'NR==2 {print $4}')
  [[ "$free" -ge 4294967296 ]] || { echo 'Runtime setup needs at least 4 GiB of free cloud-PC disk space.' >&2; exit 1; }
  archive="/var/cache/noland-runtime/$base_file"
  if ! printf '%s  %s\n' "$base_sha" "$archive" | sha256sum --check --status 2>/dev/null; then
    partial="$archive.partial"
    for attempt in 1 2 3 4; do
      status=0
      curl --fail --location --proto '=https' --proto-redir '=https' --connect-timeout 30 --max-time 900 \
        --speed-limit 1024 --speed-time 60 --continue-at - \
        "https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/$base_file" -o "$partial" || status=$?
      if [[ "$status" = 0 ]]; then break; fi
      if [[ "$status" = 33 || "$status" = 36 ]]; then rm -f -- "$partial"; fi
      [[ "$attempt" != 4 ]] || { echo 'Ubuntu runtime download failed; resume Provisioning to retry.' >&2; exit 1; }
      sleep "$((attempt * 2))"
    done
    if ! printf '%s  %s\n' "$base_sha" "$partial" | sha256sum --check --status; then
      rm -f -- "$partial"
      echo 'Ubuntu base checksum mismatch; corrupt download removed.' >&2
      exit 1
    fi
    mv -f -- "$partial" "$archive"
  fi
  image_stage=$(mktemp -d /opt/noland/runtime/images/.staging-XXXXXXXX)
  tar --extract --gzip --no-same-owner --file "$archive" --directory "$image_stage"
  install -d -m 755 "$image_stage/etc/ssl/certs" "$image_stage/dev" "$image_stage/tmp" "$image_stage/usr/local/bin"
  chmod 1777 "$image_stage/tmp"
  # No host /dev or /proc mounts, service restarts, or container runtime needed.
  for spec in 'null 1 3' 'zero 1 5' 'random 1 8' 'urandom 1 9'; do
    read -r name major minor <<< "$spec"
    if [[ ! -e "$image_stage/dev/$name" ]]; then mknod -m 666 "$image_stage/dev/$name" c "$major" "$minor"; fi
  done
  rm -f -- "$image_stage/etc/resolv.conf"
  cp -L /etc/resolv.conf "$image_stage/etc/resolv.conf"
  cp /etc/ssl/certs/ca-certificates.crt "$image_stage/etc/ssl/certs/ca-certificates.crt"
  cat > "$image_stage/etc/apt/sources.list.d/ubuntu.sources" <<'SOURCES'
Types: deb
URIs: https://archive.ubuntu.com/ubuntu
Suites: noble noble-updates
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: https://security.ubuntu.com/ubuntu
Suites: noble-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
SOURCES
  printf '#!/bin/sh\nexit 101\n' > "$image_stage/usr/sbin/policy-rc.d"
  chmod 755 "$image_stage/usr/sbin/policy-rc.d"
  chroot "$image_stage" /usr/bin/env DEBIAN_FRONTEND=noninteractive apt-get update
  mapfile -t packages < <(grep -vE '^\s*(#|$)' "$source_root/runtime-images/ubuntu24/packages.txt")
  chroot "$image_stage" /usr/bin/env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${packages[@]}" gcc libc6-dev
  cp "$source_root/runtime-images/ubuntu24/glibc-probe.c" "$image_stage/tmp/noland-glibc-probe.c"
  chroot "$image_stage" gcc -O0 -fno-builtin -Wall -Werror /tmp/noland-glibc-probe.c -o /usr/local/bin/noland-glibc-probe
  chroot "$image_stage" /usr/local/bin/noland-glibc-probe
  chroot "$image_stage" apt-get clean
  rm -f -- "$image_stage/tmp/noland-glibc-probe.c"
  # Bubblewrap cannot overlay a file on Ubuntu Base's absolute localtime
  # symlink after the image is mounted read-only. Materialize these small host
  # configuration mount targets; do not change the parent's configuration.
  for name in hosts resolv.conf localtime; do
    if [[ -f "/etc/$name" ]]; then
      install -m 644 "/etc/$name" "$image_stage/etc/$name.noland-new"
      mv -f -- "$image_stage/etc/$name.noland-new" "$image_stage/etc/$name"
    fi
  done
  mkdir -p "$image_stage/app" "$image_stage/home/runtime" "$image_stage/opt/noland-driver" "$image_stage/run/noland" "$image_stage/tmp/.X11-unix" "$image_stage/dev/shm"
  printf '%s\n' "$image_sha" > "$image_stage/etc/noland-runtime-complete"
  chmod 755 "$image_stage"
  mv -- "$image_stage" "$image"
  image_stage=''
fi

printf '{"id":"%s","profile":"ubuntu24-x86_64","baseSha256":"%s"}\n' "$image_id" "$base_sha" > "$work/image.json"
install -o root -g root -m 644 "$work/image.json" /etc/noland-runtime/image.json.new
mv -f /etc/noland-runtime/image.json.new /etc/noland-runtime/image.json
sudo -n -H -u "$user" /usr/local/bin/noland-runtime status --json
echo 'No Land Runtime is installed. Open its desktop icon and drop a trusted Linux app.'
