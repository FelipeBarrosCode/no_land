#!/usr/bin/env bash
set -euo pipefail

bundle_dir=${1:?Usage: fix-appimage-metadata.sh <bundle-dir> <x64|arm64>}
architecture=${2:?Usage: fix-appimage-metadata.sh <bundle-dir> <x64|arm64>}

case "$architecture" in
  x64)
    tool_arch=x86_64
    output_arch=x86_64
    expected_sha256=a6d71e2b6cd66f8e8d16c37ad164658985e0cf5fcaa950c90a482890cb9d13e0
    ;;
  arm64)
    tool_arch=aarch64
    output_arch=aarch64
    expected_sha256=1b00524ba8c6b678dc15ef88a5c25ec24def36cdfc7e3abb32ddcd068e8007fe
    ;;
  *)
    echo "Unsupported AppImage architecture: $architecture" >&2
    exit 1
    ;;
esac

mapfile -d '' appimages < <(find "$bundle_dir" -type f -name '*.AppImage' -print0)
if [[ ${#appimages[@]} -ne 1 ]]; then
  echo "Expected exactly one AppImage under $bundle_dir; found ${#appimages[@]}." >&2
  exit 1
fi

appimage=$(realpath "${appimages[0]}")
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

extract_dir="$work_dir/appimage"
mkdir -p "$extract_dir"
(
  cd "$extract_dir"
  chmod +x "$appimage"
  "$appimage" --appimage-extract >/dev/null
)
appdir="$extract_dir/squashfs-root"

mapfile -d '' desktop_files < <(find "$appdir/usr/share/applications" -maxdepth 1 -type f -name '*.desktop' -print0)
if [[ ${#desktop_files[@]} -ne 1 ]]; then
  echo "Expected exactly one packaged desktop file; found ${#desktop_files[@]}." >&2
  exit 1
fi

desktop_file=${desktop_files[0]}
desktop_name=$(basename "$desktop_file")
icon_name=$(sed -n 's/^Icon=//p' "$desktop_file" | head -n 1)
if [[ -z "$icon_name" ]]; then
  echo "The packaged desktop file has no Icon entry: $desktop_file" >&2
  exit 1
fi

icon_file="$appdir/${icon_name}.png"
if [[ ! -f "$icon_file" ]]; then
  mapfile -d '' root_icons < <(find "$appdir" -maxdepth 1 -type f -iname '*.png' -print0)
  if [[ ${#root_icons[@]} -ne 1 ]]; then
    echo "Could not uniquely identify the root AppImage icon for Icon=$icon_name." >&2
    exit 1
  fi
  icon_file=${root_icons[0]}
fi

# Tauri bundler versions affected by tauri-apps/tauri#15110 create absolute
# symlinks here. They point back into the CI workspace and are dangling once
# the AppImage is mounted on another machine. A real PNG is valid per AppDir.
rm -f "$appdir/.DirIcon"
cp "$icon_file" "$appdir/.DirIcon"

rm -f "$appdir/$desktop_name"
ln -s "usr/share/applications/$desktop_name" "$appdir/$desktop_name"

test -s "$appdir/.DirIcon"
test -e "$appdir/$desktop_name"
file "$appdir/.DirIcon" | grep -q 'PNG image data'

tool="$work_dir/appimagetool-${tool_arch}.AppImage"
tool_url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${tool_arch}.AppImage"
curl --fail --location --retry 3 --silent --show-error "$tool_url" --output "$tool"
printf '%s  %s\n' "$expected_sha256" "$tool" | sha256sum --check --status
chmod +x "$tool"

tool_extract_dir="$work_dir/appimagetool"
mkdir -p "$tool_extract_dir"
(
  cd "$tool_extract_dir"
  "$tool" --appimage-extract >/dev/null
)

repacked="$work_dir/$(basename "$appimage")"
ARCH="$output_arch" SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}" \
  "$tool_extract_dir/squashfs-root/AppRun" "$appdir" "$repacked"
test -s "$repacked"
chmod +x "$repacked"
mv "$repacked" "$appimage"

verify_dir="$work_dir/verify"
mkdir -p "$verify_dir"
(
  cd "$verify_dir"
  "$appimage" --appimage-extract >/dev/null
  test -s squashfs-root/.DirIcon
  file squashfs-root/.DirIcon | grep -q 'PNG image data'
  desktop=$(find squashfs-root -maxdepth 1 -name '*.desktop' -print -quit)
  test -n "$desktop"
  test -e "$desktop"
)

echo "Repacked and verified AppImage metadata: $appimage"
