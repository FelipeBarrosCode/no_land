#!/usr/bin/env bash
set -euo pipefail
: "${UBUNTU_IMAGE:?Set an Ubuntu 24.04 image pinned by sha256}"
: "${APT_SNAPSHOT:?Set an immutable Ubuntu archive snapshot timestamp}"
: "${RUNTIME_ID:?Set an immutable runtime ID}"
: "${ARTIFACT_URL:?Set the final immutable HTTPS artifact URL}"
[[ "$UBUNTU_IMAGE" =~ ^ubuntu:24\.04@sha256:[a-f0-9]{64}$ ]]
[[ "$APT_SNAPSHOT" =~ ^[0-9]{8}T[0-9]{6}Z$ ]]
[[ "$RUNTIME_ID" =~ ^[a-zA-Z0-9_-]{1,96}$ ]]
[[ "$ARTIFACT_URL" == https://* ]]
mkdir -p dist/runtime
docker build --platform linux/amd64 --build-arg "UBUNTU_IMAGE=$UBUNTU_IMAGE" --build-arg "APT_SNAPSHOT=$APT_SNAPSHOT" -t "noland-runtime:$RUNTIME_ID" runtime-images/ubuntu24
# Dereference links INSIDE the container. The installer never needs unsafe
# absolute/parent-traversing archive links, special devices or ownership restores.
# Export explicit trees, not /proc, /sys, /dev, live /run or Docker credentials.
# Only the fixed, offline image exporter needs to read root-owned metadata.
# The built image defaults to the unprivileged runtime user.
docker run --rm --user 0 --platform linux/amd64 -v "$PWD/scripts/runtime/export.py:/export.py:ro" "noland-runtime:$RUNTIME_ID" \
  python3 /export.py > dist/runtime/ubuntu24-x86_64.tar.gz
python3 scripts/runtime/manifest.py
