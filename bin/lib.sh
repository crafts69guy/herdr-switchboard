#!/usr/bin/env bash

# Shared helpers for the herdr-switchboard plugin. Callers enable strict mode
# (set -euo pipefail) before sourcing this file. The reusable pieces here —
# config parsing, pane-context resolution, theme colors, notifications — follow
# the same contract as the sibling git-hub plugin so the two feel like one
# toolkit.

NOTIFICATIONS_ENABLED="true"
NOTIFICATION_POSITION="top-right"
# auto = honour each call's per-event sound; none/done/request force one for every toast.
NOTIFICATION_SOUND="auto"

log() {
  printf 'herdr-switchboard: %s\n' "$*" >&2
}

die() {
  local message="$1"
  local detail="${2:-$message}"

  log "$detail"
  notify "$message" request
  exit 1
}

herdr_bin() {
  printf '%s\n' "${HERDR_BIN_PATH:-herdr}"
}

# Draw the first-run bootstrap without needing the Rust binary it is preparing.
# Output goes to stderr because callers capture ensure_built's stdout as the path.
bootstrap_frame() {
  local label="$1"
  local frame="$2"
  local eye="o.o"
  local left_paw="_|"
  local right_paw="|_"
  ((frame % 4 == 1)) && left_paw="/|"
  ((frame % 4 == 1)) && right_paw="|\\"
  ((frame % 8 == 5)) && eye="o.-"

  local cols="${COLUMNS:-80}"
  local rows="${LINES:-24}"
  if command -v tput >/dev/null 2>&1; then
    cols="$(tput cols 2>/dev/null || printf '80')"
    rows="$(tput lines 2>/dev/null || printf '24')"
  fi
  local pad_x=$(((cols - 32) / 2))
  local pad_y=$(((rows - 14) / 2))
  ((pad_x < 0)) && pad_x=0
  ((pad_y < 0)) && pad_y=0
  local left
  printf -v left '%*s' "$pad_x" ''

  printf '\033[2J\033[H%*s' "$pad_y" '' >&2
  if ((cols < 40 || rows < 14)); then
    printf '%s\033[97;1m /\\_/\\\033[0m\n' "$left" >&2
    printf '%s\033[97m( \033[92;1m%s\033[97m )\033[0m\n' "$left" "$eye" >&2
    printf '%s\033[97m > ^ <\033[0m\n' "$left" >&2
  else
    printf '%s\033[92m*   \033[97;1m/\\_____/\\\033[0m\n' "$left" >&2
    printf '%s\033[97;1m   /         \\\033[0m\n' "$left" >&2
    printf '%s\033[97m  |   \033[92;1m%-5s\033[97m   |\033[0m\n' "$left" "${eye//./   }" >&2
    printf '%s\033[97m  |     ^     |\033[0m\n' "$left" >&2
    printf '%s\033[97m   \\   ---   /\033[0m\n' "$left" >&2
    printf '%s\033[97m    |_______|\033[0m\n' "$left" >&2
    printf '%s\033[97m   / %s  tap tap  %s \\\033[0m\n' "$left" "$left_paw" "$right_paw" >&2
    printf '%s\033[97m .--\033[92;1m[=]\033[97m--[ ][ ][ ]--\033[92;1m[=]\033[97m--.\033[0m\n' "$left" >&2
    printf '%s\033[36m '\''-----------------------'\''\033[0m\n' "$left" >&2
  fi
  printf '\n%s\033[1m%s\033[0m\n' "$left" "$label" >&2
  printf '%s\033[2mEsc or Ctrl-C to cancel\033[0m' "$left" >&2
}

# Run a quiet bootstrap job while the cat animates. On failure the caller can
# show the captured log or try a fallback without build/download noise painting
# over the same terminal cells.
run_with_splash() {
  local label="$1"
  local logfile="$2"
  shift 2

  if [[ ! -t 2 || ! -r /dev/tty ]]; then
    "$@" >"$logfile" 2>&1
    return
  fi

  "$@" >"$logfile" 2>&1 &
  local child=$!
  local frame=0
  local key=""
  local interrupted="false"
  trap 'interrupted="true"; kill "$child" 2>/dev/null || true' INT TERM
  printf '\033[?25l' >&2

  while kill -0 "$child" 2>/dev/null; do
    bootstrap_frame "$label" "$frame"
    frame=$((frame + 1))
    key=""
    if read -rsn1 -t 0.08 key </dev/tty 2>/dev/null && [[ "$key" == $'\033' ]]; then
      interrupted="true"
      kill "$child" 2>/dev/null || true
    fi
    [[ "$interrupted" == "true" ]] && break
  done

  local status=0
  if wait "$child"; then
    status=0
  else
    status=$?
  fi
  [[ "$interrupted" == "true" ]] && status=130
  trap - INT TERM
  printf '\033[?25h\033[2J\033[H' >&2
  return "$status"
}

plugin_version() {
  sed -n 's/^version = "\([^"]*\)"$/\1/p' "$1/herdr-plugin.toml" | head -n 1
}

target_for() {
  case "$1:$2" in
    Darwin:arm64 | Darwin:aarch64) printf 'aarch64-apple-darwin\n' ;;
    Darwin:x86_64) printf 'x86_64-apple-darwin\n' ;;
    Linux:arm64 | Linux:aarch64) printf 'aarch64-unknown-linux-musl\n' ;;
    Linux:x86_64 | Linux:amd64) printf 'x86_64-unknown-linux-musl\n' ;;
    *) return 1 ;;
  esac
}

