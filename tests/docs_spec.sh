#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

fail() {
  printf 'docs_spec: %s\n' "$*" >&2
  exit 1
}

grep -Fq 'Current module ownership lives in' "$ROOT/AGENTS.md" ||
  fail 'AGENTS.md must route module ownership to docs/architecture.md'
grep -Fq 'current file ownership and module seams live only in' "$ROOT/CLAUDE.md" ||
  fail 'CLAUDE.md must route module ownership to docs/architecture.md'
grep -Fq '## Module seams' "$ROOT/docs/architecture.md" ||
  fail 'docs/architecture.md must own the current module map'

# A second file-by-file inventory recreates the drift this contract prevents.
if grep -Fq 'Module split (`src/`)' "$ROOT/CLAUDE.md"; then
  fail 'CLAUDE.md must keep invariants, not duplicate the architecture module map'
fi

markdown_files=("$ROOT/AGENTS.md" "$ROOT/CLAUDE.md" "$ROOT/README.md")
while IFS= read -r file; do
  markdown_files+=("$file")
done < <(find "$ROOT/docs" -type f -name '*.md' -print | sort)

# Validate backticked repository paths such as `src/action.rs::open_target`.
# Symbols after `::` are compiler-checked; this guard checks the file ownership
# part that otherwise becomes stale silently after a move.
while IFS= read -r reference; do
  path="${reference#\`}"
  path="${path%%::*}"
  path="${path%,}"
  path="${path%.}"
  case "$path" in
    *.rs | *.sh | *.md | *.toml)
      [ -e "$ROOT/$path" ] || fail "missing referenced path: $path"
      ;;
  esac
done < <(
  grep -Eho '`((src|bin|tests|docs)/[^`[:space:]]+|(AGENTS|CLAUDE|README|CHANGELOG)\.md|Cargo\.toml|herdr-plugin\.toml)' \
    "${markdown_files[@]}" | sort -u
)

# Check ordinary local Markdown links as well as inline-code references.
while IFS= read -r record; do
  file="${record%%:*}"
  target="${record#*:}"
  target="${target#](}"
  target="${target#<}"
  target="${target%>}"
  target="${target%%#*}"
  case "$target" in
    '' | http://* | https://* | mailto:* | /*) continue ;;
  esac
  [ -e "$(dirname -- "$file")/$target" ] ||
    fail "missing local Markdown link from ${file#"$ROOT/"}: $target"
done < <(
  grep -EHo ']\([^` ()#]+' "${markdown_files[@]}"
)

printf 'docs_spec: ok\n'
