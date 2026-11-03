#!/bin/bash
# Builds the Linux AppImage (M16b) for another PC, as build-installer.ps1 builds the
# Windows installer. On Ubuntu 24.04 (WSL's included), from a git clone: the notices
# link the app's source at its commit, so a release AppImage needs a clean tree.
#   bash scripts/build-appimage.sh                  # release: our relay and its token, from release.toml
#   bash scripts/build-appimage.sh --config <file>  # another file, e.g. a LAN test one (test = true)
#   bash scripts/build-appimage.sh --test-relay     # a test AppImage: iroh's public relays, no file needed
#   --invite-page https://<user>.github.io/<repository>/j/   # where Copy invite links to, this build
#   --no-invite-page                                          # couchco:// links, this build
# release.toml is ~/.couch-coapp/release.toml, or COUCH_RELEASE_CONFIG; from WSL, the
# Windows one: COUCH_RELEASE_CONFIG=/mnt/c/Users/<you>/.couch-coapp/release.toml.
# `cargo xtask appimage` does the work. Then target/appimage/ holds the AppImage, its
# notices, FFmpeg's sources and SOURCES.txt, to hand out together:
#   npx --yes http-server target/appimage -p 8000
set -euo pipefail
# A relative --config or COUCH_RELEASE_CONFIG is the caller's, not the repository's.
export COUCH_INVOKED_FROM="$PWD"
cd "$(dirname "$0")/.."
. scripts/dev.sh
exec cargo xtask appimage "$@"
