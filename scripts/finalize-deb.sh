#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  echo "Usage: $0 --bundle-dir DIR" >&2
  exit 2
}

bundle_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle-dir)
      [[ $# -ge 2 ]] || usage
      bundle_dir="$2"
      shift 2
      ;;
    *)
      usage
      ;;
  esac
done

[[ -n "$bundle_dir" ]] || usage
for command in dpkg-deb gzip md5sum; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "$command is required to finalize Debian packages." >&2
    exit 1
  fi
done

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
changelog_source="$repo_root/src-tauri/packaging/changelog"
if [[ ! -s "$changelog_source" ]]; then
  echo "Generated Debian changelog is missing: $changelog_source" >&2
  exit 1
fi

mapfile -t packages < <(find "$bundle_dir" -type f -name '*.deb' -print | sort)
if [[ "${#packages[@]}" -eq 0 ]]; then
  echo "No .deb package found below $bundle_dir" >&2
  exit 1
fi

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

for package in "${packages[@]}"; do
  contents=$(dpkg-deb --contents "$package")
  if grep -Fq 'usr/share/doc/noland-connect/changelog.gz' <<<"$contents"; then
    echo "Debian changelog already present in $package"
    continue
  fi

  package_name=$(dpkg-deb --field "$package" Package)
  package_root="$work_dir/package-root"
  rebuilt_package="$work_dir/rebuilt.deb"
  rm -rf "$package_root" "$rebuilt_package"
  mkdir -p "$package_root"
  dpkg-deb --raw-extract "$package" "$package_root"

  documentation_dir="$package_root/usr/share/doc/$package_name"
  mkdir -p "$documentation_dir"
  gzip -9 -n -c "$changelog_source" > "$documentation_dir/changelog.gz"
  chmod 0644 "$documentation_dir/changelog.gz"

  # Rebuild the payload checksum manifest so the inserted documentation is
  # covered by the same integrity metadata as the original bundle contents.
  (
    cd "$package_root"
    find . -path ./DEBIAN -prune -o -type f -print0 \
      | sort -z \
      | xargs -0 md5sum \
      | sed 's#  \./#  #'
  ) > "$package_root/DEBIAN/md5sums"
  chmod 0644 "$package_root/DEBIAN/md5sums"

  dpkg-deb --root-owner-group --build "$package_root" "$rebuilt_package" >/dev/null
  mv "$rebuilt_package" "$package"
  echo "Inserted Debian changelog into $package"
done
