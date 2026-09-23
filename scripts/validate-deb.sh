#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  echo "Usage: $0 [--bundle-dir DIR]" >&2
  exit 2
}

bundle_dir="src-tauri/target"
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

if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "dpkg-deb is required to validate Debian packages." >&2
  exit 1
fi
if ! command -v lintian >/dev/null 2>&1; then
  echo "lintian is required. Install it with: sudo apt-get install lintian" >&2
  exit 1
fi

mapfile -t packages < <(find "$bundle_dir" -type f -name '*.deb' -print | sort)
if [[ "${#packages[@]}" -eq 0 ]]; then
  echo "No .deb package found below $bundle_dir" >&2
  exit 1
fi

for package in "${packages[@]}"; do
  echo "Validating $package"
  dpkg-deb --info "$package" >/dev/null
  contents=$(dpkg-deb --contents "$package")
  if ! grep -Fq 'usr/share/metainfo/com.noland.connect.metainfo.xml' <<<"$contents"; then
    echo "Package is missing /usr/share/metainfo/com.noland.connect.metainfo.xml: $package" >&2
    exit 1
  fi
  # Keep all pedantic diagnostics visible, but only Debian policy errors block
  # publishing. Advisory warnings (for example, missing helper man pages) are
  # still reported for follow-up without discarding an otherwise valid bundle.
  lintian --pedantic --fail-on error "$package"
done

echo "Validated ${#packages[@]} Debian package(s)."
