#!/usr/bin/env bash
# Git pane: the switcher binary in --git mode — the review menu for the repo this
# pane sits in. Selecting a row execs bin/review.sh over this process, so the
# review tool takes over this pane and quitting it returns to the origin pane.
#
# picker.sh owns prebuilt resolution, the Cargo fallback, and PATH fixup, so this is a
# wrapper rather than a copy of them.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

exec bash "$SCRIPT_DIR/picker.sh" --git