host_target() {
  target_for "$(uname -s)" "$(uname -m)"
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    return 1
  fi
}

binary_version_matches() {
  local actual
  actual="$("$1" --version 2>/dev/null)" || return 1
  [[ "$actual" == "herdr-switchboard $2" ]]
}

download_prebuilt() (
  local version="$1"
  local target="$2"
  local output="$3"
  command -v curl >/dev/null 2>&1 || return 1
  command -v tar >/dev/null 2>&1 || return 1

  local asset="herdr-switchboard-v${version}-${target}.tar.gz"
  local base="${SWITCHBOARD_RELEASE_URL:-https://github.com/crafts69guy/herdr-switchboard/releases/download/v${version}}"
  local tmp
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/herdr-switchboard.XXXXXX")"
  trap 'rm -rf -- "$tmp"' EXIT

  curl -fsSL "$base/$asset" -o "$tmp/$asset"
  curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
  local expected actual
  expected="$(awk -v asset="$asset" '$2 == asset { print $1; exit }' "$tmp/SHA256SUMS")"
  [[ -n "$expected" ]] || return 1
  actual="$(sha256_file "$tmp/$asset")" || return 1
  [[ "$actual" == "$expected" ]] || return 1

  mkdir -p "$tmp/unpack" "$(dirname -- "$output")"
  tar -xzf "$tmp/$asset" -C "$tmp/unpack"
  [[ -f "$tmp/unpack/herdr-switchboard" ]] || return 1
  chmod 755 "$tmp/unpack/herdr-switchboard"
  binary_version_matches "$tmp/unpack/herdr-switchboard" "$version" || return 1
  cp "$tmp/unpack/herdr-switchboard" "$output.tmp.$$"
  chmod 755 "$output.tmp.$$"
  mv -f "$output.tmp.$$" "$output"
)

# Resolve a version-matched prebuilt switcher, falling back to a local Cargo
# build. A linked development checkout deliberately skips release downloads so
# its binary always comes from the source the contributor is editing.
ensure_built() (
  local root="${HERDR_PLUGIN_ROOT:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
  local version target="" bin log managed="true"
  version="$(plugin_version "$root")"
  [[ -n "$version" ]] || die "Switchboard's plugin version is unreadable." "missing version in herdr-plugin.toml"
  target="$(host_target || true)"
  if [[ -d "$root/.git" ]]; then
    managed="false"
    bin="$root/target/release/herdr-switchboard"
  else
    bin="$root/target/release/herdr-switchboard-v${version}-${target:-local}"
  fi
  export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
  if [[ -x "$bin" ]] && binary_version_matches "$bin" "$version"; then
    printf '%s\n' "$bin"
    return
  fi

  mkdir -p "$root/target/release"
  log="$(mktemp "${TMPDIR:-/tmp}/herdr-switchboard-bootstrap.XXXXXX")"
  trap 'rm -f -- "$log"' EXIT

  if [[ -n "$target" && "$managed" == "true" ]]; then
    if run_with_splash "Fetching Switchboard for ${target}…" "$log" \
      download_prebuilt "$version" "$target" "$bin"; then
      printf '%s\n' "$bin"
      return
    else
      local download_status=$?
      [[ "$download_status" -eq 130 ]] && return 130
    fi
  fi

  command -v cargo >/dev/null 2>&1 || {
    [[ -s "$log" ]] && sed 's/^/  /' "$log" >&2
    die "Switchboard needs a release binary or Rust (cargo). Check your network, or install Rust." "prebuilt unavailable and cargo not found"
  }
  if run_with_splash "Building Switchboard locally…" "$log" \
    cargo build --release --manifest-path "$root/Cargo.toml"; then
    :
  else
    local build_status=$?
    [[ "$build_status" -eq 130 ]] && return 130
    sed 's/^/  /' "$log" >&2
    die "Switchboard could not build the switcher. Check the pane for cargo errors." "cargo build failed"
  fi
  binary_version_matches "$root/target/release/herdr-switchboard" "$version" ||
    die "The locally built switcher has the wrong version." "cargo output version does not match herdr-plugin.toml"
  if [[ "$bin" != "$root/target/release/herdr-switchboard" ]]; then
    cp "$root/target/release/herdr-switchboard" "$bin.tmp.$$"
    chmod 755 "$bin.tmp.$$"
    mv -f "$bin.tmp.$$" "$bin"
  fi
  printf '%s\n' "$bin"
)

