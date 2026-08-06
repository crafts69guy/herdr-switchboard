#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "plugin" && "${2:-}" == "pane" && "${3:-}" == "open" ]]; then
  if kill -0 "${SWITCHBOARD_TEST_MENU_PID:?}" 2>/dev/null; then
    printf 'popup already open\n' >&2
    exit 1
  fi
  printf '{"result":{"type":"ok"}}\n'
  exit 0
fi

printf 'unexpected herdr invocation: %s\n' "$*" >&2
exit 1
