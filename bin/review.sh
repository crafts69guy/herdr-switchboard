#!/usr/bin/env bash
# Review launcher. The git overlay (src/git.rs) resolved which repo, branch, or
# commit; this execs the right tool in the picker's overlay pane the way the clone
# flow execs get.sh — hunk for read-only review, lazygit for staging, $EDITOR for
# conflict resolution, or a custom menu.conf command. It reads its inputs from the
# REVIEW_* environment the Rust side (action::run_review) sets.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/lib.sh
source "$SCRIPT_DIR/lib.sh"

# herdr's server env is minimal; put the usual toolchain locations on PATH so
# hunk / lazygit resolve the same way the picker's build does.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

mode="${REVIEW_MODE:-worktree}"
cwd="${REVIEW_CWD:-.}"
arg="${REVIEW_ARG:-}"
custom="${REVIEW_CUSTOM:-}"

# Branded pre-roll before a hunk review: the switcher animates the same ASCII cat
# the picker starts with while it warms the diff's cache, so a slow `hunk diff` on
# a large repo opens onto the splash instead of a frozen pane. Best effort — it
# reads REVIEW_MODE/REVIEW_ARG/REVIEW_CWD from the inherited env, and a missing or
# failing binary must never block the review. Not used for lazygit/custom, which
# bring their own startup.
review_preroll() {
  "$(ensure_built)" review-splash || true
}

[[ -d "$cwd" ]] || die "Repository path no longer exists." "review cwd missing: $cwd"
cd -- "$cwd"

# A custom menu.conf command runs verbatim, replacing the pane.
if [[ "$mode" == "custom" ]]; then
  [[ -n "$custom" ]] || die "This git menu entry has no command." "empty custom review command"
  exec sh -c "$custom"
fi

# lazygit is the staging/commit surface — its own TUI, not a hunk mode.
if [[ "$mode" == "lazygit" ]]; then
  command -v lazygit >/dev/null 2>&1 || die "lazygit is not installed." "lazygit not found on PATH"
  exec lazygit
fi

# Conflicts: hunk can only *review*, so show the conflicted diff, then open the
# editor on the unmerged files to actually resolve them. Kept bash 3.2-safe (no
# mapfile) since herdr may launch the system bash.
if [[ "$mode" == "conflicts" ]]; then
  files=()
  while IFS= read -r f; do
    [[ -n "$f" ]] && files+=("$f")
  done < <(git diff --name-only --diff-filter=U 2>/dev/null || true)
  if [[ ${#files[@]} -eq 0 ]]; then
    printf 'No unmerged files.\n'
    printf '\n\033[2mpress any key to close\033[0m'
    read -rsn1 _ || true
    exit 0
  fi
  ensure_hunk
  review_preroll
  hunk diff -- "${files[@]}" || true
  exec "${EDITOR:-vi}" "${files[@]}"
fi

# Everything else is a hunk review.
ensure_hunk
review_preroll
case "$mode" in
  worktree) exec hunk diff ;;
  staged) exec hunk diff --staged ;;
  branch)
    if [[ -n "$arg" ]]; then
      exec hunk diff "$arg"
    else
      exec hunk diff
    fi
    ;;
  history)
    [[ -n "$arg" ]] || die "No commit to show." "history review with empty sha"
    exec hunk show "$arg"
    ;;
  *) die "Unknown review mode '$mode'." "unknown review mode '$mode'" ;;
esac
