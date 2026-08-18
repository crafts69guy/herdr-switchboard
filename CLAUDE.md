# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A [herdr](https://herdr.dev) plugin providing a unified switcher over three sources — running
herdr **agents**, open herdr **workspaces**, and **ghq repos** — in one fuzzy picker. It is a
Rust TUI (ratatui + nucleo), not an fzf wrapper. The switcher and the changelog viewer are
modes of the same binary; the settings form is a floating overlay **inside** the switcher
(⌥,), not a separate mode or pane. The **git menu is a herdr pane of its own** (`Prefix + g` →
`bin/git.sh` → `--git`), not an overlay on the switcher, and its selection `exec`s a review tool
over that pane. The clone flow and the review launcher (`bin/review.sh`, which runs
`tuicr`/`lazygit`) are the only bash that reaches a terminal. The plugin needs no fzf.
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

herdr plugin link /path/to/herdr-switchboard         # install this checkout for manual testing
herdr server reload-config                   # after touching keybindings/config
herdr plugin config-dir switchboard          # where the runtime config.toml lives
```

There is no test runner for the bash layer beyond `tests/manifest_spec.sh`. Changes to overlay
layout, keybindings, or herdr CLI calls need manual exercise in a real herdr session.

## Architecture

**Two layers, joined by environment variables.** Every action starts in bash and may end in Rust:

1. `bin/action.sh` is the single entrypoint for all manifest actions. It maps the action id
   (via `HERDR_PLUGIN_ACTION_ID`) to a pane id (`picker` / `git` / `get` overlays, `changelog`
   popup) and its placement, captures the **origin pane id and cwd** before the pane steals focus,
   and passes them forward as
   `SWITCHBOARD_ORIGIN_PANE_ID` / `SWITCHBOARD_ORIGIN_CWD` on `herdr plugin pane open`. The `git` action opens the
   dedicated `git` pane; that pane acts on a **cwd**, not a pane id.
2. `bin/picker.sh` resolves a versioned, checksummed release binary for managed installs and
   falls back to Cargo for offline/linked checkouts. Its Bash typing-cat owns first-run feedback;
   `--prepare` resolves the binary without launching it.
3. The TUI (`src/`) loads the sources **synchronously, before claiming the terminal** (~35ms), so
   the first frame is the loaded list; an empty result hands the pane to `bin/get.sh` without ever
   taking the screen. It then runs the picker event loop and — **after `ratatui::restore()`** —
   dispatches the accepted action. Interactive accepts (clone prompt, remove confirmation, `ghq
