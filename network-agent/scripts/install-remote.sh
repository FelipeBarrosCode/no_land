#!/usr/bin/env bash
set -euo pipefail

SOURCE_ARCHIVE="${1:-/tmp/noland-network-agent-src.tgz}"
BUILD_ROOT="${2:-/tmp/noland-network-agent-build}"
SOURCE_DIR="${BUILD_ROOT}/network-agent"
INSTALL_REVISION="${3:?network-agent install revision is required}"
BINARY_PATH="/usr/local/bin/noland-network-agent"
WRAPPER_PATH="/usr/local/libexec/noland-network-agent-bind-wg0"
UNIT_PATH="/etc/systemd/system/noland-network-agent.service"
REVISION_PATH="/usr/local/share/noland-network-agent/install-revision"
BUILD_LOG="${BUILD_ROOT}/build.log"

cleanup() {
    rm -rf "$BUILD_ROOT"
    rm -f "$SOURCE_ARCHIVE" "$BUILD_LOG"
}
trap cleanup EXIT

if [[ "$(id -u)" -ne 0 ]]; then
    echo "noland-network-agent installer must run as root" >&2
    exit 1
fi
if ! command -v systemctl >/dev/null 2>&1; then
    echo "systemd is required to install noland-network-agent" >&2
    exit 1
fi
if [[ ! -f "$SOURCE_ARCHIVE" ]]; then
    echo "network-agent source archive not found at $SOURCE_ARCHIVE" >&2
    exit 1
fi

export HOME=/root
export RUSTUP_HOME=/root/.rustup
export CARGO_HOME=/root/.cargo
export PATH="/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

if ! command -v cargo >/dev/null 2>&1; then
    if ! command -v curl >/dev/null 2>&1; then
        echo "cargo is unavailable and curl is required for the minimal rustup bootstrap" >&2
        exit 1
    fi
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable
fi

rm -rf "$BUILD_ROOT"
install -d -m 0755 "$BUILD_ROOT"
tar -xzf "$SOURCE_ARCHIVE" -C "$BUILD_ROOT"
if [[ ! -f "$SOURCE_DIR/Cargo.toml" ]]; then
    echo "source archive does not contain network-agent/Cargo.toml" >&2
    exit 1
fi

cd "$SOURCE_DIR"
if [[ -f Cargo.lock ]]; then
    build_args=(--release --locked)
else
    build_args=(--release)
fi
if ! cargo build "${build_args[@]}" >"$BUILD_LOG" 2>&1; then
    echo "noland-network-agent release build failed; last build output:" >&2
    tail -n 120 "$BUILD_LOG" >&2 || true
    exit 1
fi
if [[ ! -x target/release/noland-network-agent ]]; then
    echo "release build did not produce target/release/noland-network-agent" >&2
    exit 1
fi

install -o root -g root -m 0755 target/release/noland-network-agent "$BINARY_PATH"
install -d -o root -g root -m 0755 /usr/local/libexec
cat >"$WRAPPER_PATH" <<'NOLAND_NETWORK_AGENT_WRAPPER'
__NOLAND_NETWORK_AGENT_WRAPPER__
NOLAND_NETWORK_AGENT_WRAPPER
chown root:root "$WRAPPER_PATH"
chmod 0755 "$WRAPPER_PATH"

cat >"$UNIT_PATH" <<'NOLAND_NETWORK_AGENT_UNIT'
__NOLAND_NETWORK_AGENT_UNIT__
NOLAND_NETWORK_AGENT_UNIT
chown root:root "$UNIT_PATH"
chmod 0644 "$UNIT_PATH"
install -d -o root -g root -m 0755 "$(dirname "$REVISION_PATH")"
printf '%s\n' "$INSTALL_REVISION" >"$REVISION_PATH"
chown root:root "$REVISION_PATH"
chmod 0644 "$REVISION_PATH"

systemctl daemon-reload
systemctl enable --now noland-network-agent.service
systemctl restart noland-network-agent.service
if ! systemctl is-active --quiet noland-network-agent.service; then
    systemctl --no-pager --full status noland-network-agent.service >&2 || true
    exit 1
fi

printf '%s\n' "NOLAND_NETWORK_AGENT_READY"
