#!/usr/bin/env bash
# Keep every Update entrypoint on the same hosted Rust surface.
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$SCRIPT_DIR/picker.sh" --update
