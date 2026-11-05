#!/usr/bin/env bash
# Looks for what the living docs and skills name but the code no longer has:
# files, couch-cli subcommands, COUCH_* variables, test names and
# couch_*::paths. Results pages (docs/m*-results.md, docs/p*-results.md) are
# history and left alone. Prints each problem; exits 1 if there are any.
#
#   bash scripts/check-docs.sh          (Git Bash on Windows, or Linux)
#
# A name that is right but not ours (an upstream test, a runtime file) goes
# in the IGNORE list below, with why.
set -u
cd "$(dirname "$0")/.."

docs=(CLAUDE.md README.md docs/architecture.md docs/development.md docs/self-hosting.md docs/protocol.md docs/adr/*.md .claude/skills/*/SKILL.md)

# Names the docs may use that aren't in this repository.
IGNORE=(
  'src/extensions/Win32/System/StructuredStorage.rs' # the windows crate's source
  'endpoint_relay_map_change'                        # an iroh test
  'ff_d3d12va_common_frame_params'                   # an FFmpeg function
)
ignored() {
  local x
  for x in "${IGNORE[@]}"; do [[ "$1" == "$x" ]] && return 0; done
  return 1
}

problems=0
problem() {
  echo "$1"
  problems=$((problems + 1))
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if git rev-parse --is-inside-work-tree > /dev/null 2>&1; then
  { git ls-files; git ls-files --others --exclude-standard; } > "$tmp/files"
else
  # A copy without .git (WSL's, from scripts/wsl/sync.sh).
  find . -type f -not -path './target/*' -not -path '*/node_modules/*' | sed 's#^\./##' > "$tmp/files"
fi
code_dirs=(crates apps/desktop/src apps/desktop/src-tauri xtask scripts)

# Backticked names, one per line.
grep -ohE '`[^` ]+`' "${docs[@]}" | tr -d '`' | sed 's#\\#/#g' | sort -u > "$tmp/names"

# 1. Files: paths with a folder, and bare file names of code.
while read -r p; do
  case "$p" in *'<'* | *'*'* | *'%'* | *'~'* | *'='* | *'$'* | http*) continue ;; esac
  ignored "$p" && continue
  if [[ "$p" == */* ]]; then
    [[ "$p" =~ ^(crates|apps|scripts|docs|xtask|deploy|third_party|\.github|\.claude|src|src-tauri)/ ]] || continue
  else
    [[ "$p" =~ \.(rs|ts|tsx|ps1|sh|mjs|js|css|nsh)$ ]] || continue
  fi
  q="${p%/}"
  q="${q#./}"
  [[ -e "$q" ]] && continue
  found=""
  for root in crates apps/desktop apps/desktop/src apps/desktop/src-tauri docs scripts xtask/src deploy; do
    [[ -e "$root/$q" ]] && found=1 && break
  done
  [[ -n "$found" ]] && continue
  awk -v n="$(basename "$q")" '$0 == n || substr($0, length($0) - length(n)) == "/" n { found = 1; exit } END { exit !found }' "$tmp/files" && continue
  problem "file: $p"
done < <(grep -E '\.[a-z]{1,4}/?$|/' "$tmp/names")

# 2. couch-cli subcommands, from its Cmd enum.
sed -n '/^enum Cmd {/,/^}/p' crates/couch-cli/src/main.rs |
  grep -oE '^    [A-Z][A-Za-z]+' |
  sed -E 's/^ +//; s/([a-z])([A-Z])/\1-\2/g' | tr 'A-Z' 'a-z' | sort -u > "$tmp/subs"
grep -ohE 'couch-cli(\.exe)? +[a-z][a-z-]+' "${docs[@]}" | awk '{print $NF}' | sort -u |
  while read -r s; do
    [[ "$s" == or || "$s" == and ]] && continue
    grep -qx "$s" "$tmp/subs" || echo "couch-cli subcommand: $s"
  done > "$tmp/out"
while read -r line; do problem "$line"; done < "$tmp/out"

# 3. COUCH_* variables the code never reads (a prefix ending in _ names a family).
grep -rohE '\bCOUCH_[A-Z0-9_]+' "${code_dirs[@]}" --include='*.rs' --include='*.ps1' --include='*.sh' --include='*.toml' --include='*.mjs' --include='*.ts' 2>/dev/null | sort -u > "$tmp/env"
grep -ohE '\bCOUCH_[A-Z0-9_]+' "${docs[@]}" | sort -u | while read -r v; do
  if [[ "$v" == *_ ]]; then
    grep -q "^$v" "$tmp/env" || echo "variable family: $v*"
  else
    grep -qx "$v" "$tmp/env" || echo "variable: $v"
  fi
done > "$tmp/out"
while read -r line; do problem "$line"; done < "$tmp/out"

# 4. Test and scenario names (snake_case, four words or more).
grep -E '^[a-z][a-z0-9]*(_[a-z0-9]+){3,}$' "$tmp/names" | while read -r t; do
  ignored "$t" && continue
  grep -rqw "$t" "${code_dirs[@]}" --include='*.rs' --include='*.ts' --include='*.tsx' --include='*.js' 2>/dev/null || echo "test or name: $t"
done > "$tmp/out"
while read -r line; do problem "$line"; done < "$tmp/out"

# 5. couch_*::paths: every segment somewhere in its crate.
grep -ohE '\bcouch_[a-z_]+(::[A-Za-z_][A-Za-z0-9_]*)+' "${docs[@]}" | sort -u | while read -r p; do
  crate="${p%%::*}"
  dir="crates/${crate//_/-}"
  if [[ ! -d "$dir" ]]; then
    echo "crate: $p"
    continue
  fi
  rest="${p#*::}"
  for seg in ${rest//::/ }; do
    grep -rqw "$seg" "$dir" || { echo "path: $p ($seg)"; break; }
  done
done > "$tmp/out"
while read -r line; do problem "$line"; done < "$tmp/out"

if ((problems > 0)); then
  echo "check-docs: $problems problem(s)"
  exit 1
fi
echo "check-docs: nothing stale found"
