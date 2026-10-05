#!/usr/bin/env bash
# Dress the sandbox Herdr session before a take. A tape runs this, hidden, from
# the first pane: it opens a few workspaces with stand-in agents so Projects has
# live rows to show, then hands the screen back to a clean prompt.
#
#   stage.sh            workspaces + agents (the default set)
#   stage.sh --split    also split the current tab, for Zen
set -euo pipefail

SRC="$HOME/src/github.com"

pane_id() { sed -n 's/.*"pane_id":"\([^"]*\)".*/\1/p' | head -n 1; }

# workspace <dir> <label> [agent] — a background workspace, optionally running
# a stand-in agent in its root pane.
workspace() {
  local dir="$1" label="$2" agent="${3:-}" pane
  pane="$(herdr workspace create --cwd "$dir" --label "$label" --no-focus | pane_id)"
  if [[ -n "$agent" && -n "$pane" ]]; then
    sleep 0.4
    herdr pane run "$pane" "$agent" >/dev/null
  fi
}

workspace "$SRC/northwind/web-dashboard" web-dashboard codex
workspace "$SRC/northwind/billing-worker" billing-worker claude
workspace "$HOME/.herdr/worktrees/web-dashboard/dark-mode" dark-mode

# The current tab gets an agent beside the shell, the way a real session looks.
agent_pane="$(herdr pane split --current --direction right --cwd "$PWD" --no-focus | pane_id)"
sleep 0.4
herdr pane run "$agent_pane" claude >/dev/null

if [[ "${1:-}" == "--split" ]]; then
  herdr pane split --current --direction down --cwd "$PWD" --no-focus >/dev/null
fi

# Give Herdr a beat to recognise the stand-ins before the first frame.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [[ "$(herdr agent list | grep -o '"pane_id"' | wc -l)" -ge 3 ]] && break
  sleep 0.5
done
clear
