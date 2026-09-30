#!/usr/bin/env bash
set -euo pipefail

appimage=${1:?Usage: extract-appimage.sh <AppImage> <destination>}
destination=${2:?Usage: extract-appimage.sh <AppImage> <destination>}

appimage=$(realpath "$appimage")
mkdir -p "$destination"

offset=$(python3 - "$appimage" <<'PY'
import struct
import sys

path = sys.argv[1]
with open(path, "rb") as handle:
    payload = handle.read()

cursor = 0
while True:
    offset = payload.find(b"hsqs", cursor)
    if offset < 0:
        raise SystemExit("Could not locate a SquashFS filesystem in the AppImage")
    if offset + 32 <= len(payload):
        block_size = struct.unpack_from("<I", payload, offset + 12)[0]
        compression = struct.unpack_from("<H", payload, offset + 20)[0]
        major = struct.unpack_from("<H", payload, offset + 28)[0]
        if major == 4 and 4096 <= block_size <= 1048576 and block_size & (block_size - 1) == 0 and 1 <= compression <= 6:
            print(offset)
            break
    cursor = offset + 4
PY
)

unsquashfs -no-progress -o "$offset" -d "$destination/squashfs-root" "$appimage" >/dev/null
test -d "$destination/squashfs-root"

echo "Extracted $(basename "$appimage") at SquashFS offset $offset"
