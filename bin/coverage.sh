#!/usr/bin/env bash
set -euo pipefail

# Line coverage over src/, gated at MINIMUM.
#
# Kept out of bin/check.sh on purpose: coverage needs its own instrumented
# build, which turns a ~5s gate into a ~90s one. check.sh stays the fast loop
# you run constantly; this is the one CI enforces on every pull request.
#
# Usage:
#   bash bin/coverage.sh              # report + enforce the threshold
#   bash bin/coverage.sh --html       # also write an annotated HTML report
#   bash bin/coverage.sh --open       # ... and open it
#   bash bin/coverage.sh --json PATH  # machine-readable summary
#
# Nothing is excluded from the denominator. Some code genuinely cannot be
# covered by a unit test — a mode's `main`, `surface::run` claiming a real
# terminal, `SystemProbe` reading real sockets — and it still counts. An
# exclusion list is a second thing to argue about and a place for the number to
# quietly stop meaning what it says.

MINIMUM=90

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail() {
  printf 'coverage: %s\n' "$*" >&2
  exit 1
}

html=0
open=0
json=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --html) html=1 ;;
    --open) html=1; open=1 ;;
    --json) shift; json="${1:-}"; [[ -n "$json" ]] || fail "--json needs a path" ;;
    *) fail "unknown argument: $1" ;;
  esac
  shift
done

# `cargo llvm-cov` needs llvm-tools-preview, which only a rustup toolchain
# carries. A Homebrew (or distro) rust can be first on PATH while a perfectly
# good rustup toolchain sits beside it, so prefer running under rustup when it
# has the tools and fall back to a bare `cargo` for CI images that ship them.
cargo_cmd=(cargo)
if command -v rustup >/dev/null 2>&1; then
  toolchain="$(rustup show active-toolchain 2>/dev/null | awk '{ print $1 }')"
  if [[ -n "$toolchain" ]] &&
    [[ -n "$(find "${HOME}/.rustup/toolchains/${toolchain}/lib/rustlib" \
      -name 'llvm-profdata' -print -quit 2>/dev/null)" ]]; then
    cargo_cmd=(rustup run "$toolchain" cargo)
  fi
fi

command -v cargo-llvm-cov >/dev/null 2>&1 ||
  "${cargo_cmd[@]}" llvm-cov --version >/dev/null 2>&1 ||
  fail "cargo-llvm-cov is not installed. Run:
    rustup component add llvm-tools-preview
    cargo install cargo-llvm-cov --locked"

# No `--branch`: it needs `-Z coverage-options=branch`, which is nightly only,
# and this repository builds on stable everywhere. Line coverage is what the
# gate is stated in, so the two agree.

# One instrumented run feeds every output below, so the percentage a human
# reads and the percentage the gate enforces can never come from two builds.
"${cargo_cmd[@]}" llvm-cov clean --workspace
"${cargo_cmd[@]}" llvm-cov --no-report

if [[ -n "$json" ]]; then
  "${cargo_cmd[@]}" llvm-cov report --json --summary-only --output-path "$json"
  printf 'coverage: summary written to %s\n' "$json"
fi

if [[ "$html" -eq 1 ]]; then
  "${cargo_cmd[@]}" llvm-cov report --html
  printf 'coverage: html report at %s\n' "$ROOT/target/llvm-cov/html/index.html"
  [[ "$open" -eq 1 ]] && command -v open >/dev/null 2>&1 &&
    open "$ROOT/target/llvm-cov/html/index.html"
fi

# Per-file table first so a failure names what to go and test, then the gate.
"${cargo_cmd[@]}" llvm-cov report
"${cargo_cmd[@]}" llvm-cov report --fail-under-lines "$MINIMUM" >/dev/null ||
  fail "line coverage is below ${MINIMUM}%"

printf 'coverage: ok (line coverage >= %s%%)\n' "$MINIMUM"