get -u` output) deliberately run on the torn-down terminal, not inside the TUI.

**Why the origin pane matters:** `split` and `pane` targets act on the captured `SWITCHBOARD_ORIGIN_PANE_ID`.
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
- `splash.rs` — one static ASCII cat frame, drawn entirely with terminal cells. The `--git` mode
  draws it and `exec`s tuicr over it, because tuicr can take 0.9–1.7s to read a large diff before
  its own first frame. No animation, no floor, nothing to cancel — all that is left of `startup.rs`
- `trace.rs` — opt-in perf tracing behind `SWITCHBOARD_TRACE`, appended to `$SWITCHBOARD_TRACE_FILE` or
  `state_dir()/trace.log`. **Never stdout/stderr**: the TUI owns the terminal for its whole life
- `runner.rs` — the `CommandRunner` trait (`SystemRunner` in prod, `MockRunner` in tests) every
  herdr/ghq/git call routes through, which is what makes the IO edge testable
- `data.rs` — `Theme`, `Config`, `Entry`, and the per-source loaders `load_agents` /
  `load_workspaces` / `load_repos`
- `markdown.rs` — the changelog/README markdown (`Block`, `parse`, `render`, `spans`,
  `flatten_links`, `wrap`), shared by `changelog`, `ui`, and `preview`
- `state.rs` — `state_dir()` + `now()`, shared by `history` and `update`
- `tui.rs` — the shared chrome: `boxed()` (the one rounded-panel helper — rounded border in
  `overlay0`, caption in `title_color`), the command-bar `pill_row` widget, and `run_simple`
  (the changelog event loop; the picker keeps its own loop for the preview worker + mouse)
- `ui.rs` — three-row layout: Search (3) / body (list + optional preview) / full-width command bar (1),
  built from `tui::boxed`. It also draws the in-picker overlays: the `⌥c`
  changelog, the `?` cheatsheet, and — via `settings::draw` — the `⌥,` settings form
- `picker.rs` — the shared engine behind the **mode** pickers (`menu` / `commands` / `ports`):
  the `PickerMode` trait, the fuzzy/query state, and a `draw` that renders the same three
  panels as `ui.rs` through `tui::boxed`. Its chrome is not free-styled — see the
  constraint below
- `preview.rs` — the preview card (header + pills / meta column / captioned rules). Reads
  agents and workspaces from herdr's JSON with `serde_json` and styles everything from
  `Theme`; shells out to `bin/preview.sh` only for the repo file tree, which arrives as
  ANSI already and passes through `ansi-to-tui`
- `git.rs` — the **whole `--git` mode**: `main()` (repo from the pane's cwd, menu, event loop,
  preroll, `exec`), the review menu (worktree/branch/commits/all-files/pull-request/saved-reviews/
  lazygit + `menu.conf` customs), the generic `View::List` sub-list over `Vec<Row>` shared by the
  PR and saved-review pickers, and the IO helpers (`detect_base_branch`, `load_rows`,
  `parse_menu_conf`). `on_key` stays IO-free by returning a `Step`: the caller runs the fetch and
  hands the rows back through `show_list`
- `action.rs` — `Accept` enum → herdr CLI verbs, plus `run_review` (`exec`s `bin/review.sh`)
- `history.rs` — recency state at `$XDG_STATE_HOME/herdr-switchboard/recent.tsv`, atomic write, cap 200
- `settings.rs` — the `Settings` overlay: the `SETTINGS` form, its cycle rings, and `write_setting`,
  a namespaced-config writer that preserves comments and hand-added keys. Opened with `⌥,` and drawn as a
  floating two-column card **over** the picker (like the `⌥c` changelog), not a separate pane; the
  picker embeds it as `App::settings` and routes keys to `Settings::on_key` while it is shown. Edits
  are **drafts** (`values` vs the `saved` baseline): cycling stages a value, `a` calls `apply`
  (writes only the changed keys, then adopts the draft), and `esc` calls `discard` — nothing hits
  `config.toml` until you apply
- `changelog.rs` — the `--changelog` mode: parses `$HERDR_PLUGIN_ROOT/CHANGELOG.md` and renders it
  (inline markdown, hanging-indent wrap, `← installed` marker from `CARGO_PKG_VERSION`). `parse` +
  `render` are shared with the picker's `⌥c` popup, so both surfaces stay identical
- `zen.rs` — the whole zen feature: the enter/leave state machine, the `--zen` `PickerMode`, and
  the `zen toggle|on|off|chrome-restore` CLI verb. Pure geometry (`gutter_ratios`,
  `anchor_between`) and the state-file codec are separated from the herdr calls so both are
  testable without a running herdr; every herdr call goes through `CommandRunner`
- `chrome.rs` — the `zen_chrome` levels (`off`/`panes`/`full`) and **the only code that writes
  herdr's own `config.toml`**. `plan_overrides`/`apply`/`restore` are pure `toml_edit` functions
  over a `DocumentMut`; the IO edge is `engage`/`disengage`, and the reload goes through
  `CommandRunner`
- `socket.rs` — **the only** code that talks to herdr's unix socket instead of the CLI, and only
  because `pane.graphics.set`/`.clear` have no CLI subcommand. Everything in it fails soft
- `usage.rs` — the whole `--usage` pane: the `Provider` registry (Codex reads its rate limits off
  the tail of the newest rollout JSONL; Claude asks the endpoint behind the in-session `/usage`),
  the braille donut (`donut_points` is pure and tested), the per-window bars and `Fact` rows, and
  the `SimpleMode` popup. The only network call and the only credential read in the plugin; see the
  constraints below
- `update.rs` — the `--update-check` mode plus the cache the picker reads
  (`$XDG_STATE_HOME/herdr-switchboard/update.tsv`, `checked_at<TAB>latest`, 24h TTL)

**Sort vs. search:** fuzzy score always wins while a query is present; `SortMode` (recent/name/kind)
only orders the resting, no-query list. Both paths honour the `GroupFilter`. Ties break on load
order so the list stays stable.

## Non-obvious constraints

- **herdr draws the pane frame; the TUI must not compete with it.** Every plugin pane is
  `popup` or `overlay`, and herdr frames both and requires a non-empty title. So a pane
  title is a **single icon** (`󰊢`, `󰍜`, `󰆍`, `󰛳`) and the human-readable mode name goes on
  the *list panel* inside — word titles on both produced `Ports` twice, two rows apart.
  For the same reason the plugin's own borders recede in `overlay0` while herdr's frame
  keeps the accent, and **no panel paints a background**: the panes are transparent so the
  terminal shows through all of them. `panel_bg` is for text sitting *on* a coloured pill or
  mode chip, and for the floating popups (changelog/settings) that genuinely need to occlude
  what is under them — not for the picker's own panels. Three tests in `picker.rs`
  (`the_search_box_is_captioned_search_and_the_list_carries_the_mode_title`,
  `panels_use_the_projects_pickers_border_and_caption_slots`,
  `no_panel_paints_an_opaque_background`) pin this down.
- **There is one panel helper, `tui::boxed`, and every framed surface goes through it.**
  It lived in `ui.rs` while `picker.rs` hand-rolled its own `Block`s, and the two drifted
  exactly as you would expect: accent borders instead of `overlay0`, and captions passed as
  bare `&str` so they rendered in the terminal's default foreground rather than
  `title_color`. A new framed surface calls `boxed`; it does not build a `Block` itself.
- **The git menu is a pane, and a review `exec`s over that pane.** There is a manifest pane for
  the *menu* (`[[panes]] id = "git"`), but **none for the review**: `git::main` `exec`s
  `bin/review.sh` over itself the way the clone flow's `Accept::Clone` `exec`s `get.sh`, so tuicr
  inherits the pane and quitting it returns to the pane `Prefix + g` was pressed in. The pane's
  placement must stay **full-frame `overlay`** — tuicr renders into whatever window it is handed,
  and a popup would hand it a tiny one. The picker knows nothing about git: there is no `⌥g`, no
  `Accept::Git`, no `App::git`. Two entry points would drift apart, and only one of them can be
  the fast one. `tuicr` is a TUI — never run it non-interactively (it blocks); only ever in a pane.
- **The picker loads its sources synchronously, before `init_terminal`.** There is no worker, no
  channel, no minimum-visible floor: `load_all` costs ~35ms, and the 420ms floor the old animation
  imposed *was* the first-list latency. An empty result must be detected here too, before the
  terminal is claimed, so the handoff to `bin/get.sh` never takes the screen and gives it back.
  Do not reintroduce a floor "so the cat is visible" — the cat is what was removed.
- **A list fetch runs outside `Git::on_key`.** `on_key` returns a `Step`; `Step::Load(kind)` asks
  the caller to run `load_rows` and hand the result to `show_list`. That is what keeps the entire
  key surface IO-free and unit-testable through `MockRunner`, and it is why `gh pr list` talking to
  GitHub leaves the menu on screen rather than freezing a half-drawn list.
- **Nothing writes to tuicr's config.** The theme is a whole file that
  [hue-theme](https://github.com/crafts69guy/hue-theme) owns
  (`~/.config/tuicr/themes/hue-<mood>.toml`, 41 keys, none optional). tuicr **exits 2** on a theme
  it cannot fully resolve, and `--theme` on a mismatched name takes the whole review path down —
  so `bin/review.sh` never passes `--theme` and `ensure_tuicr` only checks the binary exists.
- **The bash layer delegates open + config to the Rust binary; it no longer mirrors them.** The
  clone flow (`bin/get.sh`) opens a repo with `herdr-switchboard open --target … --path … --origin …
  --label …` and reads settings with `herdr-switchboard config get <key> [default]`, so the herdr
  open verbs (`src/action.rs::open_target`) and typed config reader (`Config::load`) live in one
  place. `bin/lib.sh` keeps `ensure_built` (build-on-demand, shared by the picker and clone flow),
  `toml_get` (used only by `configure_notifications`, the pre-build notification path that must not
  depend on a cargo build), and the pane-context/JSON helpers. The old bash `open_repo`/`focus_*`/
  `theme_color`/`hex_rgb` are gone. **A change to how a target opens now lands only in `action.rs`.**
- **Config parsing is intentionally flat.** `Config::load` (`src/data.rs`) is the canonical
  hand-rolled line parser — one `key = value` per line, no sections, no nesting — and bash reads
  settings through it via `herdr-switchboard config get`. `toml_get` (`bin/lib.sh`) survives only
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
- **`SWITCHBOARD_FORCE_TARGET` overrides `default_target` for Enter, repos only.** `bin/action.sh` exports it
  for the `open-workspace` / `open-tab` / `open-split` hot-path actions; `src/action.rs`
  (`forced_target` + `resolve_default_target`) resolves it once in `main` and passes it to
  `dispatch`. Enter on an **agent** or **workspace** still focuses that entry — forcing a target
  only changes where a _repo_ lands, matching the manifest's "Pick a repo; Enter opens it in…".
  Invalid values on either the env var or the config degrade to `workspace` instead of erroring.
- **Zen cannot be built on `herdr pane zoom`, and this was measured.** Splitting a zoomed tab
  **silently cancels the zoom** (`zoomed` flips to `false` the moment a gutter appears), and
  `pane layout` on a zoomed tab reports the *underlying* rects rather than the rendered ones, so
  gutters sized from it land wrong. Zen therefore `pane move --new-tab`s the target instead —
  which preserves its `terminal_id`, so the process survives — and **never touches another pane**.
  Do not "simplify" zen back to zoom: it silently produces a tab with an uncentred pane and two
  stray shells. Two further counter-intuitive facts the centring depends on: `pane split` can only
  place a new pane **right or down**, and `--ratio R` gives `R` to the **existing** pane, not the
  new one — hence the swap in step 3 of `zen::enter`.
- **`layout.apply` is destructive — never call it.** It looks like the natural way to restore a
  tab's layout, and given a tree of existing `pane_id`s it does **not** re-parent them: it builds
  a *new* tab full of *new* panes and discards the originals, processes and all (measured against
  0.8.0 — it silently replaced three live panes with fresh shells). Its sibling `layout.export` is
  read-only and safe, and is what `zen::sibling_anchor` reads. This is why zen's restore is
  `pane move` + `pane swap`, and why a deeply nested tab restores approximately: there is no
  non-destructive verb for "insert at this point in the tree". `Anchor::exact` carries that fact
  and the exit notifies rather than silently rearranging.
- **herdr's chrome has no per-pane switch, so `zen_chrome` edits herdr's own config — and that
  is the only file outside the plugin anything here may write.** The sidebar, tab row, pane
  borders/gaps and scrollbars are global `[ui]` keys; `herdr pane|tab|server|config --help` and
  the socket method list have nothing UI-shaped, and `herdr config` has no `set`, so
  `server reload-config` after a `toml_edit` rewrite is the only lever. Two orderings are load
  bearing and easy to invert: `enter` **snapshots before it writes** and only then engages (last
  of the herdr work — a reload while the splits are settling lays the gutters out against a stale
  frame), and `leave` **restores before it clears**, keeping the snapshot when herdr refuses so
  `zen chrome-restore` can retry. The snapshot is its own state file (`zen.chrome.tsv`) rather
  than part of the session record precisely because it must outlive the session. `enter` never
  re-snapshots over a leftover snapshot: that would record zen's own values as the user's and
  lose the way home for good. Default is `off`; tests must never use `Level::Full`, the same way
  they never use the real `SessionStore` path.
- **`socket.rs` is a deliberate exception to the `CommandRunner` rule, not a precedent.**
  `pane.graphics.*` is absent from `herdr pane --help` and reachable only over the socket, so the
  gutter scrim has no CLI path. Everything else must keep shelling out through `CommandRunner`,
  which is what keeps the IO edge mockable. herdr composites the scrim **opaque** regardless of the
  alpha byte, so there is no translucency knob to add — and the scrim is painted over *parked
  gutters*, never over live panes, because a pane that redraws would punch through it.
- **Version sync:** `Cargo.toml` and `herdr-plugin.toml` versions must match; `tests/manifest_spec.sh`
  enforces it. `bin/release.sh` bumps both, so bump through it rather than by hand.
- **The changelog is the release notes.** Every user-facing change adds a line to
  `CHANGELOG.md`'s `[Unreleased]` section _in the same commit_; `bin/release.sh` promotes that
  section to a dated one and tags. The tag workflow builds four native archives, generates
  `SHA256SUMS`, and publishes that section verbatim after all targets pass. Commits are not
  Conventional Commits and nothing comes from `git log` — an empty `[Unreleased]` aborts.
- **The picker never makes a network request; `usage` is the single exception.** `update.rs`
  spawns a detached `--update-check` child (own process group, no stdio) that runs `git ls-remote`
  and writes a cache; the picker only ever reads that file, so the badge lands on a _later_ launch.
  Do not "simplify" this into a thread: the picker frequently exits in under a second and the fetch
  takes several, so the cache would never be written. `git ls-remote` over the GitHub API on
  purpose — no `jq`, no 60/hour unauthenticated rate limit, no auth. Everything fails silently.
  The `usage` pane is allowed to fetch *while you watch* because it exists to answer a question
  whose only correct answer is the current one, and it is a pane you opened on purpose rather than
  a hot path — but it still never blocks the first frame: the offline provider is loaded before
  `init_terminal`, the networked one runs on a worker thread bounded by `usage.timeout_ms`, and its
  card sits in `Slot::Loading` until the answer arrives. Nothing else may follow it.
- **`usage` is the only code that reads a credential, and the token must never reach argv.**
  `argv` is readable through `ps` by every process the user owns, for the whole life of the call,
  so `Claude::fetch` hands `curl` its `Authorization` header through stdin as a `--config -` file.
  That is the entire reason `CommandRunner::output_stdin` exists — do not add a second caller
  without the same justification, and do not "tidy" the header back into a `-H` flag. The token is
  never cached, written, traced, or drawn; `the_token_reaches_curl_through_stdin_and_never_through_argv`
  in `usage.rs` pins it down by asserting the secret is absent from `runner.calls()`.
- **The account line reads credential-adjacent files, and reads exactly one field from each.**
  Codex exposes no command that prints its address, so `account_from_codex_auth` decodes the
  `email` claim from the ID token in `~/.codex/auth.json` — payload only, signature unverified on
  purpose, because the value is a label and nothing here trusts it. The access and refresh tokens
  sitting beside it in that file are read past and dropped: never log, draw, or forward anything
  from it but the address. Claude's address and plan come from `~/.claude.json`, an ordinary
  settings file. `base64url_decode` is hand-rolled and refuses padded input; do not swap in a
  base64 crate for sixty-four characters.
- **A quota card grades by the provider's word when it has one.** Claude's usage endpoint ships a
  `severity` per limit and Codex ships none, so `window_color` takes `Severity` when present and
  falls back to `usage.warn_percent`/`alert_percent` otherwise. Yes, that means two cards can
  colour by two different rules — deliberately: the provider knows what its own plan considers
  close to the edge, and a threshold invented here does not. `limits[]` and the named buckets share
  no id, so `claude_severities` joins them on the rounded percentage; a weak key whose worst case
  is two windows at the same percentage sharing a colour that is correct for both.
- **Every usage card is laid out to the same row heights.** `draw` computes one `CardRows` from the
  busiest slot and hands it to every card, because sizing per card puts a one-window provider's
  donut at a different height from a four-window provider's, and two cards that do not line up read
  as two unrelated widgets. `every_card_puts_its_rows_at_the_same_height` pins it.
- **Local time comes from `date +%z`, once per refresh.** `std` has no local-time API and there is
  no date crate here, so `local_offset` shells out through `CommandRunner` and `format_clock` does
  the civil-calendar arithmetic. An unreadable offset degrades to UTC — wrong by hours, never wrong
  about which number resets. Do not add a time zone crate for one line of one card.
- **A quota percentage is used as reported, never rescaled.** Both sources were measured on a real
  account: Codex writes `used_percent: 41.0` and the usage endpoint answers `utilization: 51.0`.
  An earlier `normalize_utilization` guessed that anything at or below `1.0` was a fraction and
  multiplied by 100 — which reads a genuine `0.8%`, the state of every plan just after its window
  rolls over, as `80%`. `clamp_percent` only clamps. Guessing a scale fails silently and in the
  alarming direction; do not reintroduce it.
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
