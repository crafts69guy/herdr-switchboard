#!/usr/bin/env bash
# Install the newest tagged version of this plugin over the installed one.
#
# The only flow here that writes outside the plugin's own state. It runs in its own
# overlay pane, never inside the picker: `herdr plugin install` rewrites the checkout
# that holds the very binary the picker is executing.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/lib.sh
source "$SCRIPT_DIR/lib.sh"

REPO="crafts69guy/herdr-switchboard"

pause() {
  printf '\n\033[2mpress any key to close\033[0m'
  read -rsn1 _ || true
}

# Where herdr says this plugin came from.
#
# **Fails closed.** Only an unambiguous `"source":{"kind":"github"…}` counts as a managed
# install; an empty answer, an unreadable one, or a shape we do not recognise all read as
# "not managed" and stop the update. The alternative failure — mistaking a linked
# development checkout for a managed one — would have `herdr plugin install` overwrite
# someone's working tree with a release tarball. Refusing to update is a rounding error
# next to that, so every uncertainty resolves that way. `jq` is deliberately not used:
# it is optional for this plugin, and the launcher path must not depend on it.
source_kind() {
  local json
  json="$("$(herdr_bin)" plugin list --plugin switchboard --json 2>/dev/null)" || return 1
  # Anchored to the source object, so an unrelated "kind" elsewhere cannot answer for it.
  printf '%s' "$json" |
    grep -o '"source":{"kind":"[^"]*"' |
    head -n 1 |
    sed 's/.*"kind":"//; s/"$//'
}

plugin_field() {
  "$(herdr_bin)" plugin list --plugin switchboard --json 2>/dev/null | json_string_value "$1"
}

kind="$(source_kind || true)"

if [[ "$kind" != "github" ]]; then
  root="$(plugin_field plugin_root || true)"
  printf '\033[1mSwitchboard is not a managed install.\033[0m\n\n'
  if [[ "$kind" == "local" ]]; then
    printf 'It is linked from a checkout you control:\n\n  \033[36m%s\033[0m\n\n' "${root:-unknown}"
    printf 'Updating would overwrite that working tree, so this stops here. Pull it\n'
    printf 'yourself, and relink to let herdr re-read the manifest:\n\n'
    printf '  \033[2mgit -C %s pull\033[0m\n' "${root:-<checkout>}"
    printf '  \033[2mcargo build --release --manifest-path %s/Cargo.toml\033[0m\n' "${root:-<checkout>}"
  printf '  \033[2mherdr plugin unlink switchboard && herdr plugin link %s\033[0m\n' "${root:-<checkout>}"
  else
    printf 'herdr reports its source as \033[33m%s\033[0m, which this cannot update safely.\n' "${kind:-unreadable}"
    printf 'Install it from GitHub to get updates:\n\n  \033[2mherdr plugin install %s\033[0m\n' "$REPO"
  fi
  pause
  exit 0
fi

printf '\033[1mUpdating Switchboard\033[0m from \033[36m%s\033[0m\n\n' "$REPO"

if ! "$(herdr_bin)" plugin install "$REPO" --yes; then
  die "Switchboard could not be updated. Check the pane for details." "herdr plugin install $REPO failed"
fi

# Prepare the versioned release binary while this update pane is already open. The
# newly installed picker owns download/checksum/fallback policy; invoking its
# prepare-only mode avoids duplicating that contract here.
root="$(plugin_field plugin_root || true)"
if [[ -n "$root" && -f "$root/Cargo.toml" ]]; then
  if ! HERDR_PLUGIN_ROOT="$root" bash "$root/bin/picker.sh" --prepare; then
    log "binary preparation failed; the next switcher open will retry"
  else
    SWITCHBOARD_BIN="$(HERDR_PLUGIN_ROOT="$root" ensure_built)"
    export SWITCHBOARD_BIN
  fi
fi

version="$(plugin_field version || true)"
notify "Updated to ${version:-the latest version}." done
printf '\n\033[32mUpdated.\033[0m Open the changelog to see what changed.\n'
pause
