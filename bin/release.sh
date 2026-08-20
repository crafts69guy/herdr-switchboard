#!/usr/bin/env bash
set -euo pipefail

# Cut a release: verify, bump both version files, promote the changelog's
# [Unreleased] section, tag, and push. The tag-triggered Release workflow builds
# all supported binaries, checksums them, and publishes the GitHub release from
# that same changelog section only after every target succeeds.
#
#   bash bin/release.sh 0.5.0
#
# This is a maintainer script run from a normal terminal, so it deliberately does
# not source bin/lib.sh — that toolkit's die() notifies through herdr.

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

readonly REPO_URL="https://github.com/crafts69guy/herdr-switchboard"
readonly CHANGELOG="$ROOT/CHANGELOG.md"
readonly MANIFEST="$ROOT/herdr-plugin.toml"
readonly CARGO_TOML="$ROOT/Cargo.toml"

log() {
  printf 'release: %s\n' "$*" >&2
}

fail() {
  log "$*"
  exit 1
}

version="${1-}"
[[ -n "$version" ]] || fail "usage: bash bin/release.sh <version>   (e.g. 0.5.0)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "version must be x.y.z, got '$version'"

tag="v$version"
today="$(date +%F)"

# --- preflight ---------------------------------------------------------------
# Everything that can reject the release runs before a single file is touched, so
# a rejected release leaves the tree exactly as it was found.

# The publish step asks for confirmation, and `read` cannot ask anything without a
# terminal — it would hit EOF and, under `set -e`, kill the script silently right
# after the bump. Refuse now, while the tree is still untouched.
[[ -t 0 ]] || fail "stdin is not a terminal; run this from a terminal so the confirmation prompt works"

branch="$(git rev-parse --abbrev-ref HEAD)"
[[ "$branch" == "main" ]] || fail "releases are cut from main, not '$branch'"

[[ -z "$(git status --porcelain)" ]] || fail "working tree is dirty; commit or stash first"

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  fail "tag $tag already exists"
fi

previous_tag="$(git describe --tags --abbrev=0 2>/dev/null || true)"
[[ -n "$previous_tag" ]] || fail "no previous tag found; cannot build a compare link"

# Promoting an empty section would publish a release with no notes.
unreleased="$(awk '/^## \[Unreleased\]/ { flag = 1; next } /^## \[/ { flag = 0 } flag' "$CHANGELOG")"
[[ -n "${unreleased//[[:space:]]/}" ]] ||
  fail "CHANGELOG.md [Unreleased] is empty; nothing to release"

# --- gates -------------------------------------------------------------------
# Run on the clean tree: a failure here is an abort, not a half-bumped checkout.

log "running gates"
bash bin/check.sh

# --- bump --------------------------------------------------------------------

log "bumping $previous_tag -> $tag"

# Only the leading [package]/manifest key block declares a bare `version = "..."`
# at column 0, so the first match is the one to rewrite.
bump_version() {
  local file="$1"
  local tmp="$file.tmp"

  awk -v new="$version" '
    !done && /^version = "/ { sub(/"[^"]*"/, "\"" new "\""); done = 1 }
    { print }
  ' "$file" >"$tmp"
  mv "$tmp" "$file"
}

bump_version "$CARGO_TOML"
bump_version "$MANIFEST"

# Refresh Cargo.lock's own record of the package version.
cargo check --quiet

# Promote [Unreleased] into a dated section, leaving a fresh empty one behind, and
# retarget the compare links.
promote_changelog() {
  local tmp="$CHANGELOG.tmp"

  awk \
    -v new="$version" \
    -v date="$today" \
    -v prev="$previous_tag" \
    -v tag="$tag" \
    -v url="$REPO_URL" '
    /^## \[Unreleased\]$/ {
      print "## [Unreleased]"
      print ""
      print "## [" new "] - " date
      next
    }
    /^\[Unreleased\]: / {
      print "[Unreleased]: " url "/compare/" tag "...HEAD"
      print "[" new "]: " url "/compare/" prev "..." tag
      next
    }
    { print }
  ' "$CHANGELOG" >"$tmp"
  mv "$tmp" "$CHANGELOG"
}

promote_changelog

# The bump must leave the two version files agreeing.
bash tests/manifest_spec.sh

# --- publish -----------------------------------------------------------------

notes="$(awk -v want="## [$version]" '
  index($0, want) == 1 { flag = 1; next }
  /^## \[/ { flag = 0 }
  flag
' "$CHANGELOG")"
[[ -n "${notes//[[:space:]]/}" ]] || fail "could not extract release notes for $version"

log "release notes for $tag:"
printf '%s\n' "$notes" >&2

# `|| true`: a bare `read` returning non-zero would trip `set -e` before fail() runs.
reply=""
read -r -p "release: publish $tag? [y/N] " reply || true
[[ "$reply" == "y" || "$reply" == "Y" ]] ||
  fail "aborted; undo the bump with: git checkout Cargo.toml herdr-plugin.toml Cargo.lock CHANGELOG.md"

git add "$CARGO_TOML" "$MANIFEST" "$ROOT/Cargo.lock" "$CHANGELOG"
git commit -m "Release $tag"
git tag -a "$tag" -m "$tag"
git push origin "$branch"
git push origin "$tag"

log "pushed $tag; the Release workflow will publish it after all four binaries pass"