# Guard the review viewer. tuicr is the review TUI bin/review.sh launches; it is a
# separate dependency, so fail with a clear install hint rather than a bare
# "command not found".
#
# This writes nothing to tuicr's config: the theme is generated and installed by
# hue-theme, which owns a whole `~/.config/tuicr/themes/hue-*.toml` file of its own.
# tuicr exits 2 on a theme it cannot fully resolve, so a half-written one here would
# break every review, not just its colours.
ensure_tuicr() {
  command -v tuicr >/dev/null 2>&1 ||
    die "tuicr is required for review — brew install tuicr." "tuicr not found on PATH"
}

# Load notification policy through the typed Rust config reader.
# Keep defaults active until both values validate so malformed notification
# config can still be reported to the user.
configure_notifications() {
  local binary="$1"
  local allow_invalid="${2:-false}"
  local enabled position sound

  enabled="$("$binary" config get notifications true)"
  position="$("$binary" config get notification_position top-right)"
  sound="$("$binary" config get notification_sound auto)"

  case "$enabled" in
    true | false) ;;
    *)
      if [[ "$allow_invalid" == "true" ]]; then
        log "invalid notifications '$enabled'; using true until Settings repairs it"
        enabled=true
      else
        die "Invalid notifications setting. Use true or false." "invalid notifications '$enabled'"
      fi
      ;;
  esac

  case "$position" in
    '' | top-left | top-right | bottom-left | bottom-right) ;;
    *)
      if [[ "$allow_invalid" == "true" ]]; then
        log "invalid notification_position '$position'; using top-right until Settings repairs it"
        position=top-right
      else
        die \
          "Invalid notification_position setting. Check the plugin config." \
          "invalid notification_position '$position'"
      fi
      ;;
  esac

  case "$sound" in
    auto | none | done | request) ;;
    *)
      if [[ "$allow_invalid" == "true" ]]; then
        log "invalid notification_sound '$sound'; using auto until Settings repairs it"
        sound=auto
      else
        die \
          "Invalid notification_sound setting. Use auto, none, done, or request." \
          "invalid notification_sound '$sound'"
      fi
      ;;
  esac

  NOTIFICATIONS_ENABLED="$enabled"
  NOTIFICATION_POSITION="$position"
  NOTIFICATION_SOUND="$sound"
}

