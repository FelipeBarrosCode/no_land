#!/usr/bin/env bash
# Build, install, enable, and start noland-lifecycle-agent from the deployed workspace.
set -euo pipefail

SOURCE_LINK="/opt/noland/state-agent"
TARGET_USER="${1:?target user is required}"
EXPECTED_VERSION="${2:?expected version is required}"
EXPECTED_REVISION="${3:?deployment revision is required}"
STAGING_ROOT="${4:?root-private staging directory is required}"
REAL_BINARY="/usr/local/libexec/noland-lifecycle-agent.real"
PUBLIC_BINARY="/usr/local/bin/noland-lifecycle-agent"
UNIT_PATH="/etc/systemd/system/noland-lifecycle-agent.service"
REVISION_PATH="/usr/local/share/noland-lifecycle-agent/install-revision"
SOCKET_PATH="/run/noland/lifecycle/agent.sock"

if [[ "$(id -u)" != "0" ]]; then
  echo "lifecycle-agent installer must run as root" >&2
  exit 1
fi
if [[ ! "$TARGET_USER" =~ ^[a-z_][a-z0-9_-]*[$]?$ ]]; then
  echo "invalid lifecycle-agent target user" >&2
  exit 1
fi
if [[ ! "$EXPECTED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid lifecycle-agent expected version" >&2
  exit 1
fi
if [[ ! "$EXPECTED_REVISION" =~ ^[0-9]+$ ]]; then
  echo "invalid lifecycle-agent deployment revision" >&2
  exit 1
fi
if [[ ! -d "$STAGING_ROOT" || -L "$STAGING_ROOT" ]]; then
  echo "root-private lifecycle-agent staging directory is invalid" >&2
  exit 1
fi
if [[ ! -L "$SOURCE_LINK" || "$(stat -c '%u' "$SOURCE_LINK")" != "0" ]]; then
  echo "state-agent workspace link is missing or not root-owned" >&2
  exit 1
fi
SOURCE_ROOT="$(readlink -f "$SOURCE_LINK")"
if [[ ! "$SOURCE_ROOT" =~ ^/opt/noland/state-agent-[0-9]+$ ]]; then
  echo "state-agent workspace link resolves outside the managed deployment layout" >&2
  exit 1
fi
for trusted_path in /opt /opt/noland "$SOURCE_ROOT"; do
  if [[ "$(stat -c '%u' "$trusted_path")" != "0" ]]; then
    echo "state-agent workspace has a non-root-owned path component" >&2
    exit 1
  fi
  trusted_mode="$(stat -c '%a' "$trusted_path")"
  if (( (8#$trusted_mode & 0022) != 0 )); then
    echo "state-agent workspace has a group/world-writable path component" >&2
    exit 1
  fi
done
if [[ -n "$(find "$SOURCE_ROOT" -xdev \( ! -user root -o -perm /022 \) -print -quit)" ]]; then
  echo "state-agent workspace contains untrusted ownership or writable content" >&2
  exit 1
fi
if [[ ! -f "$SOURCE_ROOT/Cargo.toml" || ! -f "$SOURCE_ROOT/Cargo.lock" ]]; then
  echo "state-agent workspace is missing from $SOURCE_ROOT" >&2
  exit 1
fi
if [[ ! -f "$SOURCE_ROOT/crates/noland-lifecycle-agent/Cargo.toml" ]]; then
  echo "noland-lifecycle-agent workspace package is missing" >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to install the lifecycle agent" >&2
  exit 1
fi
if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemd is required to run the lifecycle agent" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to verify lifecycle-agent readiness" >&2
  exit 1
fi
if ! id "$TARGET_USER" >/dev/null 2>&1; then
  echo "lifecycle-agent target user does not exist" >&2
  exit 1
fi

TARGET_GROUP="$(id -gn "$TARGET_USER")"
if [[ ! "$TARGET_GROUP" =~ ^[a-z_][a-z0-9_-]*[$]?$ ]]; then
  echo "invalid lifecycle-agent target group" >&2
  exit 1
fi

if ! command -v xprop >/dev/null 2>&1; then
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "xprop is required and x11-utils can only be installed automatically with apt" >&2
    exit 1
  fi
  export DEBIAN_FRONTEND=noninteractive
  export NEEDRESTART_MODE=l
  export NEEDRESTART_SUSPEND=1
  apt-get -o DPkg::Lock::Timeout=600 update -qq
  apt-get -o DPkg::Lock::Timeout=600 install -y -qq --no-install-recommends x11-utils
fi
command -v xprop >/dev/null 2>&1

export HOME=/root
export CARGO_HOME=/root/.cargo
export RUSTUP_HOME=/root/.rustup
export PATH="$CARGO_HOME/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal \
      >"$STAGING_ROOT/rustup.log" 2>&1
fi

cd "$SOURCE_ROOT"
if ! cargo build --release -p noland-lifecycle-agent --locked \
  >"$STAGING_ROOT/build.log" 2>&1; then
  echo "noland-lifecycle-agent build failed" >&2
  tail -n 120 "$STAGING_ROOT/build.log" >&2 || true
  exit 1
fi
if [[ ! -x "$SOURCE_ROOT/target/release/noland-lifecycle-agent" ]]; then
  echo "lifecycle-agent build completed without an executable" >&2
  exit 1
fi

install -d -o root -g root -m 0755 /usr/local/libexec /usr/local/bin
real_temp="$(mktemp /usr/local/libexec/.noland-lifecycle-agent.real.XXXXXX)"
wrapper_temp="$(mktemp /usr/local/bin/.noland-lifecycle-agent.XXXXXX)"
unit_temp=""
revision_temp=""
cleanup_destination_temps() {
  rm -f "$real_temp" "$wrapper_temp"
  if [[ -n "$unit_temp" ]]; then rm -f "$unit_temp"; fi
  if [[ -n "$revision_temp" ]]; then rm -f "$revision_temp"; fi
}
trap cleanup_destination_temps EXIT

install -o root -g root -m 0755 \
  "$SOURCE_ROOT/target/release/noland-lifecycle-agent" "$real_temp"
mv -f "$real_temp" "$REAL_BINARY"

cat >"$wrapper_temp" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ "\${1:-}" == "--version" ]]; then
  printf '%s\n' 'noland-lifecycle-agent $EXPECTED_VERSION'
  exit 0
fi
exec /usr/local/libexec/noland-lifecycle-agent.real "\$@"
EOF
chown root:root "$wrapper_temp"
chmod 0755 "$wrapper_temp"
mv -f "$wrapper_temp" "$PUBLIC_BINARY"

UNIT_TEMPLATE="$STAGING_ROOT/noland-lifecycle-agent.service.template"
cat >"$UNIT_TEMPLATE" <<'__NOLAND_LIFECYCLE_UNIT_EOF__'
__NOLAND_LIFECYCLE_UNIT__
__NOLAND_LIFECYCLE_UNIT_EOF__

MATERIALIZED_UNIT="$STAGING_ROOT/noland-lifecycle-agent.service"
sed \
  -e "s/__NOLAND_TARGET_USER__/$TARGET_USER/g" \
  -e "s/__NOLAND_TARGET_GROUP__/$TARGET_GROUP/g" \
  "$UNIT_TEMPLATE" >"$MATERIALIZED_UNIT"
if grep -q '__NOLAND_' "$MATERIALIZED_UNIT"; then
  echo "lifecycle-agent systemd unit contains unresolved placeholders" >&2
  exit 1
fi

unit_temp="$(mktemp /etc/systemd/system/.noland-lifecycle-agent.service.XXXXXX)"
install -o root -g root -m 0644 "$MATERIALIZED_UNIT" "$unit_temp"
mv -f "$unit_temp" "$UNIT_PATH"

install -d -o root -g root -m 0755 "$(dirname "$REVISION_PATH")"
revision_temp="$(mktemp "$(dirname "$REVISION_PATH")/.install-revision.XXXXXX")"
printf '%s\n' "$EXPECTED_REVISION" >"$revision_temp"
chown root:root "$revision_temp"
chmod 0644 "$revision_temp"
mv -f "$revision_temp" "$REVISION_PATH"

install -d -o root -g root -m 0755 /etc/noland/lifecycle
systemctl daemon-reload
systemctl enable noland-lifecycle-agent.service >/dev/null
systemctl restart noland-lifecycle-agent.service

for _ in $(seq 1 30); do
  if python3 - "$SOCKET_PATH" <<'__NOLAND_HEALTH_PY__'
import json
import socket
import sys

request = {"id": "install-readiness", "method": "GetHealth", "params": {}}
stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
stream.settimeout(2)
stream.connect(sys.argv[1])
stream.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
response = json.loads(stream.makefile("rb").readline(65537))
result = response.get("result") or {}
if response.get("id") != request["id"] or response.get("error") is not None:
    raise SystemExit(1)
if result.get("enabled") is not False or result.get("instanceId") != 0:
    raise SystemExit(1)
__NOLAND_HEALTH_PY__
  then
    printf '%s\n' NOLAND_LIFECYCLE_AGENT_READY
    exit 0
  fi
  sleep 1
  if ! systemctl is-active --quiet noland-lifecycle-agent.service; then
    echo "lifecycle-agent service stopped before its RPC socket became ready" >&2
    systemctl --no-pager --full status noland-lifecycle-agent.service >&2 || true
    exit 1
  fi
done

echo "lifecycle-agent service did not create its RPC socket within 30 seconds" >&2
systemctl --no-pager --full status noland-lifecycle-agent.service >&2 || true
exit 1
