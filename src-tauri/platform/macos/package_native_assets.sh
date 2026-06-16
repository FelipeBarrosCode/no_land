#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
BRIDGE_SOURCE="$SCRIPT_DIR/.build/release/NolandTunnelBridge"
APP_RESOURCES_DIR="$ROOT_DIR/target/release/bundle/macos/Noland Connect.app/Contents/Resources"

if [ ! -f "$BRIDGE_SOURCE" ]; then
  echo "Bridge binary not found at $BRIDGE_SOURCE. Build it first with swift build -c release." >&2
  exit 1
fi

mkdir -p "$APP_RESOURCES_DIR"
cp "$BRIDGE_SOURCE" "$APP_RESOURCES_DIR/NolandTunnelBridge"
echo "Copied NolandTunnelBridge into app resources: $APP_RESOURCES_DIR"