# Herdr read commands emit JSON. Paths and IDs are plain strings in practice;
# this small extractor avoids adding jq as a launcher dependency.
json_string_value() {
  local key="$1"
  awk -v wanted="$key" '
    {
      marker = "\"" wanted "\""
      pos = index($0, marker)
      if (!pos) next
      value = substr($0, pos + length(marker))
      sub(/^[[:space:]]*:[[:space:]]*"/, "", value)
      if (value == $0) next
      sub(/".*/, "", value)
      gsub(/\\\//, "/", value)
      gsub(/\\\\/, "\\", value)
      print value
      exit
    }
  '
}

json_bool_value() {
  local key="$1"
  awk -v wanted="$key" '
    {
      marker = "\"" wanted "\""
      pos = index($0, marker)
      if (!pos) next
      value = substr($0, pos + length(marker))
      sub(/^[[:space:]]*:[[:space:]]*/, "", value)
      if (value ~ /^true/) print "true"
      if (value ~ /^false/) print "false"
      exit
    }
  '
}

context_value() {
  local key="$1"
  if [[ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ]]; then
    printf '%s\n' "$HERDR_PLUGIN_CONTEXT_JSON" | json_string_value "$key"
  fi
}

context_pane_id() {
  local pane_id="${HERDR_PANE_ID:-${HERDR_ACTIVE_PANE_ID:-}}"
  if [[ -z "$pane_id" ]]; then
    pane_id="$(context_value pane_id)"
  fi
  printf '%s\n' "$pane_id"
}

pane_details() {
  local pane_id="$1"
  local herdr
  herdr="$(herdr_bin)"

  "$herdr" pane get "$pane_id" 2>/dev/null ||
    "$herdr" pane process-info --pane "$pane_id" 2>/dev/null
}

current_pane_details() {
  "$(herdr_bin)" pane current 2>/dev/null
}

pane_cwd() {
  local pane_id="$1"
  local details cwd
  details="$(pane_details "$pane_id")" || return 1
  cwd="$(printf '%s\n' "$details" | json_string_value foreground_cwd)"
  if [[ -z "$cwd" ]]; then
    cwd="$(printf '%s\n' "$details" | json_string_value cwd)"
  fi
  [[ -n "$cwd" ]] || return 1
  printf '%s\n' "$cwd"
}

active_cwd() {
  local pane_id="$1"
  local cwd="${HERDR_ACTIVE_PANE_CWD:-}"
  local details

  if [[ -z "$cwd" ]]; then
    cwd="$(context_value foreground_cwd)"
  fi
  if [[ -z "$cwd" ]]; then
    cwd="$(context_value cwd)"
  fi
  if [[ -z "$cwd" && -n "$pane_id" ]]; then
    cwd="$(pane_cwd "$pane_id")" || true
  fi
  # CLI-invoked plugin actions do not receive HERDR_PLUGIN_CONTEXT_JSON. This is
  # the path used from inside a popup pane: read the currently focused pane while
  # the popup is still alive and retain its cwd before the pane closes.
  if [[ -z "$cwd" && "${HERDR_ENV:-}" == "1" ]]; then
    details="$(current_pane_details)" || true
    cwd="$(printf '%s\n' "$details" | json_string_value foreground_cwd)"
    if [[ -z "$cwd" ]]; then
      cwd="$(printf '%s\n' "$details" | json_string_value cwd)"
    fi
  fi
  if [[ -z "$cwd" && "${HERDR_ENV:-}" != "1" ]]; then
    cwd="$PWD"
  fi

  [[ -n "$cwd" ]] || return 1
  printf '%s\n' "$cwd"
}

notify() {
  local body="$1"
  # Per-event sound: done for completions, request for attention/errors, none for
  # neutral. A caller omits it for a plain toast. `notification_sound = auto` honours
  # this; any other config value forces one sound for every toast.
  local event_sound="${2:-none}"
  local sound

  [[ "$NOTIFICATIONS_ENABLED" == "true" ]] || return 0

  if [[ "$NOTIFICATION_SOUND" == "auto" ]]; then
    sound="$event_sound"
  else
    sound="$NOTIFICATION_SOUND"
  fi

  if [[ -n "${SWITCHBOARD_BIN:-}" && -x "$SWITCHBOARD_BIN" ]]; then
    "$SWITCHBOARD_BIN" notify "$body" "$sound" >/dev/null 2>&1 ||
      log "notification unavailable: $body"
  else
    log "notification skipped before the typed config reader was available: $body"
  fi
}

# --- ghq helpers ------------------------------------------------------------

ghq_root() {
  ghq root 2>/dev/null
}

# Turn a ghq list entry (relative "host/owner/repo") into a workspace/tab label.
repo_label() {
  local rel="$1"
  local mode="${2:-repo}"
  case "$mode" in
    owner-repo)
      printf '%s\n' "$rel" | awk -F/ '{ if (NF>=2) printf "%s/%s\n", $(NF-1), $NF; else print $NF }'
      ;;
    path)
      printf '%s\n' "$rel"
      ;;
    repo | *)
      basename -- "$rel"
      ;;
  esac
}

# Focusing a workspace/agent and opening a repo at a target now live only in the
# Rust switcher (src/action.rs). The picker calls them directly; the clone flow
# reaches them through `herdr-switchboard open` (see ensure_built), so the herdr
# verbs are no longer mirrored here.
