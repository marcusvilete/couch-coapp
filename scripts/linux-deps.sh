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
apt_options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
  -o DPkg::Lock::Timeout=120)
# A fetch that trickles gets past those limits (CI hung silently for its
# whole 10 minutes on 2026-11-17, where this step takes under a minute): each
# command is capped and tried once more, saying when it starts, so a log
# shows where it stalled.
attempt() {
  local limit=$1
  shift
  for try in 1 2; do
    echo "linux-deps: $(date -u +%T) ${1} (try ${try})"
    if timeout --kill-after=10 "$limit" "${@:2}"; then
      return 0
    fi
    echo "linux-deps: $(date -u +%T) ${1} failed or took over ${limit} s"
    # An install cut short leaves dpkg half done.
    dpkg --configure -a || true
  done
  return 1
}
attempt 120 "updating the package lists" apt-get "${apt_options[@]}" update -q --error-on=any
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
# Fetched first, showing each package, then installed from what was
# fetched: a log then tells a slow mirror from a slow install (CI timed out
# here three times in a row on 2026-11-19, saying neither). A try cut short
# leaves its partial downloads, which the next one resumes.
attempt 300 "downloading the packages" \
  apt-get "${apt_options[@]}" install -y -q --download-only --no-install-recommends "${packages[@]}"
attempt 300 "installing the packages" \
  apt-get "${apt_options[@]}" install -y -qq --no-install-recommends "${packages[@]}"
echo "linux-deps: $(date -u +%T) done"
