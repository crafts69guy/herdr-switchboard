# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A [herdr](https://herdr.dev) plugin providing a unified switcher over three sources — running
herdr **agents**, open herdr **workspaces**, and **ghq repos** — in one fuzzy picker. It is a
Rust TUI (ratatui + nucleo), not an fzf wrapper. The switcher and the changelog viewer are
modes of the same binary; the settings form is a floating overlay **inside** the switcher
(⌥,), not a separate mode or pane; the git menu (⌥g) is likewise a floating overlay whose
selection `exec`s a review tool. The clone flow and the review launcher (`bin/review.sh`, which
runs `hunk`/`lazygit`/`$EDITOR`) are the only bash that reaches a terminal. The plugin needs no fzf.
See `README.md` for user-facing keybindings and configuration.

## Commands

```bash
cargo build                                  # debug binary
cargo build --release                        # what bin/picker.sh actually launches
cargo test                                   # unit tests (sorting, group filter, history parsing)
cargo test recent_sort_puts_latest_opened_first   # single test by name
cargo fmt --check
cargo clippy --all-targets -- -D warnings    # warnings are failures
bash tests/manifest_spec.sh                  # manifest/entrypoint contract, version sync, bash syntax
bash tests/update_guard_spec.sh              # the update guard, with herdr stubbed via HERDR_BIN_PATH
bash bin/release.sh 0.5.0                    # cut a release (gates, bump, changelog, tag, gh release)

herdr plugin link /path/to/herdr-ghq         # install this checkout for manual testing
herdr server reload-config                   # after touching keybindings/config
herdr plugin config-dir ghq                  # where the runtime config.toml lives
```

There is no test runner for the bash layer beyond `tests/manifest_spec.sh`. Changes to overlay
layout, keybindings, or herdr CLI calls need manual exercise in a real herdr session.

## Architecture

**Two layers, joined by environment variables.** Every action starts in bash and may end in Rust:

1. `bin/action.sh` is the single entrypoint for all manifest actions. It maps the action id
   (via `HERDR_PLUGIN_ACTION_ID`) to a pane id (`picker` / `get` overlays, `changelog` popup) and
   its placement, captures the **origin pane id and cwd** before the pane steals focus, and
   passes them forward as
   `GHQ_ORIGIN_PANE_ID` / `GHQ_ORIGIN_CWD` on `herdr plugin pane open`. The `git` action reuses the
   `picker` pane with `GHQ_OPEN_GIT=1`, which opens the switcher straight into the git overlay.
2. `bin/picker.sh` resolves a versioned, checksummed release binary for managed installs and
   falls back to Cargo for offline/linked checkouts. Its Bash typing-cat owns first-run feedback;
   `--prepare` resolves the binary without launching it.
3. The TUI (`src/`) claims the terminal before loading entries, animates a terminal-cell ASCII cat
   (`startup.rs`) while a worker runs the source commands, then runs the picker event loop and —
   **after `ratatui::restore()`** —
   dispatches the accepted action. Interactive accepts (clone prompt, remove confirmation, `ghq
get -u` output) deliberately run on the torn-down terminal, not inside the TUI.

**Why the origin pane matters:** `split` and `pane` targets act on the captured `GHQ_ORIGIN_PANE_ID`.
The overlay pane is _not_ the user's pane. Never guess or infer a pane/workspace/agent id — every id
must come from `herdr agent list`, `herdr workspace list`, or the captured origin.

**Module split (`src/`):**

- `main.rs` — `App` (a thin shell over four sub-structs: `Picker` / `PreviewState` /
  `ChangelogState` / `HitZones`), `handle_key` → `Flow` (Continue/Quit/Accept) which resolves a
  chord through the keymap and runs `apply_action`, and `browse_order`
- `keymap.rs` — the `Chord → Action` tables (Insert + Normal), built from defaults and overridden
  by `keys.*` config lines; `Mode` (Insert/Normal) and the `keymode = modal` flag. `chord_of`
  reduces a key event; `parse_chord` reads a config spec
- `source.rs` — the `Source` registry (kind / enabled / load) that `load_all` folds; adding a
  source's data is a new impl + one registry line. Preview/dispatch stay per-kind matches (a cycle
  otherwise), guarded by the compiler
- `startup.rs` — the source-loading worker plus the full/compact ASCII typing-cat animation, drawn
  entirely with terminal cells (no image protocol); progress messages cross a channel and never
  block the event loop
