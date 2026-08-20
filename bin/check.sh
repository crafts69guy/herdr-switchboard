#!/usr/bin/env bash
set -euo pipefail

# One full verification interface for local sessions, CI, and releases. Keep the
# individual specs focused; add new repository-wide gates here instead of copying
# command lists into agent docs and workflows.
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo fmt --check
cargo clippy --quiet --all-targets -- -D warnings
cargo test --quiet
bash tests/manifest_spec.sh
bash tests/update_guard_spec.sh
bash tests/bootstrap_spec.sh
bash tests/menu_handoff_spec.sh
bash tests/docs_spec.sh

printf 'check: ok\n'
