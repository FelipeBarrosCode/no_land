#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

PROJECT_NAME="NolandMacOSWireGuard"
PROJECT_PATH="$SCRIPT_DIR/${PROJECT_NAME}.xcodeproj"
WORKSPACE_SWIFTPM_DIR="$PROJECT_PATH/project.xcworkspace/xcshareddata/swiftpm"
DERIVED_DATA_ROOT="$HOME/Library/Developer/Xcode/DerivedData"

echo "Resetting local macOS/Xcode setup for ${PROJECT_NAME}..."

if [ -d "$PROJECT_PATH" ]; then
  rm -rf "$PROJECT_PATH"
  echo "Removed generated Xcode project: $PROJECT_PATH"
fi

if [ -d "$WORKSPACE_SWIFTPM_DIR" ]; then
  rm -rf "$WORKSPACE_SWIFTPM_DIR"
  echo "Removed local SwiftPM workspace metadata: $WORKSPACE_SWIFTPM_DIR"
fi

find "$DERIVED_DATA_ROOT" -maxdepth 1 -type d -name "${PROJECT_NAME}-*" -print0 2>/dev/null | while IFS= read -r -d '' path; do
  rm -rf "$path"
  echo "Removed DerivedData: $path"
done

if [ -d "$SCRIPT_DIR/.build" ]; then
  rm -rf "$SCRIPT_DIR/.build"
  echo "Removed local Swift build artifacts: $SCRIPT_DIR/.build"
fi

echo "Done. Next steps:"
echo "  1. bash generate_xcode_project.sh"
echo "  2. open \"$PROJECT_PATH\""
echo "  3. Re-add the vendored wireguard-apple local package"
echo "  4. Reapply signing/capabilities in Xcode"
