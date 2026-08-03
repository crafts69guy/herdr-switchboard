#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/herdr-plugin.toml"
CARGO_TOML="$ROOT/Cargo.toml"

fail() {
  printf 'manifest_spec: %s\n' "$*" >&2
  exit 1
}

# First `version = "..."` in a file, from its leading key block.
toml_version() {
  sed -n 's/^version = "\(.*\)"$/\1/p' "$1" | head -n 1
}

# herdr reads herdr-plugin.toml; cargo reads Cargo.toml. A release that bumps one
# and not the other ships a binary whose version disagrees with the manifest.
manifest_version="$(toml_version "$MANIFEST")"
cargo_version="$(toml_version "$CARGO_TOML")"

[ -n "$manifest_version" ] || fail "herdr-plugin.toml declares no version"
[ -n "$cargo_version" ] || fail "Cargo.toml declares no version"
[ "$manifest_version" = "$cargo_version" ] ||
  fail "version mismatch: herdr-plugin.toml $manifest_version, Cargo.toml $cargo_version"

# Every pane entrypoint must launch through HERDR_PLUGIN_ROOT so herdr can start
# it from the originating repo, not the plugin checkout.
assert_rooted_pane_command() {
  local script="$1"
  local expected
  expected="command = [\"bash\", \"-c\", \"exec bash \\\"\$HERDR_PLUGIN_ROOT/bin/$script\\\"\"]"

  grep -Fqx -- "$expected" "$MANIFEST" ||
    fail "$script must be launched through HERDR_PLUGIN_ROOT"
}

assert_rooted_pane_command picker.sh
assert_rooted_pane_command git.sh
assert_rooted_pane_command get.sh
assert_rooted_pane_command changelog.sh
assert_rooted_pane_command update-plugin.sh

# Every declared action dispatches through bin/action.sh. Settings is deliberately
# absent: it is an in-picker overlay (⌥,), not a herdr action.
for action in menu git get changelog update-plugin open-workspace open-tab open-split; do
  grep -Fq "id = \"$action\"" "$MANIFEST" || fail "action '$action' is not declared"
done

# The git action opens its own pane, not the picker: the menu must not pay for
# loading agents, workspaces, and repos on the way to a review.
grep -Eq '^  git\) entrypoint="git" ;;$' "$ROOT/bin/action.sh" ||
  fail "the git action must open the dedicated git pane"

# The pane script must resolve from an unrelated working directory.
foreign_cwd="$(mktemp -d)"
trap 'rm -rf "$foreign_cwd"' EXIT
(
  cd "$foreign_cwd"
  HERDR_PLUGIN_ROOT="$ROOT" bash -c 'test -f "$HERDR_PLUGIN_ROOT/bin/picker.sh"'
) || fail "pane command could not resolve the plugin script from a foreign cwd"

# Every bin script must be syntactically valid bash.
for script in "$ROOT"/bin/*.sh; do
  bash -n "$script" || fail "syntax error in $script"
done

printf 'manifest_spec: ok\n'
