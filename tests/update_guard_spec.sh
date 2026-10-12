#!/usr/bin/env bash
# Guard cases live beside the Rust updater; this checks the public Bash handoff.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
cp "$ROOT/bin/update-plugin.sh" "$tmp/bin/"
cat >"$tmp/bin/picker.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@"
STUB
output="$(cd / && bash "$tmp/bin/update-plugin.sh")"
[[ "$output" == "--update" ]] || {
  printf 'update_guard_spec: updater must launch picker --update\n' >&2
  exit 1
}
printf 'update_guard_spec: ok\n'
