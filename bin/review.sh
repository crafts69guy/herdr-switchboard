#!/usr/bin/env bash
# Review launcher. The git menu (src/git.rs, the --git mode) resolved which repo,
# branch, commit, PR, or saved review; this execs the right tool over the git pane
# the way the clone flow execs get.sh — tuicr for review, lazygit for staging, or a
# custom menu.conf command. It reads its inputs from the REVIEW_* environment the
# Rust side (action::run_review) sets.
#
# Every tuicr *TUI* launch carries --no-update-check: the pane is a review surface,
# not a place to discover that a new tuicr exists, and the check costs a network
# round trip in front of the diff. `review comments` is headless and does not take
# the flag.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bin/lib.sh
source "$SCRIPT_DIR/lib.sh"

# herdr's server env is minimal; put the usual toolchain locations on PATH so
# tuicr / lazygit resolve the same way the picker's build does.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

mode="${REVIEW_MODE:-worktree}"
cwd="${REVIEW_CWD:-.}"
arg="${REVIEW_ARG:-}"
custom="${REVIEW_CUSTOM:-}"

[[ -d "$cwd" ]] || die "Repository path no longer exists." "review cwd missing: $cwd"
cd -- "$cwd"

# A custom menu.conf command runs verbatim, replacing the pane.
if [[ "$mode" == "custom" ]]; then
  [[ -n "$custom" ]] || die "This git menu entry has no command." "empty custom review command"
  exec sh -c "$custom"
fi

# lazygit is the staging/commit surface — its own TUI, not a tuicr mode.
if [[ "$mode" == "lazygit" ]]; then
  command -v lazygit >/dev/null 2>&1 || die "lazygit is not installed." "lazygit not found on PATH"
  exec lazygit
fi

# Everything else is tuicr. The theme is tuicr's own (hue-theme writes it); passing
# --theme here would exit 2 the moment the two disagree and take the whole review
# path down with it.
ensure_tuicr
case "$mode" in
  # -w is staged *and* unstaged: tuicr has no staged-only flag, and lazygit is the
  # better pre-commit surface anyway.
  worktree) exec tuicr -w --no-update-check ;;
  # Dropping -w here would review the branch without the working tree — the
  # uncommitted work you are most likely about to look at.
  branch)
    if [[ -n "$arg" ]]; then
      exec tuicr -r "$arg.." -w --no-update-check
    else
      exec tuicr -w --no-update-check
    fi
    ;;
  commits) exec tuicr --no-update-check ;;
  all-files) exec tuicr -A --no-update-check ;;
  pr)
    [[ -n "$arg" ]] || die "No pull request to review." "pr review with empty number"
    exec tuicr pr "$arg" --no-update-check
    ;;
  comments)
    [[ -n "$arg" ]] || die "No review session to open." "comments review with empty session"
    # Headless, unlike every other mode here: it prints and exits, which would
    # close this pane before anything could be read. Hold it open instead of
    # exec-ing, the way the clone flow holds its output.
    tuicr review comments --session "$arg" || true
    printf '\n\033[2mpress any key to close\033[0m'
    read -rsn1 _ || true
    exit 0
    ;;
  # tuicr has no conflict mode, and -p is not the way in: `git diff` on an unmerged
  # path is a *combined* diff (`diff --cc` / `@@@ -1,3 -1,3 +1,9 @@@`), not the
  # two-sided diff a review tool reads. --file opens the file itself, conflict
  # markers and all, which is what there is to look at mid-merge.
  conflict)
    [[ -n "$arg" ]] || die "No conflicted file to open." "conflict review with empty path"
    # `git status --porcelain` prints paths from the repo root even when it runs in a
    # subdirectory, and this pane may well be in one — resolve against the top level,
    # not against the cwd.
    root="$(git rev-parse --show-toplevel 2>/dev/null || printf '%s' "$cwd")"
    # `DD` (both deleted) is a real conflict with no file on disk: it is listed so it
    # is not a silent omission, but there is nothing for tuicr to open.
    [[ -f "$root/$arg" ]] || die "That file is not on disk." "conflict file missing: $root/$arg"
    exec tuicr --file "$root/$arg" --no-update-check
    ;;
  *) die "Unknown review mode '$mode'." "unknown review mode '$mode'" ;;
esac
