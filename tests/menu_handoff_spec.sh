#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

fail() {
  printf 'menu_handoff_spec: %s\n' "$*" >&2
  exit 1
}

# A popup cannot open while the Central Menu popup still owns Herdr's modal
# popup slot. Model the menu process with a short-lived parent and make the
# Herdr stub reject any open attempted before that parent exits.
sleep 1 &
menu_pid=$!

HERDR_BIN_PATH="$ROOT/tests/menu_handoff_herdr_stub.sh" \
  HERDR_PLUGIN_ACTION_ID=agents \
  HERDR_PLUGIN_ROOT="$ROOT" \
  SWITCHBOARD_ORIGIN_PANE_ID=w1:p1 \
  SWITCHBOARD_ORIGIN_CWD="$ROOT" \
  SWITCHBOARD_HANDOFF_PARENT_PID="$menu_pid" \
  SWITCHBOARD_TEST_MENU_PID="$menu_pid" \
  bash "$ROOT/bin/action.sh" ||
  fail "the target popup was opened before the Central Menu exited"

wait "$menu_pid"
printf 'menu_handoff_spec: ok\n'
