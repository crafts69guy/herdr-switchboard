# Repository Guidelines

## Project Structure & Module Organization

The Rust TUI lives in `src/`. `surface.rs` is the universal host for terminal claim,
mouse capture, polling, redraw, and teardown; no other module owns an event loop.
`main.rs` dispatches argv modes, while `projects.rs` composes and hosts the Projects
Picker. `source.rs` holds the explicit `ProjectCatalog` load policy, while `data.rs`
parses source responses and carries entry presentation data. `query.rs`,
`projects/view.rs`, `projects/preview.rs`, `action.rs`, and `history.rs` carry query
compilation, responsive Projects Picker rendering, previews, accepted effects, and
recency state. `picker.rs` is the shared engine behind the
mode pickers (`menu.rs`, `agents.rs`, `commands.rs`, `ports.rs`, `zen.rs`), and
`tui.rs` is rendering vocabulary only. `settings.rs`, `changelog.rs`, and `usage.rs`
are popups hosted through `Surface`; `git.rs` is its own pane with typed background
effects. `chrome.rs`, `socket.rs`, `runner.rs`, `notify.rs`, `config.rs`, `keymap.rs`,
`markdown.rs`, `state.rs`, `trace.rs`, `splash.rs`, and `update.rs` are supporting
modules. Read `CLAUDE.md` before changing their non-obvious contracts.

Bash entrypoints in `bin/` connect the TUI and the bash clone flow to herdr. Plugin
metadata is defined in `herdr-plugin.toml`; sample user configuration belongs in
`examples/`, integration checks in `tests/`, and documentation images in `docs/`.
Do not commit generated `target/` artifacts.

## Build, Test, and Development Commands

- `cargo build` compiles a debug binary for quick iteration.
- `cargo build --release` produces the binary launched by `bin/picker.sh`.
- `cargo test` runs Rust unit tests, including filtering and history behavior.
- `bash tests/manifest_spec.sh` validates plugin actions, pane paths, version sync
  between `Cargo.toml` and `herdr-plugin.toml`, and Bash syntax from a foreign
  working directory.
- `bash tests/update_guard_spec.sh` checks that the update flow refuses to install
  over anything but an unambiguous GitHub install; it stubs `herdr` via
  `HERDR_BIN_PATH` and never touches the real one.
- `bash tests/bootstrap_spec.sh` checks release target mapping, checksum rejection,
  and atomic installation of the prebuilt switcher.
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
the manifest or entrypoint contract. Before submitting, run `cargo test`, all four
shell specs listed above, formatting, and Clippy. Manually exercise the overlay
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
