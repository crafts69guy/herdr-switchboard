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

# Picker panes may pass a mode flag, but must still resolve through the plugin root.
grep -Fq 'exec bash \"$HERDR_PLUGIN_ROOT/bin/picker.sh\"' "$MANIFEST" ||
  fail "picker panes must be launched through HERDR_PLUGIN_ROOT"
assert_rooted_pane_command git.sh
assert_rooted_pane_command get.sh
assert_rooted_pane_command changelog.sh
assert_rooted_pane_command update-plugin.sh

# Every public surface has a direct action; the menu is an additional route, not
# a replacement for the hot picker bindings.
for action in menu projects agents commands ports settings git clone changelog update open-workspace open-tab open-split; do
  grep -Fq "id = \"$action\"" "$MANIFEST" || fail "action '$action' is not declared"
done

# AI Agents is a compact centered popup, not a mostly empty full-screen overlay.
grep -Eq '^  agents\) placement=\(--placement popup --width 100 --height 26\) ;;$' "$ROOT/bin/action.sh" ||
  fail "the agents action must open a compact popup"

# The git action opens its own pane, not the picker: the menu must not pay for
# loading agents, workspaces, and repos on the way to a review.
grep -Eq '^  git\) entrypoint="git" ;;$' "$ROOT/bin/action.sh" ||
  fail "the git action must open the dedicated git pane"

# Central handoff must close the menu before opening its target; otherwise the
# new overlay becomes a child of the popup it is replacing.
close_line="$(grep -n 'plugin pane close.*SWITCHBOARD_HANDOFF_PANE_ID' "$ROOT/bin/action.sh" | cut -d: -f1)"
open_line="$(grep -n 'if ! "${command\[@\]}"' "$ROOT/bin/action.sh" | cut -d: -f1)"
[[ -n "$close_line" && -n "$open_line" && "$close_line" -lt "$open_line" ]] ||
  fail "central menu handoff must close before target open"

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