- `runner.rs` — the `CommandRunner` trait (`SystemRunner` in prod, `MockRunner` in tests) every
  herdr/ghq/git call routes through, which is what makes the IO edge testable
- `data.rs` — `Theme`, `Config`, `Entry`, and the per-source loaders `load_agents` /
  `load_workspaces` / `load_repos`
- `markdown.rs` — the changelog/README markdown (`Block`, `parse`, `render`, `spans`,
  `flatten_links`, `wrap`), shared by `changelog`, `ui`, and `preview`
- `state.rs` — `state_dir()` + `now()`, shared by `history` and `update`
- `tui.rs` — the shared command-bar `pill_row` widget and `run_simple` (the changelog
  event loop; the picker keeps its own loop for the preview worker + mouse)
- `ui.rs` — three-row layout: Search (3) / body (list + optional preview) / full-width command bar (1);
  `boxed()` is the shared rounded-block helper. It also draws the in-picker overlays: the `⌥c`
  changelog, the `?` cheatsheet, and — via `settings::draw` — the `⌥,` settings form
- `preview.rs` — the preview card (header + pills / meta column / captioned rules). Reads
  agents and workspaces from herdr's JSON with `serde_json` and styles everything from
  `Theme`; shells out to `bin/preview.sh` only for the repo file tree, which arrives as
  ANSI already and passes through `ansi-to-tui`
- `git.rs` — the `⌥g` git overlay: the review menu (worktree/staged/branch/history/conflicts/
  lazygit + `menu.conf` customs) drawn like `settings`, plus the IO-free helpers it opens with
  (`detect_base_branch`, `load_commits`, `parse_menu_conf`). A selection resolves a `ReviewSpec`
  the picker hands to `action::run_review`, which `exec`s `bin/review.sh`
