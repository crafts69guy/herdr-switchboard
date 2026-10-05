#!/usr/bin/env bash
# Re-record the README media from demo/tapes/*.tape into docs/media/.
#
#   bash demo/render.sh               every take
#   bash demo/render.sh projects git  just these
#
# Each take gets a freshly built sandbox (demo/sandbox.sh) and its own Herdr
# server, which is stopped afterwards, so takes are reproducible and never touch
# the recording user's Herdr session. Needs vhs, ttyd, ffmpeg, cc, and the font
# named in common.tape.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Tapes that exist for a Screenshot; the GIF vhs records alongside is dropped.
STILLS=(usage)

for tool in vhs ttyd ffmpeg cc; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf 'render: %s is required\n' "$tool" >&2
    exit 1
  }
done

# Build up front: otherwise the first take records the bootstrap splash.
cargo build --release --quiet

if [[ $# -gt 0 ]]; then
  takes=("$@")
else
  takes=()
  for tape in demo/tapes/*.tape; do
    name="$(basename -- "$tape" .tape)"
    [[ "$name" == common ]] || takes+=("$name")
  done
fi

stop_server() { bash demo/enter.sh herdr server stop >/dev/null 2>&1 || true; }
trap stop_server EXIT

mkdir -p docs/media
for name in "${takes[@]}"; do
  [[ -f "demo/tapes/$name.tape" ]] || {
    printf 'render: no tape named %s\n' "$name" >&2
    exit 1
  }
  printf 'render: %s\n' "$name"
  bash demo/sandbox.sh >/dev/null
  vhs "demo/tapes/$name.tape" >/dev/null
  stop_server
  for still in "${STILLS[@]}"; do
    [[ "$name" == "$still" ]] && rm -f "docs/media/$name.gif"
  done
done

ls -lh docs/media
