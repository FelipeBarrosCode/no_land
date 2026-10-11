#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  echo "Usage: $0 --bundle-dir DIR --arch <x64|arm64>" >&2
  exit 2
}

bundle_dir=""
architecture=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle-dir)
      [[ $# -ge 2 ]] || usage
      bundle_dir=$2
      shift 2
      ;;
    --arch)
      [[ $# -ge 2 ]] || usage
      architecture=$2
      shift 2
      ;;
    *) usage ;;
  esac
done

[[ -n "$bundle_dir" && -n "$architecture" ]] || usage
case "$architecture" in
  x64) package_arch=x86_64 ;;
  arm64) package_arch=aarch64 ;;
  *) usage ;;
esac

for command in docker node sha256sum; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "$command is required to build the Arch Linux package." >&2
    exit 1
  }
done

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bundle_dir=$(realpath "$bundle_dir")
mapfile -d '' deb_packages < <(find "$bundle_dir" -type f -name '*.deb' -print0)
if [[ ${#deb_packages[@]} -ne 1 ]]; then
  echo "Expected exactly one Debian package under $bundle_dir; found ${#deb_packages[@]}." >&2
  exit 1
fi

version=$(node -p "require('$repo_root/src-tauri/tauri.conf.json').version")
deb_sha256=$(sha256sum "${deb_packages[0]}" | awk '{print $1}')
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
cp "${deb_packages[0]}" "$work_dir/noland-connect.deb"

sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@ARCH@/$package_arch/g" \
  -e "s/@DEB_SHA256@/$deb_sha256/g" \
  "$repo_root/packaging/arch/PKGBUILD.in" > "$work_dir/PKGBUILD"

docker run --rm \
  --env HOST_UID="$(id -u)" \
  --env HOST_GID="$(id -g)" \
  --volume "$work_dir:/build" \
  --workdir /build \
  "${ARCHLINUX_IMAGE:-archlinux:base-devel}" \
  bash -Eeuo pipefail -c '
    useradd --create-home builder
    chown -R builder:builder /build
    runuser -u builder -- makepkg --noconfirm --cleanbuild --nodeps
    package=$(find /build -maxdepth 1 -type f -name "*.pkg.tar.zst" -print -quit)
    test -n "$package"
    pacman -Qip "$package"
    pacman -Qlp "$package" | grep -Eq "usr/bin/noland-connect$"
    chown -R "$HOST_UID:$HOST_GID" /build
  '

output_dir="$bundle_dir/arch"
mkdir -p "$output_dir"
mapfile -d '' built_packages < <(find "$work_dir" -maxdepth 1 -type f -name '*.pkg.tar.zst' -print0)
if [[ ${#built_packages[@]} -ne 1 ]]; then
  echo "Expected one Arch Linux package; found ${#built_packages[@]}." >&2
  exit 1
fi
cp "${built_packages[0]}" "$output_dir/"
echo "Built and validated Arch Linux package: $output_dir/$(basename "${built_packages[0]}")"
