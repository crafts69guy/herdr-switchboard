#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/lib.sh
source "$SCRIPT_DIR/lib.sh"

ACTION_ID="${HERDR_PLUGIN_ACTION_ID:-}"
[[ -n "$ACTION_ID" ]] || die "Switchboard could not determine which action to run." "HERDR_PLUGIN_ACTION_ID is not set"

# Map each action to the overlay pane it opens and, for the hot-path actions,
# the Enter target the picker should force.
entrypoint=""
force_target=""
case "$ACTION_ID" in
  menu) entrypoint="menu" ;;
  projects) entrypoint="projects" ;;
  agents) entrypoint="agents" ;;
  commands) entrypoint="commands" ;;
  ports) entrypoint="ports" ;;
  settings) entrypoint="settings" ;;
  git) entrypoint="git" ;;
  open-workspace) entrypoint="projects"; force_target="workspace" ;;
  open-tab) entrypoint="projects"; force_target="tab" ;;
  open-split) entrypoint="projects"; force_target="split" ;;
  clone) entrypoint="clone" ;;
  changelog) entrypoint="changelog" ;;
  update) entrypoint="update" ;;
  *) die "Switchboard received an unsupported action. Check plugin logs." "unknown plugin action '$ACTION_ID'" ;;
esac

case "$entrypoint" in
  projects|git|clone) command -v ghq >/dev/null 2>&1 || die "ghq is required — brew install ghq." "ghq not found on PATH" ;;
esac

pane_id="${SWITCHBOARD_ORIGIN_PANE_ID:-$(context_pane_id)}"
cwd="${SWITCHBOARD_ORIGIN_CWD:-}"

wait_for_handoff_parent() {
  local parent_pid="${SWITCHBOARD_HANDOFF_PARENT_PID:-}"
  local attempt
  [[ -n "$parent_pid" ]] || return 0
  [[ "$parent_pid" =~ ^[0-9]+$ ]] ||
    die "Switchboard could not hand off from its menu." "invalid menu handoff parent pid '$parent_pid'"

  for ((attempt = 0; attempt < 200; attempt++)); do
    if ! kill -0 "$parent_pid" 2>/dev/null; then
      return 0
    fi
    sleep 0.01
  done
  die "Switchboard could not hand off from its menu." "menu process $parent_pid did not exit"
}

# The picker is a full overlay. The changelog is a fixed-size popup — it scrolls, so
# height is comfort rather than a fit. (Settings is not a pane: it is an in-picker
# floating overlay, opened with ⌥, from the switcher.)
placement=(--placement overlay)
case "$entrypoint" in
  menu) placement=(--placement popup --width 76 --height 24) ;;
  agents) placement=(--placement popup --width 100 --height 26) ;;
  settings) placement=(--placement popup --width 100 --height 32) ;;
  changelog) placement=(--placement popup --width 88 --height 28) ;;
esac

command=("$(herdr_bin)" plugin pane open --plugin switchboard --entrypoint "$entrypoint" "${placement[@]}")
if [[ -n "$cwd" ]] || cwd="$(active_cwd "$pane_id")"; then
  command+=(--cwd "$cwd" --env "SWITCHBOARD_ORIGIN_CWD=$cwd")
fi
if [[ -n "$pane_id" ]]; then
  command+=(--env "SWITCHBOARD_ORIGIN_PANE_ID=$pane_id")
fi
if [[ -n "$force_target" ]]; then
  command+=(--env "SWITCHBOARD_FORCE_TARGET=$force_target")
fi
if [[ -n "${SWITCHBOARD_HANDOFF_PANE_ID:-}" ]]; then
  "$(herdr_bin)" plugin pane close "$SWITCHBOARD_HANDOFF_PANE_ID" >/dev/null 2>&1 ||
    die "Switchboard could not close its menu pane." "menu handoff close failed"
fi
wait_for_handoff_parent
if ! "${command[@]}"; then
  die "Switchboard could not open the $entrypoint pane. Check plugin logs." "herdr failed to open the switchboard $entrypoint pane"
fi