- `hunk.rs` — the `hunk-theme` mode: maps herdr `[theme.custom]` → a `~/.config/hunk/config.toml`
  `[custom_theme]` (chrome only; diff bg stays hunk's `base`). Marker-guarded, so it never
  overwrites a hand-written hunk config; `bin/review.sh` calls it before launching `hunk`
- `action.rs` — `Accept` enum → herdr CLI verbs, plus `run_review` (`exec`s `bin/review.sh`)
- `history.rs` — recency state at `$XDG_STATE_HOME/herdr-ghq/recent.tsv`, atomic write, cap 200
- `settings.rs` — the `Settings` overlay: the `SETTINGS` form, its cycle rings, and `write_setting`,
  a flat-config writer that preserves comments and hand-added keys. Opened with `⌥,` and drawn as a
  floating two-column card **over** the picker (like the `⌥c` changelog), not a separate pane; the
  picker embeds it as `App::settings` and routes keys to `Settings::on_key` while it is shown. Edits
  are **drafts** (`values` vs the `saved` baseline): cycling stages a value, `a` calls `apply`
  (writes only the changed keys, then adopts the draft), and `esc` calls `discard` — nothing hits
  `config.toml` until you apply
- `changelog.rs` — the `--changelog` mode: parses `$HERDR_PLUGIN_ROOT/CHANGELOG.md` and renders it
  (inline markdown, hanging-indent wrap, `← installed` marker from `CARGO_PKG_VERSION`). `parse` +
  `render` are shared with the picker's `⌥c` popup, so both surfaces stay identical
- `update.rs` — the `--update-check` mode plus the cache the picker reads
  (`$XDG_STATE_HOME/herdr-ghq/update.tsv`, `checked_at<TAB>latest`, 24h TTL)

**Sort vs. search:** fuzzy score always wins while a query is present; `SortMode` (recent/name/kind)
only orders the resting, no-query list. Both paths honour the `GroupFilter`. Ties break on load
order so the list stays stable.

## Non-obvious constraints

- **The git menu dispatches by `exec`-ing `review.sh` in the overlay pane, not by opening a new
  herdr pane.** A review takes over the picker's own overlay pane the way the clone flow's
  `Accept::Clone` `exec`s `get.sh` — so there is **no manifest pane for review**, and `hunk` quits
  back to the origin when the overlay pane closes. `Git::on_key` returning `true` (a `chosen`
  `ReviewSpec` set) is turned into `Flow::Accept(Accept::Git)`, which `main` intercepts *before*
  `action::dispatch` and routes to `run_review`. `hunk`/`show`/`patch` are TUIs — the `hunk-review`
  SKILL forbids running them non-interactively (they block); only ever launch them in a pane.
- **The bash layer delegates open + config to the Rust binary; it no longer mirrors them.** The
  clone flow (`bin/get.sh`) opens a repo with `herdr-ghq-switcher open --target … --path … --origin …
  --label …` and reads settings with `herdr-ghq-switcher config get <key> [default]`, so the herdr
  open verbs (`src/action.rs::open_target`) and the flat-config reader (`Config::load`) live in one
  place. `bin/lib.sh` keeps `ensure_built` (build-on-demand, shared by the picker and clone flow),
  `toml_get` (used only by `configure_notifications`, the pre-build notification path that must not
  depend on a cargo build), and the pane-context/JSON helpers. The old bash `open_repo`/`focus_*`/
  `theme_color`/`hex_rgb` are gone. **A change to how a target opens now lands only in `action.rs`.**
- **Config parsing is intentionally flat.** `Config::load` (`src/data.rs`) is the canonical
  hand-rolled line parser — one `key = value` per line, no sections, no nesting — and bash reads
  settings through it via `herdr-ghq-switcher config get`. `toml_get` (`bin/lib.sh`) survives only
  for `configure_notifications`, which runs before the binary is guaranteed built; it must stay
  format-compatible. Do not add a TOML crate or nested keys without changing `Config::load`, the
  writer in `src/settings.rs` (`write_setting`, which preserves comments and hand-added keys), and
  the `toml_get` mirror. Theme parsing (`[theme.custom]` from herdr's config) is a separate
  hand-rolled scanner in both `Theme::load` and… nowhere in bash any more (the bash `theme_color`
  was removed with the other dead mirrors).
- **A click zone is measured by the loop that draws the thing.** `tab_zones` and
  `footer_zones` (`src/ui.rs`) are built inside the same loops that lay out the tab strip
  and the command bar, because a zone computed separately drifts the moment a label
  changes — and drifts _silently_, into clicking the wrong action. `list_state` is kept
  on the `App` for the same reason: its scroll offset is the only thing that turns a
  clicked row back into an entry, so it cannot be a fresh `ListState` per frame.
- **The cheatsheet's descriptions must fit `HELP_DESC`** (`src/ui.rs`) — the popup's half
  width less the key pill, around 19 columns. A longer one is cut with no ellipsis, so it
  ships looking like a shorter phrase; `wheel  Scroll whatever is under it` reached a
  README screenshot as `Scroll whatever is`. `row` asserts, and a `TestBackend` render
  test in `main.rs` fires it.
- **Keys are a config-driven keymap, not hardcoded `match` arms.** `handle_key` resolves a
  `Chord` through `App::keymap` (`src/keymap.rs`) and runs `apply_action`. Two ordered tables
  (Insert + Normal) plus a `␣` leader table; `keymode` picks the start mode and `esc` toggles
  Insert↔Normal. `keys.<action>` config lines rebind (first chord wins as the shown one).
  **The footer (`draw_footer`) and the cheatsheet (`draw_help`) render from the keymap via
  `Keymap::label_for(mode, action)`**, so both re-label per mode and per remap — never hardcode a
  key cap in either; add the action to the curated list and it picks up its live chord. A row
  whose action is unbound in the current mode drops out. `label_for` renders leader verbs as
  `␣g`. Adding an action is one row in `keymap::NAMES`, its default chord in a table, and an
  `apply_action` arm; a new `Accept` also needs the footer curated list and `dispatch`. Cheatsheet
  descriptions must still fit `HELP_DESC`.
- **The mouse is turned on by hand, and must be turned off on every exit path.** `main.rs`
  writes `?1000h`/`?1006h` itself rather than using crossterm's `EnableMouseCapture`, which
  also enables any-event tracking (`?1003h`) — every pointer move would wake the loop into
  a redraw for an event we discard. `?1000h` reports the wheel *and* buttons, which is
  exactly what the picker consumes; drags stay herdr's, which runs with
  `mouse_capture = true`. `init_terminal`/`restore_terminal` pair the escapes, and
  `init_terminal` chains the disable ahead of the panic hook `ratatui::init` installs,
  since that hook restores the screen but knows nothing about the mouse. Leaving it on
  drops mouse escapes into the user's shell.
- **The preview clips; it must never wrap.** Every body goes through `clip`/`clip_line`
  (`src/preview.rs`) so one card line is exactly one screen row — that is what makes
  `preview_scroll` mean what it says and `preview_len`/`preview_rows` bound it correctly.
  `draw_preview` therefore has no `Wrap`. Re-adding one, or emitting an unclipped line,
  breaks the scroll silently: the offset drifts from the content instead of erroring. The
  pane's width reaches the worker through `App::preview_width`, published by `ui::draw`,
  which is why `run` draws _before_ it calls `request_preview`.
- **Nothing uses `jq` — keep it that way.** No code path shells out to it: the bash layer reads
  herdr's JSON with the awk-based `json_string_value` / `json_bool_value` in `bin/lib.sh`, and the
  Rust layer uses `serde_json` (`data.rs`, `preview.rs`). It is not a documented requirement, so a
  new jq call would be a new hard dependency on a machine that may not have it — and a silent one,
  since a missing jq fails the same way a wrong filter does: empty output, no error.
- **`GHQ_FORCE_TARGET` overrides `default_target` for Enter, repos only.** `bin/action.sh` exports it
  for the `open-workspace` / `open-tab` / `open-split` hot-path actions; `src/action.rs`
  (`forced_target` + `resolve_default_target`) resolves it once in `main` and passes it to
  `dispatch`. Enter on an **agent** or **workspace** still focuses that entry — forcing a target
  only changes where a _repo_ lands, matching the manifest's "Pick a repo; Enter opens it in…".
  Invalid values on either the env var or the config degrade to `workspace` instead of erroring.
- **Version sync:** `Cargo.toml` and `herdr-plugin.toml` versions must match; `tests/manifest_spec.sh`
  enforces it. `bin/release.sh` bumps both, so bump through it rather than by hand.
- **The changelog is the release notes.** Every user-facing change adds a line to
  `CHANGELOG.md`'s `[Unreleased]` section _in the same commit_; `bin/release.sh` promotes that
  section to a dated one and tags. The tag workflow builds four native archives, generates
  `SHA256SUMS`, and publishes that section verbatim after all targets pass. Commits are not
  Conventional Commits and nothing comes from `git log` — an empty `[Unreleased]` aborts.
- **The TUI never makes a network request.** `update.rs` spawns a detached `--update-check`
  child (own process group, no stdio) that runs `git ls-remote` and writes a cache; the picker
  only ever reads that file, so the badge lands on a _later_ launch. Do not "simplify" this into
  a thread: the picker frequently exits in under a second and the fetch takes several, so the
  cache would never be written. `git ls-remote` over the GitHub API on purpose — no `jq`, no
  60/hour unauthenticated rate limit, no auth. Everything fails silently.
- **The update flow fails closed.** `bin/update-plugin.sh` installs only when herdr reports
  an unambiguous `"source":{"kind":"github"…}`; local links, unreadable output, and shapes it
  does not recognise all refuse. The failure it must never make is the permissive one —
  `herdr plugin install` would overwrite a contributor's working tree. `tests/update_guard_spec.sh`
  stubs `herdr` through `HERDR_BIN_PATH` and asserts every case. Never widen the guard without
  extending that spec, and never name a real mutating command inside backticks in it.
- **An update must force a rebuild.** `target/` is gitignored, so re-fetching the source leaves
  the old binary in place and `bin/picker.sh` only builds when the binary is _missing_ — the new
  code would ship with the old switcher still running. `update-plugin.sh` removes it and rebuilds.
- **`ctrl-x` (remove) is the only destructive path.** It requires typing the repo name to confirm.
  Preserve that; test against disposable repos.
- **Pane commands must launch through `$HERDR_PLUGIN_ROOT`** — `tests/manifest_spec.sh` asserts the
  exact manifest string, since herdr starts panes from the user's repo, not the plugin checkout.

## Conventions

Rustfmt defaults; `anyhow::Result` with typed errors; no `unwrap()` in production paths. Bash uses
`#!/usr/bin/env bash`, `set -euo pipefail`, quoted expansions, and helpers from `bin/lib.sh`.
TOML keys are snake_case; plugin action ids are kebab-case. Commits are short and imperative;
`bin/release.sh` makes the `Release vX.Y.Z` commit, so do not hand-tag subjects like `(v0.4.0)`
the way pre-0.5.0 commits did. Never commit `target/`.

## Agent skills

### Issue tracker

Local markdown — issues and specs live as files under `.scratch/<feature>/` in this repo
(gitignored). See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical roles (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`,
`wontfix`), used as-is. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context — `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
