#!/usr/bin/env bash
# The guard in bin/update-plugin.sh decides whether `herdr plugin install` may rewrite
# the plugin's checkout. Get it wrong in the permissive direction and it overwrites a
# contributor's working tree, so every case here asserts it fails closed: only an
# unambiguous github source installs, everything else refuses.
#
# `herdr` is stubbed through HERDR_BIN_PATH, the seam bin/lib.sh already reads, so the
# real herdr and the real repo are never touched.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

fail() {
  printf 'update_guard_spec: %s\n' "$*" >&2
  exit 1
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# A herdr that answers `plugin list` with $FAKE_JSON and records `plugin install`.
cat >"$tmp/herdr" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "plugin" && "${2:-}" == "list" ]]; then
  printf '%s' "$FAKE_JSON"
  exit 0
fi
if [[ "${1:-}" == "plugin" && "${2:-}" == "install" ]]; then
  printf 'install %s\n' "${3:-}" >>"$INSTALL_LOG"
  exit 0
fi
exit 0
STUB
chmod +x "$tmp/herdr"

# Runs the guard against one `plugin list` payload; echoes whether install was reached.
run_with() {
  local json="$1"
  : >"$tmp/install.log"
  FAKE_JSON="$json" \
    INSTALL_LOG="$tmp/install.log" \
    HERDR_BIN_PATH="$tmp/herdr" \
    NOTIFICATIONS_ENABLED="false" \
    bash "$ROOT/bin/update-plugin.sh" </dev/null >"$tmp/out.txt" 2>&1 || true
  if [[ -s "$tmp/install.log" ]]; then printf 'installed\n'; else printf 'refused\n'; fi
}

assert_refused() {
  local label="$1" json="$2" result
  result="$(run_with "$json")"
  # Single quotes: backticks in a double-quoted string are command substitution, and
  # this file names the very command it must never let run.
  [[ "$result" == "refused" ]] ||
    fail "$label: expected refusal, but the install ran"
}

# A linked development checkout: the case that must never be installed over.
assert_refused "local source" \
  '{"result":{"plugins":[{"plugin_root":"/home/dev/herdr-switchboard","source":{"kind":"local"}}]}}'

# Nothing readable at all.
assert_refused "empty response" ''

# Not JSON we understand.
assert_refused "garbage response" 'not json at all'

# No source object.
assert_refused "missing source" \
  '{"result":{"plugins":[{"plugin_root":"/x","version":"0.5.0"}]}}'

# A "kind" belonging to something else must not answer for the source. If herdr ever
# adds such a field, this refuses rather than guessing.
assert_refused "unrelated kind field first" \
  '{"result":{"plugins":[{"agent_session":{"kind":"id"},"source":{"kind":"local"}}]}}'

# Pretty-printed output does not match the anchored pattern — and unreadable means stop.
assert_refused "pretty-printed source" \
  '{"result": {"plugins": [{"source": {"kind": "github"}}]}}'

# The one case that proceeds: an unambiguous managed install.
github='{"result":{"plugins":[{"plugin_root":"'"$tmp/nocargo"'","version":"0.5.0","source":{"kind":"github","owner":"crafts69guy","repo":"herdr-switchboard","managed_path":"'"$tmp/nocargo"'"}}]}}'
result="$(run_with "$github")"
[[ "$result" == "installed" ]] ||
  fail "github source: expected an install, but the guard refused"
grep -Fq 'install crafts69guy/herdr-switchboard' "$tmp/install.log" ||
  fail "github source: installed the wrong repo: $(cat "$tmp/install.log")"

printf 'update_guard_spec: ok\n'
