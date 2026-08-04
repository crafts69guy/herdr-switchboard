#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_DIR="$ROOT/bin"
# shellcheck source=bin/lib.sh
source "$ROOT/bin/lib.sh"

fail() {
  printf 'bootstrap_spec: %s\n' "$*" >&2
  exit 1
}

[[ "$(target_for Darwin arm64)" == "aarch64-apple-darwin" ]] || fail "macOS arm target"
[[ "$(target_for Darwin x86_64)" == "x86_64-apple-darwin" ]] || fail "macOS Intel target"
[[ "$(target_for Linux aarch64)" == "aarch64-unknown-linux-musl" ]] || fail "Linux arm target"
[[ "$(target_for Linux x86_64)" == "x86_64-unknown-linux-musl" ]] || fail "Linux Intel target"
if target_for FreeBSD x86_64 >/dev/null 2>&1; then
  fail "unsupported hosts must not map to a release target"
fi

tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
version="9.8.7"
target="$(host_target)"
asset="herdr-switchboard-v${version}-${target}.tar.gz"
mkdir -p "$tmp/release/payload" "$tmp/plugin"
printf '#!/usr/bin/env bash\nprintf "herdr-switchboard 9.8.7\\n"\n' >"$tmp/release/payload/herdr-switchboard"
chmod 755 "$tmp/release/payload/herdr-switchboard"
tar -C "$tmp/release/payload" -czf "$tmp/release/$asset" herdr-switchboard
hash="$(sha256_file "$tmp/release/$asset")"
printf '%s  %s\n' "$hash" "$asset" >"$tmp/release/SHA256SUMS"

output="$tmp/plugin/target/release/switcher"
SWITCHBOARD_RELEASE_URL="file://$tmp/release" \
  download_prebuilt "$version" "$target" "$output"
[[ -x "$output" ]] || fail "verified release binary was not installed"
[[ "$($output --version)" == "herdr-switchboard 9.8.7" ]] || fail "installed the wrong binary"

printf 'bad  %s\n' "$asset" >"$tmp/release/SHA256SUMS"
if SWITCHBOARD_RELEASE_URL="file://$tmp/release" \
  download_prebuilt "$version" "$target" "$tmp/plugin/rejected"; then
  fail "checksum mismatch must be rejected"
fi
[[ ! -e "$tmp/plugin/rejected" ]] || fail "rejected binary must not be installed"

# A managed checkout with an unavailable release must remain usable through the
# Cargo fallback. Stub Cargo writes the output contract without compiling Rust.
fallback="$tmp/fallback"
tools="$tmp/home/.cargo/bin"
mkdir -p "$fallback" "$tools"
printf 'version = "1.2.3"\n' >"$fallback/herdr-plugin.toml"
printf '[package]\nname = "fixture"\nversion = "1.2.3"\n' >"$fallback/Cargo.toml"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  'manifest=""' \
  'while (($#)); do' \
  '  if [[ "$1" == "--manifest-path" ]]; then manifest="$2"; break; fi' \
  '  shift' \
  'done' \
  'root="$(dirname -- "$manifest")"' \
  'mkdir -p "$root/target/release"' \
  'printf '\''#!/usr/bin/env bash\nprintf "herdr-switchboard 1.2.3\\n"\n'\'' >"$root/target/release/herdr-switchboard"' \
  'chmod 755 "$root/target/release/herdr-switchboard"' >"$tools/cargo"
chmod 755 "$tools/cargo"
fallback_bin="$(
  HOME="$tmp/home" \
    HERDR_PLUGIN_ROOT="$fallback" \
    SWITCHBOARD_RELEASE_URL="file://$tmp/missing" \
    ensure_built
)"
[[ -x "$fallback_bin" ]] || fail "Cargo fallback did not install a versioned binary"
[[ "$($fallback_bin --version)" == "herdr-switchboard 1.2.3" ]] || fail "Cargo fallback binary is wrong"

printf 'bootstrap_spec: ok\n'
