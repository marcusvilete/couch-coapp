#!/usr/bin/env bash
# Lists `pub` items of the workspace's library crates whose name appears
# nowhere else in the workspace's Rust (src, tests, apps, xtask):
# candidates for dead code that rustc can't flag, since it never calls a
# library's `pub` item unused. A name used anywhere else, even unrelated,
# hides an item (common names: `new`, `start`), so this finds the clearly
# unused ones; read each before removing it, and look again after: what
# only it used may be unused next. Generated bindings are skipped.
#
#   bash scripts/tidy/unused-pub.sh              # every library crate
#   bash scripts/tidy/unused-pub.sh couch-room   # one
#   TESTS_ONLY=1 bash scripts/tidy/unused-pub.sh # used by tests alone: uses
#     in `tests/` folders and couch-testkit don't count (a `#[cfg(test)]`
#     module inside a source file still does)
set -uo pipefail
cd "$(dirname "$0")/../.."

all_rs=$(git ls-files '*.rs' | grep -v '/bindings/')
if [ "${TESTS_ONLY:-0}" = 1 ]; then
  all_rs=$(printf '%s\n' $all_rs | grep -v -e '/tests/' -e '^crates/couch-testkit/')
fi
crates=${1:-$(ls crates)}
for crate in $crates; do
  dir="crates/$crate"
  [ -f "$dir/src/lib.rs" ] || continue
  git ls-files "$dir/src/*.rs" | grep -v '/bindings/' | while read -r file; do
    grep -nE '^\s*pub (async )?(unsafe )?(fn|struct|enum|trait|const|static|type|mod) [A-Za-z_][A-Za-z0-9_]*' "$file" \
      | sed -E 's/^([0-9]+):\s*pub (async )?(unsafe )?(fn|struct|enum|trait|const|static|type|mod) ([A-Za-z_][A-Za-z0-9_]*).*/\1 \4 \5/' \
      | while read -r line kind name; do
          # Uses: every whole-word match counted, the definition included.
          uses=$(printf '%s\n' $all_rs | xargs grep -wo -- "$name" 2>/dev/null | wc -l)
          if [ "$uses" -le 1 ]; then
            echo "$file:$line $kind $name"
          fi
        done
  done
done
