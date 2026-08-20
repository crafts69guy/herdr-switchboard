# Repository Guidelines

## Project Structure & Module Organization

The Rust TUI lives in `src/`, Bash entrypoints in `bin/`, integration checks in
`tests/`, and user-facing assets in `docs/`. Current module ownership lives in
`docs/architecture.md`; do not duplicate its file-by-file inventory here or in
`CLAUDE.md`. Read `CLAUDE.md` for non-obvious runtime, security, and performance
contracts before changing their implementations.

The stable architectural rules are: `surface` is the only terminal/event-loop host;
slow external work crosses a typed effect seam; process calls use `CommandRunner`
except for the documented socket adapter; and feature implementation children stay
private. Plugin metadata is defined in `herdr-plugin.toml`, sample user configuration
belongs in `examples/`, and generated `target/` artifacts are never committed.

## Documentation Contract

- `README.md` is user-facing: installation, configuration, usage, and contribution
  entrypoints.
- `AGENTS.md` owns workflow and verification rules; `CLAUDE.md` owns non-obvious
  invariants.
- `docs/architecture.md` is the single source of truth for current module ownership
  and seams.
- A module move, rename, split, or responsibility change updates architecture docs
  in the same commit.
- Search all Markdown for the old path, symbol, and ownership wording before committing.
- Run `bash tests/docs_spec.sh`; do not satisfy it by copying the module map into another document.

## Build, Test, and Development Commands

- `cargo build` compiles a debug binary for quick iteration.
- `cargo build --release` produces the binary launched by `bin/picker.sh`.
- `cargo test` runs Rust unit tests, including filtering and history behavior.
- `bash bin/check.sh` is the complete local, CI, and release gate: formatting,
  Clippy, Rust tests, and every shell specification.
- `bash bin/release.sh <version>` cuts a release. It needs a terminal for its
  confirmation prompt, so an agent cannot run it — ask the maintainer to.
- `cargo fmt --check` verifies Rust formatting.
- `cargo clippy --all-targets -- -D warnings` treats lint findings as failures.
- `herdr plugin link /path/to/herdr-switchboard` installs the checkout for manual testing;
  reload configuration with `herdr server reload-config`.

## Coding Style & Naming Conventions

Use rustfmt defaults (four-space indentation), `snake_case` for functions and
modules, and `PascalCase` for Rust types. Prefer typed errors with `anyhow::Result`
and avoid `unwrap()` in production paths. Bash scripts must use
`#!/usr/bin/env bash`, `set -euo pipefail`, quoted expansions, and shared helpers
from `bin/lib.sh`. Keep TOML keys and plugin action IDs snake_case and kebab-case,
respectively (for example, `default_target` and `open-workspace`).

## Testing Guidelines

Place focused Rust tests beside their module in `#[cfg(test)]` blocks and name
them after observable behavior. Extend `tests/manifest_spec.sh` when changing
the manifest or entrypoint contract. Before submitting, run `bash bin/check.sh`.
Manually exercise the overlay
for layout, keybinding, or Herdr CLI changes; attach a current screenshot when
visual output changes.

## Commit & Pull Request Guidelines

Commits use short, imperative summaries; keep each one focused. Pull requests should
explain user impact, list verification performed, link related issues, and call out
required herdr/ghq versions.

Any change a user would notice adds a line to the `## [Unreleased]` section of
`CHANGELOG.md` **in the same commit** — describe the change in the user's terms
(`alt-p toggles the preview`), not the code's (`refactor preview module`). Purely
internal work (formatting, refactors, contributor docs) adds nothing. Nothing is
generated from `git log`, so an entry that is not written here is lost: `bin/release.sh`
promotes `[Unreleased]` verbatim into the GitHub release notes, and aborts if it is
empty.

Releases go through `bin/release.sh`, which bumps `Cargo.toml` and `herdr-plugin.toml`
together, dates the changelog section, and tags. The tag workflow builds four native
binaries and publishes the release only when they all pass. Do not bump versions by hand; older
commits ended their summary with a release tag such as `(v0.4.0)`, but the script now
makes a dedicated `Release vX.Y.Z` commit instead.

## Safety & Configuration

Never hardcode credentials or user-specific paths. Verify real pane, workspace,
and agent IDs before issuing herdr commands. Preserve typed confirmation for
repository removal and test destructive flows against disposable repositories.

Usage is the only surface that reads a credential and the only in-process HTTP
client. Two rules hold there and nowhere else.

- **A secret never reaches a command line.** `argv` is readable through `ps` by
  every process the user owns, for the whole life of the call, so the OAuth token
  goes to `curl` through stdin as a `--config -` file. That is the only reason
  `CommandRunner::output_stdin` exists. Do not add a caller without the same
  justification, and do not "tidy" the header back into a `-H` flag.
- **Nothing read from a credential file is kept.** `usage.rs` takes the `email`
  claim out of the Codex ID token and drops everything else in that file; no token
  is ever logged, traced, drawn, cached, or forwarded. `trace.rs` writes to a file,
  so anything passed to it is anything written to disk.

Do not add network work to a launch or navigation hot path. The explicit exceptions
are Git's on-demand pull-request fetch through `gh`, Clone/update commands the user
invokes deliberately, and `update.rs`, whose `git ls-remote` runs in a detached child
because the Projects Picker frequently exits in under a second.
