#!/usr/bin/env bash
# Picker entrypoint: launch the herdr-switchboard TUI (Rust). The binary is
# built on demand the first time — a herdr overlay pane hosts the whole thing,
# so agents/workspaces/repos are read live and the accept key opens/focuses.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/lib.sh
source "$SCRIPT_DIR/lib.sh"

PLUGIN_ROOT="${HERDR_PLUGIN_ROOT:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
export HERDR_PLUGIN_ROOT="$PLUGIN_ROOT"

BIN="$(ensure_built)"

if [[ "${1:-}" == "--prepare" ]]; then
  exit 0
fi

exec "$BIN" "$@"
