#!/bin/bash
# Starts the development app in WSLg (it shows on the Windows desktop) under a
# throwaway profile, from the WSL copy (sync.sh). Its log: ~/runs/app.log.
#   wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/dev/couch-coapp/scripts/wsl/app.sh [profile]
# Stop it with: pkill -f target/debug/couch-desktop; pkill -f vite
set -e
cd "${COUCH_WSL_COPY:-$HOME/couch-coapp}"
. scripts/dev.sh
cd apps/desktop
[ -d node_modules ] || npm ci --silent
mkdir -p ~/runs
COUCH_PROFILE="${1:-linuxtest}" npm run tauri dev > ~/runs/app.log 2>&1
