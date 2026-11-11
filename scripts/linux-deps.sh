#!/bin/bash
# The system packages a Linux build needs (M16, ADR 0011), on Ubuntu 24.04 or
# Debian alike. Run once, as root:
#   sudo bash scripts/linux-deps.sh
# CI runs it too (.github/workflows/linux.yml). Rust comes from rustup (the
# repository's rust-toolchain.toml picks the version), Node 24 from NodeSource.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
# Retries and a time limit per fetch, so an unreachable mirror fails instead
# of hanging; and an update that couldn't fetch a list fails too (apt only
# warns by default, then installs from stale lists or not at all).
apt_options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
apt-get "${apt_options[@]}" update -qq --error-on=any
packages=(
  # Building: C for libopus and SDL3, clang for regenerating FFmpeg's bindings.
  build-essential cmake ninja-build pkg-config clang libclang-dev curl file xz-utils
  # The Tauri shell: WebKitGTK and its kin, the notification area icon.
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
  libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libssl-dev
  # SDL3, built from source: windows (Wayland, X11), GPU, sound, controllers.
  libwayland-dev wayland-protocols libxkbcommon-dev libdecor-0-dev
  libx11-dev libxext-dev libxrandr-dev libxcursor-dev libxi-dev libxss-dev
  libxfixes-dev libxtst-dev libxinerama-dev
  libdrm-dev libgbm-dev libegl-dev libgl-dev libgles-dev libvulkan-dev libva-dev
  libpipewire-0.3-dev libpulse-dev libasound2-dev
  libudev-dev libdbus-1-dev libibus-1.0-dev libusb-1.0-0-dev
)
apt-get "${apt_options[@]}" install -y -qq --no-install-recommends "${packages[@]}"
