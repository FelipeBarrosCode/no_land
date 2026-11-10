#!/usr/bin/env bash
set -euo pipefail

bundle_dir=${1:?Usage: fix-appimage-metadata.sh <bundle-dir> <x64|arm64>}
architecture=${2:?Usage: fix-appimage-metadata.sh <bundle-dir> <x64|arm64>}

# Keep the release URL and checksums together; continuous assets are replaced upstream.
tool_version=1.9.1

case "$architecture" in
  x64)
    tool_arch=x86_64
    output_arch=x86_64
    expected_sha256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
    ;;
  arm64)
    tool_arch=aarch64
    output_arch=aarch64
    expected_sha256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
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
# Stage on the artifact's filesystem, so the final, verified replacement is a rename.
work_dir=$(mktemp -d "$(dirname "$appimage")/.appimage-metadata.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT

extract_dir="$work_dir/appimage"
mkdir -p "$extract_dir"
(
  cd "$extract_dir"
  chmod +x "$appimage"
  "$appimage" --appimage-extract >/dev/null
)
appdir="$extract_dir/squashfs-root"

verify_public_entrypoint() {
  local root=$1
  # FUSE can hide a bad root inode mode. Firejail mounts SquashFS with the
  # kernel, which correctly denies access to root-owned 0700 directories.
  if [[ -z "$(find "$root" -maxdepth 0 -type d -perm 0755 -print -quit)" ]]; then
    echo "The packaged AppDir root must have mode 0755 for non-root users: $root" >&2
    return 1
  fi
  if [[ -z "$(find -L "$root/AppRun" -maxdepth 0 -type f -perm 0755 -print -quit)" ]]; then
    echo "The packaged AppRun must have mode 0755 for non-root users: $root/AppRun" >&2
    return 1
  fi
}

# AppRun alone being executable is insufficient: the serialized filesystem root
# must also be traversable after appimagetool changes its owner to root.
chmod 0755 "$appdir"

# AppRun is the AppImage entry point. Some Tauri/linuxdeploy combinations
# leave it without an executable bit, which makes sandboxed catalog checks
# fail before the application can start.
if [[ ! -e "$appdir/AppRun" ]]; then
  echo "The extracted AppImage has no AppRun entry point." >&2
  exit 1
fi
chmod 0755 "$appdir/AppRun"
test -x "$appdir/AppRun"
verify_public_entrypoint "$appdir"

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
tool_url="https://github.com/AppImage/appimagetool/releases/download/${tool_version}/appimagetool-${tool_arch}.AppImage"
curl --fail --location --retry 3 --silent --show-error "$tool_url" --output "$tool"
if ! printf '%s  %s\n' "$expected_sha256" "$tool" | sha256sum --check; then
  echo "appimagetool $tool_version ($tool_arch) checksum verification failed." >&2
  exit 1
fi
chmod +x "$tool"

tool_extract_dir="$work_dir/appimagetool"
mkdir -p "$tool_extract_dir"
(
  cd "$tool_extract_dir"
  "$tool" --appimage-extract >/dev/null
)

repacked="$work_dir/$(basename "$appimage")"
ARCH="$output_arch" SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}" \
  "$tool_extract_dir/squashfs-root/AppRun" \
    --mksquashfs-opt=-root-mode --mksquashfs-opt=755 "$appdir" "$repacked"
test -s "$repacked"
chmod 0755 "$repacked"

verify_dir="$work_dir/verify"
mkdir -p "$verify_dir"
(
  cd "$verify_dir"
  "$repacked" --appimage-extract >/dev/null
  verify_public_entrypoint squashfs-root
  test -x squashfs-root/AppRun
  test -s squashfs-root/.DirIcon
  file squashfs-root/.DirIcon | grep -q 'PNG image data'
  desktop=$(find squashfs-root -maxdepth 1 -name '*.desktop' -print -quit)
  test -n "$desktop"
  test -e "$desktop"
)

# Failed validation must leave the original artifact intact.
mv "$repacked" "$appimage"
echo "Repacked and verified AppImage metadata: $appimage"
