#!/bin/bash
# Runs the commands in a file inside the WSL copy (sync.sh), with scripts/dev.sh's
# environment. A file rather than arguments: wsl.exe splits a command line passed
# from PowerShell at its spaces, quotes and pipes included. From Windows:
#   wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/dev/couch-coapp/scripts/wsl/run.sh <file>
# where <file> is a path WSL can read (/mnt/c/...), its lines plain bash.
set -o pipefail
commands="$(readlink -f "${1:?the file of commands to run}")"
cd "${COUCH_WSL_COPY:-$HOME/couch-coapp}"
. scripts/dev.sh
# The file may come from Windows with CRLF.
. <(sed 's/\r$//' "$commands")
