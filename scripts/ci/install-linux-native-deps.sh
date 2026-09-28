#!/usr/bin/env bash
set -Eeuo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential \
  cmake \
  pkg-config \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libasound2-dev \
  libudev-dev \
  libssl-dev \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libopus-dev \
  libpulse-dev \
  libsdl2-dev
