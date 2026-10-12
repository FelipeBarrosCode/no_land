#!/usr/bin/env bash
# Run natively on Ubuntu 22.04 x86_64 or arm64, never on the client's OS.
set -euo pipefail
case "$(uname -m)" in
  x86_64|aarch64) arch="$(uname -m)" ;;
  *) echo "Unsupported remote-agent architecture" >&2; exit 1 ;;
esac
root="$(cd "$(dirname "$0")/../.." && pwd)"
dest="$root/state-agent/prebuilt/$arch"
mkdir -p "$dest"
cargo build --locked --release --manifest-path "$root/state-agent/Cargo.toml" -p noland-state-agent -p noland-lifecycle-agent
cargo build --locked --release --manifest-path "$root/network-agent/Cargo.toml"
cargo build --locked --release --manifest-path "$root/vm-cloud-mic-agent/Cargo.toml"
cp "$root/state-agent/target/release/noland-state-agent" "$dest/"
cp "$root/state-agent/target/release/noland-lifecycle-agent" "$dest/"
cp "$root/network-agent/target/release/noland-network-agent" "$dest/"
cp "$root/vm-cloud-mic-agent/target/release/noland-mic-receiver" "$dest/"
bpf="$(find "$root/state-agent/target/release/build" -path '*/out/noland_observer.bpf.o' -type f -print -quit)"
test -s "$bpf"
cp "$bpf" "$dest/noland_observer.bpf.o"
for binary in noland-state-agent noland-lifecycle-agent noland-network-agent noland-mic-receiver; do
  readelf -h "$dest/$binary"
  ldd "$dest/$binary" | tee "$dest/$binary.dependencies.txt"
  if grep -q 'not found' "$dest/$binary.dependencies.txt"; then exit 1; fi
done
(cd "$dest" && sha256sum noland-state-agent noland-lifecycle-agent noland-network-agent noland-mic-receiver noland_observer.bpf.o > SHA256SUMS)
