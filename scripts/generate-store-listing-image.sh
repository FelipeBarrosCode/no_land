#!/usr/bin/env sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SRC=${1:-"$ROOT_DIR/src-tauri/icons/icon.png"}
OUT=${2:-"$ROOT_DIR/src-tauri/icons/StoreListingPortrait.png"}
WIDTH=${3:-720}
HEIGHT=${4:-1080}
ICON_SIZE=${5:-420}
BG=${6:-"#0b1020"}

mkdir -p "$(dirname -- "$OUT")"

magick -size "${WIDTH}x${HEIGHT}" xc:"$BG" \
  \( "$SRC" -resize "${ICON_SIZE}x${ICON_SIZE}" \) \
  -gravity center -composite \
  "$OUT"

echo "Generated $OUT (${WIDTH}x${HEIGHT}) from $SRC"
