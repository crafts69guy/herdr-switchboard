#!/usr/bin/env bash
# Clone flow: resolve a repository reference (clipboard first, then an editable
# prompt), clone it with ghq get, and open the result with the default target.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/lib.sh
source "$SCRIPT_DIR/lib.sh"

PLUGIN_ROOT="${HERDR_PLUGIN_ROOT:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
CONFIG_DIR="${HERDR_PLUGIN_CONFIG_DIR:-$PLUGIN_ROOT/.config}"

command -v ghq >/dev/null 2>&1 || die "ghq is required — brew install ghq." "ghq not found on PATH"

# The switcher binary is the single reader of namespaced config and the single
# implementation of the open verbs — the clone flow delegates to both rather
# than mirroring them here. `config get` reads from HERDR_PLUGIN_CONFIG_DIR.
export HERDR_PLUGIN_CONFIG_DIR="$CONFIG_DIR"
SWITCHBOARD_BIN="$(ensure_built)"
export SWITCHBOARD_BIN
configure_notifications "$SWITCHBOARD_BIN" true

clone_source="$("$SWITCHBOARD_BIN" config get clone_source clipboard)"
open_after="$("$SWITCHBOARD_BIN" config get open_after_clone true)"
default_target="$("$SWITCHBOARD_BIN" config get default_target workspace)"
label_mode="$("$SWITCHBOARD_BIN" config get label repo)"

ROOT="$(ghq_root)"
[[ -n "$ROOT" ]] || die "ghq root is not configured." "ghq root returned empty"

clipboard_read() {
  if command -v pbpaste >/dev/null 2>&1; then
    pbpaste 2>/dev/null
  elif command -v wl-paste >/dev/null 2>&1; then
    wl-paste 2>/dev/null
  elif command -v xclip >/dev/null 2>&1; then
    xclip -o -selection clipboard 2>/dev/null
  fi
}

looks_like_repo() {
  local s="$1"
  [[ "$s" =~ ^(https?://|git@|ssh://) ]] && return 0
  [[ "$s" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] && return 0
  [[ "$s" =~ ^[A-Za-z0-9.-]+/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] && return 0
  return 1
}

prefill=""
if [[ "$clone_source" == "clipboard" ]]; then
  clip="$(clipboard_read | tr -d '\r\n' | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
  if looks_like_repo "$clip"; then
    prefill="$clip"
  fi
fi

printf '\033[1m Clone a repository\033[0m\n'
printf '\033[2m owner/repo · host/owner/repo · full git URL\033[0m\n\n'
# read -e -i pre-fills the readline buffer with the clipboard guess; the user
# can accept it with Enter or edit it inline.
if ! IFS= read -r -e -i "$prefill" -p 'Repository: ' url; then
  exit 0
fi
url="$(printf '%s' "$url" | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
[[ -n "$url" ]] || {
  printf '\nNothing to clone.\n'
  exit 0
}

before="$(ghq list --full-path 2>/dev/null | sort)"

printf '\n'
if ! ghq get -- "$url"; then
  notify "Clone failed — check the pane." request
  printf '\n\033[2mpress any key to close\033[0m'
  read -rsn1 _ || true
  exit 1
fi

after="$(ghq list --full-path 2>/dev/null | sort)"
newpath="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -1)"

if [[ -z "$newpath" ]]; then
  # Already cloned (ghq get is a no-op): resolve the path by its owner/repo tail.
  tail_ref="$(printf '%s' "$url" | sed -E 's#^[a-z]+://##; s#^git@##; s#:#/#; s#\.git$##; s#/+$##')"
  base2="$(printf '%s' "$tail_ref" | awk -F/ '{ if (NF>=2) printf "%s/%s", $(NF-1), $NF; else print $NF }')"
  newpath="$(ghq list --full-path 2>/dev/null | grep -E "/${base2}$" | head -1)"
fi

if [[ -z "$newpath" || ! -d "$newpath" ]]; then
  notify "Cloned, but Switchboard could not resolve the path to open." request
  log "could not resolve cloned path for '$url'"
  exit 0
fi

rel="${newpath#"$ROOT"/}"
label="$(repo_label "$rel" "$label_mode")"

if [[ "$open_after" == "true" ]]; then
  case "$default_target" in
    workspace | tab | split | pane) ;;
    *) default_target="workspace" ;;
  esac
  "$SWITCHBOARD_BIN" open \
    --target "$default_target" \
    --path "$newpath" \
    --origin "${SWITCHBOARD_ORIGIN_PANE_ID:-}" \
    --label "$label" ||
    die "Switchboard could not open $label after cloning." "open $default_target failed for $newpath"
else
  notify "Cloned $label." done
  printf '\n\033[2mpress any key to close\033[0m'
  read -rsn1 _ || true
fi
