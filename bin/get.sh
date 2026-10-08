#!/usr/bin/env bash
# Clone uses the same hosted Projects form as alt-l.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$SCRIPT_DIR/picker.sh" --clone
