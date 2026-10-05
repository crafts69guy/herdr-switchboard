#!/usr/bin/env bash
# Attach to the demo sandbox's own Herdr server. Every variable that could point
# Herdr, Switchboard, or a shell back at the recording machine is dropped by
# `env -i`; a session already running Herdr is left alone because the socket
# lives under the sandbox $HOME.
set -euo pipefail

SANDBOX="${SWB_DEMO_ROOT:-/tmp/swb-demo}"
[[ -d "$SANDBOX/home" ]] || {
  printf 'enter: no sandbox at %s — run demo/sandbox.sh first\n' "$SANDBOX" >&2
  exit 1
}
SANDBOX="$(cd -- "$SANDBOX" && pwd -P)"
DEMO_HOME="$SANDBOX/home"

# Herdr names the first workspace after the directory it starts in.
cd "${SWB_DEMO_CWD:-$DEMO_HOME/src/github.com/northwind/api-gateway}"

exec env -i \
  HOME="$DEMO_HOME" \
  XDG_CONFIG_HOME="$DEMO_HOME/.config" \
  XDG_STATE_HOME="$DEMO_HOME/.local/state" \
  XDG_DATA_HOME="$DEMO_HOME/.local/share" \
  PATH="$SANDBOX/bin:$PATH" \
  SHELL=/bin/bash \
  TERM="${TERM:-xterm-256color}" \
  COLORTERM=truecolor \
  LANG=en_US.UTF-8 \
  "$@"
