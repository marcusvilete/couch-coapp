#!/usr/bin/env bash
# Makes a relay code (M26a, ADR 0014): what your friends paste in Couch
# Co-App's Settings → Network to use your relay. Plain bash, for the server.
#
#   ./relay-code.sh https://relay.example.com/ <token> [--personal]
#
# The token is one of the relay's `access.shared_token` values. --personal
# marks a code given to one person (a token of their own in the list): their
# app never passes it on. The code holds the token: send it privately, never
# in a public channel. The format is couch_engine::relays::code's, which
# reads this script's output in its tests (keep both in step).
set -euo pipefail

usage() {
    echo "usage: $0 <relay URL, https://…/> <token> [--personal]" >&2
    exit 2
}

[[ $# -eq 2 || $# -eq 3 ]] || usage
url=$1
token=$2
flags=""
if [[ $# -eq 3 ]]; then
    [[ $3 == --personal ]] || usage
    flags=personal
fi
[[ $url == https://* || $url == http://* ]] || { echo "the URL must start with https://" >&2; exit 2; }
# The app writes a relay URL with its trailing slash; so does the code.
[[ $url == */ ]] || url="$url/"
[[ -n $token && $token != *$'\n'* ]] || { echo "the token must be one line, not empty" >&2; exit 2; }

payload=$(printf '%s\n%s\n%s' "$url" "$token" "$flags" | base64 -w0 | tr '+/' '-_' | tr -d '=')
check=$(printf '%s\n%s\n%s' "$url" "$token" "$flags" | sha256sum | cut -c1-8)
echo "couch-relay-1.$payload.$check"
