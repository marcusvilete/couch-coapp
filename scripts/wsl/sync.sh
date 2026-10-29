#!/bin/bash
# Copies this Windows checkout into WSL's own disk (~/couch-coapp by default) for a
# Linux build (M16): building under /mnt/c is slow. Leaves out what each side builds
# or downloads for itself (target/, node_modules, the fetched third_party folders);
# vendored third_party code is copied. Run from Windows:
#   wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/dev/couch-coapp/scripts/wsl/sync.sh
set -euo pipefail
src="$(cd "$(dirname "$0")/../.." && pwd)/"
dst="${COUCH_WSL_COPY:-$HOME/couch-coapp}/"
mkdir -p "$dst"
rsync -a --delete \
  --exclude /target/ --exclude node_modules/ --exclude /apps/desktop/dist/ --exclude /.git/ \
  --exclude /third_party/_downloads/ --exclude /third_party/ffmpeg/ --exclude /third_party/sdl3/ \
  --exclude /third_party/lucide/ --exclude /third_party/sources/ --exclude /third_party/drivers/ \
  --exclude '/third_party/_extract_*' \
  "$src" "$dst"
# Windows' checkout may carry CRLF; bash can't run it.
find "$dst/scripts" "$dst/deploy" -name '*.sh' -exec sed -i 's/\r$//' {} +
echo "synced to $dst"
