#!/usr/bin/env bash
# Disposable Ubuntu-22.04 CI ONLY. Does not install a modern userspace or need a GPU.
set -euo pipefail
agent="$PWD/runtime-agent/target/release/noland-runtime"
work=$(mktemp -d)
trap 'if [ -n "${xvfb_pid:-}" ]; then kill "$xvfb_pid" 2>/dev/null || true; fi; rm -rf "$work"' EXIT
mkdir -p "$work/home"
touch "$work/Xauthority"
xauth -f "$work/Xauthority" add :0 . "$(mcookie)"
sudo install -d -m 755 /usr/local/libexec /etc/X11
sudo install -m 600 "$work/Xauthority" /etc/X11/.Xauthority-noland
cat > "$work/helper" <<'HELPER'
#!/bin/sh
set -eu
[ "$#" = 0 ] || exit 64
exec /usr/bin/xauth -f /etc/X11/.Xauthority-noland extract - :0
HELPER
sudo install -m 755 "$work/helper" /usr/local/libexec/noland-runtime-session
Xvfb :0 -auth "$work/Xauthority" -screen 0 1024x768x24 > "$work/xvfb.log" 2>&1 &
xvfb_pid=$!
for _ in $(seq 1 50); do [ -S /tmp/.X11-unix/X0 ] && break; sleep .1; done
# No runtime or release.json exists. Opening the GTK drop window must still work.
set +e
HOME="$work/home" timeout 5 "$agent" gui > "$work/gui.log" 2>&1
status=$?
set -e
cat "$work/gui.log"
test "$status" = 124
test ! -f "$work/home/.local/share/noland-runtime/activation.json"
