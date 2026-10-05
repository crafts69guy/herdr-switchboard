# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A [herdr](https://herdr.dev) plugin providing a unified switcher over running herdr **agents**,
open herdr **workspaces**, **ghq repos**, and linked Git **worktrees** in one fuzzy picker. It is a
Rust TUI (ratatui + nucleo), not an fzf wrapper. Every surface is a mode of the same binary,
selected by argv (`--menu`, `--agents`, `--usage`, `--commands`, `--ports`, `--zen`,
`--changelog`, `--settings`, `--git`; no flag is the switcher); the settings form is **also** a
floating overlay inside the switcher (⌥,), not only a pane. **`--usage` is the only mode that reads
a credential and the only in-process HTTP client** — both fenced, see the constraints below. Git
can fetch pull requests through `gh` only when that row is activated, and update checks use a
detached `git ls-remote` child. The **git menu is a herdr pane of its own** (`Prefix + g` →
`bin/git.sh` → `--git`), not an overlay on the switcher, and its selection `exec`s a review tool
over that pane. The clone flow and the review launcher (`bin/review.sh`, which runs
`tuicr`/`lazygit`), the guarded updater, and `bin/picker.sh`'s one-time bootstrap feedback are the
Bash terminal owners. The plugin needs no fzf.
See `README.md` for user-facing keybindings and configuration.

## Commands

```bash
cargo build                                  # debug binary
cargo build --release                        # what bin/picker.sh actually launches
cargo test                                   # unit tests (sorting, group filter, history parsing)
cargo test recent_sort_puts_latest_opened_first   # single test by name
bash bin/check.sh                            # complete local/CI/release gate
bash bin/coverage.sh                         # line coverage, gated at 98% (CI runs this too)
bash bin/coverage.sh --open                  # ... and open the annotated HTML report
bash bin/release.sh 0.5.0                    # cut a release (gates, bump, changelog, tag, gh release)

herdr plugin link /path/to/herdr-switchboard         # install this checkout for manual testing
herdr server reload-config                   # after touching keybindings/config
herdr plugin config-dir switchboard          # where the runtime config.toml lives
```

`bin/check.sh` is the single full-gate interface for local sessions, CI, and releases. Layout,
keybinding, or Herdr CLI changes still need manual exercise in a real Herdr session in addition to
Rust render tests.

Coverage is deliberately **not** part of `check.sh`: it needs its own instrumented build, which
would turn the ~5s gate you run constantly into a ~90s one. CI runs `bin/coverage.sh` as a separate
job. It needs `rustup component add llvm-tools-preview` and `cargo install cargo-llvm-cov --locked`;
the script prefers a rustup toolchain when one carries the tools, because a Homebrew rust can sit
first on PATH beside a perfectly good rustup toolchain and only the latter can instrument.

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
3. The TUI (`src/`) claims the terminal and draws the final Projects chrome immediately with a
   static `Standing by…` state while a feature-local worker loads the catalogue. Completion returns
   through a typed, generation-tagged effect; an empty initial result returns the same typed Clone
   outcome as a user action. `surface::run` hosts the Projects surface and guarantees terminal
   restoration before that outcome runs. Interactive accepts (clone prompt, remove confirmation,
   `ghq get -u` output) deliberately run on the restored terminal, not inside the TUI.

**Why the origin pane matters:** `split` and `pane` targets act on the captured `SWITCHBOARD_ORIGIN_PANE_ID`.
The overlay pane is _not_ the user's pane. Never guess or infer a pane/workspace/agent id — every id
must come from `herdr agent list`, `herdr workspace list`, or the captured origin.

**Module ownership:** current file ownership and module seams live only in
`docs/architecture.md`. Keep this file focused on non-obvious invariants; when moving or
splitting code, update the architecture document in the same commit and run
`bash tests/docs_spec.sh`.

**Sort vs. search:** fuzzy score always wins while a query is present; `SortMode` (recent/name/kind)
only orders the resting, no-query list. Both paths honour the `GroupFilter`. Ties break on load
order so the list stays stable.

## Non-obvious constraints

- **herdr draws the pane frame; the TUI must not compete with it.** Every plugin pane is
  `popup` or `overlay`, and herdr frames both and requires a non-empty title. So a pane
  title is a **single icon** (`󰊢`, `󰍜`, `󰆍`, `󰛳`) and the human-readable mode name goes on
  the *list panel* inside — word titles on both produced `Ports` twice, two rows apart.
  For the same reason the plugin's own borders recede in `overlay0` while herdr's frame
  keeps the accent. **Background policy belongs only to `tui::SurfaceBackground`:**
  `common.transparency = "transparent"` clears every root and floating card to the terminal
  default, while `"opaque"` fills every one with `panel_bg`. A renderer never hand-writes
  `Clear` or `.bg(panel_bg)` for a surface. `panel_bg` remains the foreground ink on coloured
  pills and mode chips in either mode. Tests in `picker.rs`, `projects.rs`, and `git.rs`
  (`the_search_box_is_captioned_search_and_the_list_carries_the_mode_title`,
  `panels_use_the_projects_pickers_border_and_caption_slots`,
  `no_panel_paints_an_opaque_background`, and the opaque-mode contracts) pin this down.
- **There is one panel frame, `tui::framed`, and every framed surface goes through it or
  through `tui::boxed`** (the same frame plus a caption, and defined in terms of it, so the
  border style is spelled once). It lived in `ui.rs` while `picker.rs` hand-rolled its own
  `Block`s, and the two drifted exactly as you would expect: accent borders instead of
  `overlay0`, and captions passed as bare `&str` so they rendered in the terminal's default
  foreground rather than `title_color`. `git.rs` drifted the same way and went further, filling
  all three of its cards with `panel_bg` — the transparency bug that shipped in 1.2.0. A new
  framed surface calls `boxed`, or `framed` when its caption slot holds something richer than a
  word (the switcher's tab strip); it does not build a `Block` itself.
- **The git menu is a pane, and a review `exec`s over that pane.** There is a manifest pane for
  the *menu* (`[[panes]] id = "git"`), but **none for the review**: `git::main` `exec`s
  `bin/review.sh` over itself the way the clone flow's `Accept::Clone` `exec`s `get.sh`, so tuicr
  inherits the pane and quitting it returns to the pane `Prefix + g` was pressed in. The pane's
  placement must stay **full-frame `overlay`** — tuicr renders into whatever window it is handed,
  and a popup would hand it a tiny one. The picker knows nothing about git: there is no `⌥g`, no
  `Accept::Git`, no `App::git`. Two entry points would drift apart, and only one of them can be
  the fast one. `tuicr` is a TUI — never run it non-interactively (it blocks); only ever in a pane.
- **Saved-review integration never takes ownership of tuicr content.** A handoff is a pointer-only,
  non-blocking effect whose inputs are the repository, session slug, and a pane id returned by
  `herdr agent list`; delivery uses `herdr agent prompt` without `--wait`. Archive state contains
  only exact session slugs and changes Switchboard visibility, never tuicr's session JSON. Comment
  bodies never enter Switchboard state, logs, notifications, or another command line.
  `docs/git-menu.md` owns the user-facing archive, target-selection, and fallback behaviour.
- **Agent handoffs share one target seam and never wait for a turn.** `agent_handoff.rs` is the only
  owner of promptable-agent parsing, same-worktree/origin selection, and `herdr agent prompt`;
  neither Git nor Projects adds `--wait`. Projects sends only JSON-escaped item kind, label, and
  absolute path as data-only context. Paths and labels never enter notifications or traces, and a
  Workspace never pretends its several pane directories are one selectable path.
- **Projects catalogue discovery is a feature-local background effect.** Startup draws the final
  Search/Context/Navigator/Preview geometry first and shows static `Standing by…`; it accepts only
  Close until the generation-tagged completion arrives. Settings Apply keeps the old rows visible
  under `Refreshing…`, locks selection-dependent actions, and restores selection by entry ID.
  There is no minimum-visible floor. Repository and worktree discovery share one `ghq list`
  snapshot, and per-repository worktree probes use at most four threads while their results are
  installed in snapshot order. An empty initial catalogue returns a typed Clone outcome so the
  terminal is restored before `bin/get.sh` takes over.
- **Projects stars are durable-entry state, not another source.** Only Repo and Worktree identities
  can be starred; Agent and Workspace IDs are live and must never enter the persistent set. The
  final Starred group filters the already-loaded catalogue and keeps its search, sort, and actions.
  `projects::stars` owns typed identity and mutation; `state` owns the private tempfile, atomic
  replace, and cross-process lock around the complete read-modify-write transaction. Writes return
  through a typed Projects effect before the reducer changes the marker or list. Missing or
  malformed state degrades to no stars and must never block the first Projects frame.
- **The pre-build cat runs before Rust exists and sizes itself from the plugin PTY.**
  `run_with_splash` reads `stty size </dev/tty` and passes those cells to `bootstrap_frame` on each
  animation frame. Do not replace it with `tput`: redirecting `tput` away from the TTY makes it
  return terminfo's 80x24 default in a wide Herdr pane. Horizontal padding is spaces; vertical
  padding is rows. `tests/bootstrap_spec.sh` pins both coordinates.
- **A list fetch runs outside `Git::on_key` and off the input thread.** `on_key` returns a `Step`;
  `GitSurface` turns `Step::Load` and `Step::CountFiles` into typed background effects, then applies
  their results from `on_tick`. That keeps the key interface IO-free and unit-testable through
  `MockRunner`, while `gh pr list` leaves the menu responsive instead of freezing a half-drawn list.
- **Nothing writes to tuicr's config.** The theme is a whole file that
  [hue-theme](https://github.com/crafts69guy/hue-theme) owns
  (`~/.config/tuicr/themes/hue-<mood>.toml`, 41 keys, none optional). tuicr **exits 2** on a theme
  it cannot fully resolve, and `--theme` on a mismatched name takes the whole review path down —
  so `bin/review.sh` never passes `--theme` and `ensure_tuicr` only checks the binary exists.
- **Bash delegates open + config to the Rust binary.** The
  clone flow (`bin/get.sh`) opens a repo with `herdr-switchboard open --target … --path … --origin …
  --label …` and reads settings with `herdr-switchboard config get <key> [default]`, so the herdr
  open verbs (`src/action.rs::open_target`) and typed config reader (`Config::load`) live in one
  place. `bin/lib.sh` keeps `ensure_built` (build-on-demand, shared by the picker and clone flow),
  `toml_get` (used only by `configure_notifications`, the pre-build notification path that must not
  depend on a cargo build), and the pane-context/JSON helpers. **A change to how a target opens
  lands only in `action.rs`.**
- **fnm activation is opt-in, local, and post-selection.** `[fnm].enabled` defaults off. Preview
  reads declarations from disk only; `fnm exec --using <repo> -- printenv PATH` runs after an open
  is accepted, never installs a version, and never performs network work. Fresh targets receive
  that PATH through Herdr's `--env` and never receive post-create terminal input; shell startup
  owns session-local initialization and must preserve the launch PATH or activate the initial cwd.
  The current-pane target remains `cd`-only and relies on fnm's standard `--use-on-cd` shell hook.
- **The Node Versions manager is the only explicit fnm mutation surface.** It loads installed
  versions locally, runs `fnm list-remote` on a worker only after the user opens the dedicated
  pane, and performs install/default/uninstall after terminal restoration. `use` is sent to the
  captured origin pane because a child process cannot change its parent shell; uninstall requires
  the exact selected version as typed confirmation.
- **Settings has two presentations, not two renderers.** The embedded form is a centred floating
  card because it overlays Projects. The standalone action already lives in a Herdr-framed popup,
  so it draws the same form full-area with a content margin and no second border or duplicate
  title. Both presentations share the columns, tabs, hit zones, hints, and command bar.
- **Configuration is typed and namespaced.** `config.rs` deserializes the section structs with
  `deny_unknown_fields`; Rust code reads those fields directly. `Config::value_for_cli` is the
  deliberately narrow compatibility seam for Bash's `config get` calls, not an internal
  lookup interface. `settings.rs` writes namespaced values through `toml_edit`, preserving comments
  and hand-added keys before validating the complete result. **Every picker has a `[keys.*]` table**
  — `projects`, `agents`, `commands`, `ports`, `zen`, `menu`, `fnm` — reached through
  `PickerMode::key_bindings` plus a `reload_config` that re-reads it; a mode that implements neither
  is one whose keys nobody can move, which is what Menu and the fnm manager silently were.
- **A click zone is measured by the loop that draws the thing.** `tab_zones` and
  `footer_zones` (`src/projects/view.rs`) are built inside the same loops that lay out the tab strip
  and the command bar, because a zone computed separately drifts the moment a label
  changes — and drifts _silently_, into clicking the wrong action. `list_state` is kept
  on the `App` for the same reason: its scroll offset is the only thing that turns a
  clicked row back into an entry, so it cannot be a fresh `ListState` per frame.
- **The cheatsheet's descriptions must fit `HELP_DESC`** (`src/projects/view.rs`) — the popup's half
  width less the key pill, around 19 columns. A longer one is cut with no ellipsis, so it
  ships looking like a shorter phrase; `wheel  Scroll whatever is under it` reached a
  README screenshot as `Scroll whatever is`. `row` asserts, and a `TestBackend` render
  test in `projects.rs` fires it.
- **A prefix names the kind of work, never the mode.** `^<key>` acts on the selected row; `⌥<key>`
  changes the view or the app (or is the heavier form of the `^` verb on the same letter, as
  Ports' `^x` TERM / `⌥x` KILL); `↵` runs the row's primary action with `^↵`/`⌥↵` as its variants.
  So **every modified chord appears in both `default_insert` and `default_normal` with the same
  action**, and Normal's bare letters are *aliases carrying the same letter*, never a respelling.
  Three tests in `keymap.rs` enforce exactly this and are the reason the layout stops drifting:
  `every_modified_chord_means_the_same_thing_in_both_modes`,
  `no_action_straddles_the_ctrl_and_alt_families`, and
  `a_bare_alias_carries_the_same_letter_as_its_chord`. Two vocabularies are carved out by name in
  `mode_idiomatic`: motion (readline in Insert, Vim in Normal) and query editing (`^u` clear,
  `⌥⌫` delete-word, Insert-only). `^u` and `^c` are reserved on every surface — no `ActionSpec`
  may take them, which is why the fnm manager's `use`/`install` sit on the `↵` ladder.
  **A chord that is somebody's multiplexer prefix never reaches the pane at all**, so opening in a
  tab is the mnemonic-free `^e`: herdr's default prefix is `^b` and `^t` is the usual tmux-refugee
  alternative, so the open group avoids both. `^b` is refused by
  `no_default_chord_takes_herdrs_prefix` and by the shared picker's check; do not "fix" `^e` into
  `^t`, and do not reclaim `^b` for anything.
  **There is no `␣` leader, and adding one back is the bug.** Space can only be a leader in Normal
  — in Insert it is a character the user is typing — so any group living there was forced to change
  prefix with the mode, which is the whole inconsistency this layout removes. The Git menu, the
  Central Menu, the settings form, and the Usage/changelog viewers sit outside the rule on purpose:
  their keys name a row, a destination, or a form field rather than a verb on a selection.
- **The shared picker answers five chords itself, and they live in one table.** `RESERVED`
  (`src/picker.rs`) holds `⌥,`, `⌥j`, `⌥k`, `⇥`, `⇧⇥`; `PickerSurface::on_key` dispatches from it
  through `reserved_for`, *ahead* of any `ActionSpec`. They used to be five hand-written `if`s that
  nothing checked against, so a mode declaring one of those chords had its action swallowed with no
  error — the same silent shape as Ports' `^w`, which shadowed delete-word for that picker alone.
  A new `ActionSpec` is therefore checked by `picker::assert_follows_prefix_concept`, which every
  mode calls from its **own** test module (the modes are private to their files, so the check
  travels to them). It refuses a bare `Char` (it would be matched ahead of the typing arm and make
  that letter untypeable), `^c`/`^u`, anything in `RESERVED`, a `key_label` that is not the chord it
  listens for, and a shared action id bound to a chord it does not carry elsewhere.
  `the_prefix_concept_check_rejects_every_shape_it_names` asserts the guard actually bites, because
  a guard that silently passes is the failure it exists to prevent.
- **One keypress gets one answer, and `same_chord` is where that is decided.** Matching weighs only
  CTRL and ALT, exactly as `keymap::chord_of` does: SHIFT is already baked into the character a
  terminal reports and terminals disagree about whether they set the bit too. `ActionSpec::matches`
  compared modifier bits with `==` while the keymap used `contains`, so the same press acted in
  Projects and did nothing in every other picker — `⇧↵` ran the row in one and was inert in the
  other, and a `^⇧` chord missed its `^` action outright. Because SHIFT no longer separates two
  chords, `assert_follows_prefix_concept` also refuses two specs that normalise to the same chord;
  without that rule the second one would simply never run.
- **Keys are a config-driven keymap, not hardcoded `match` arms.** `handle_key` resolves a
  `Chord` through `App::keymap` (`src/keymap.rs`) and runs `apply_action`. Two ordered tables
  (Insert + Normal); typed `common.keymode` defaults to Normal and `esc` toggles Insert↔Normal.
  `[keys.projects]` entries rebind actions (first chord wins as the shown one) into **both** tables,
  so an override cannot reintroduce a per-mode split.
  **The footer (`draw_footer`) and the cheatsheet (`draw_help`) render from the keymap via
  `Keymap::label_for(mode, action)`**, so both re-label per remap — never hardcode a
  key cap in either; add the action to the curated list and it picks up its live chord. A row
  whose action is unbound in the current mode drops out. `label_for` returns the *first* matching
  chord, which is why a bare alias is listed after the chord it shadows.
  Adding an action is one row in `keymap::NAMES`, its default chord in **both** tables, and an
  `apply_action` arm; a new `Accept` also needs the footer curated list and `dispatch`. Cheatsheet
  descriptions must still fit `HELP_DESC`.
- **One frame absorbs every event already queued, and a list row borrows rather than copies.**
  These two are one fact from opposite ends. A terminal delivers a wheel turn or a held key as a
  burst, and `surface::run` used to answer each with its own full repaint; meanwhile `draw_list`
  (`projects/view.rs`) and the shared picker's row builder cloned every column of every *filtered*
  entry per frame — including the rows a `List` scrolls past and never paints, over a Commands
  catalogue that is `commands.history_limit` (5,000) rows deep by default. So a burst multiplied a
  per-entry cost that was already linear. `drain_frame` now takes queued input up to
  `MAX_COALESCED_EVENTS` before repainting — the cap is what stops a paste from starving the
  screen — and rows borrow out of `state.items` / `picker.entries`, which is why both draw
  functions destructure their `&mut` state into disjoint field borrows and why the shared picker
  moves its `ListState` out with `mem::take` before building rows. Padding comes from
  `tui::spaces`, a slice of one static. Do not "tidy" a row builder back to `to_string()`: it
  compiles, it looks identical, and it silently restores the whole cost.
- **`Stars::contains` and the tab counts must not allocate or rescan.** `contains` is asked once
  per entry per keystroke by the reducer, once per entry per group by the Context panel, and once
  per entry per frame by the list. It answers from `StarSet`, which stores one ID set *per kind* so
  the probe is a borrowed `&str`; a `BTreeSet<StarKey>` cannot, because building the probe key
  means cloning the entry's ID. `StarKey` stays the on-disk shape, so the file is unchanged.
  Tab counts are cached on `Picker` and rebuilt by `recount` at the three points that can
  invalidate them — construction, `replace_stars`, `replace_entries` — never in `recompute`, which
  runs per keystroke and cannot change them. A stale cache here fails silently, so
  `tab_counts_are_rebuilt_whenever_the_entries_or_the_stars_change` checks every tab against a
  fresh scan.
- **A probe that a `read_dir` can rule out must not be a process.** `may_have_worktrees`
  (`data.rs`) asks whether `.git/worktrees/` has anything in it before spending a `git worktree
  list` on a repository, because that probe runs once per *ghq repository* — hundreds of forks for
  the handful of repositories that actually have linked worktrees. It **fails open**: a `.git`
  *file* (this path is itself a linked worktree or a submodule) and an unreadable directory both
  still probe, since a wrong "no" loses a worktree from the catalogue silently while a wrong "yes"
  costs one subprocess. `the_worktree_prefilter_only_refuses_what_the_filesystem_settles` pins
  every case.
- **Independent reads that each pay a process start are overlapped, and only those.**
  `projects/effect.rs` overlaps `ghq root` with `ghq list`, `source.rs` overlaps `herdr agent list`
  with `herdr workspace list`, and `repo_card` (`projects/preview.rs`) fans out branch, dirty
  state, last commit, and the `preview.sh` tree so the card costs the slowest rather than their
  sum. This is only sound because every one of them is an independent *read*. Nothing that mutates,
  and nothing whose result another call depends on, goes in a scope like these.
- **`state.rs` owns every durable write, and a read-modify-write is one locked transaction.**
  Five callers used to hand-roll `fs::write` to a **fixed** `.tmp` sibling and rename — so two
  Switchboard processes writing the same file raced on one temp name, and `history` and
  `review_archive`, which load-modify-write, silently erased each other's rows. All of them now go
  through `state`: `write_private` / `update_private` for this plugin's own state (owner-only), and
  `replace_atomically` for a file the *user* owns — its own `config.toml` and, for zen chrome,
  herdr's — which gains the lock and the unique tempfile but keeps the mode its owner gave it. A
  new persistent file adds no sixth implementation.
- **The mouse is turned on by hand, and must be turned off on every exit path.** `surface.rs`
  writes `?1000h`/`?1006h` itself rather than using crossterm's `EnableMouseCapture`, which
  also enables any-event tracking (`?1003h`) — every pointer move would wake the loop into
  a redraw for an event we discard. `?1000h` reports the wheel *and* buttons, which is
  exactly what every surface consumes; drags stay herdr's, which runs with
  `mouse_capture = true`. The universal host pairs claim/restore and
  chains the disable ahead of the panic hook `ratatui::init` installs,
  since that hook restores the screen but knows nothing about the mouse. Leaving it on
  drops mouse escapes into the user's shell. **Every new terminal mode implements `Surface` and
  runs through `surface::run`; it never calls `ratatui::init` or reads crossterm events itself.**
- **A click selects; a click on the already-selected row acts.** Terminals report no
  double-click (crossterm gives `Down` only), so single-click-to-run would let a stray
  click `exec` tuicr over a pane or write a setting, and a timestamp heuristic would put
  the same guesswork in five places. A command-bar pill carries the **key printed on its
  cap** as its payload and routes through `on_key`, so the label cannot drift from the
  behaviour. The rule is the same in `projects.rs`, `picker.rs`, `git.rs` and `settings.rs`;
  what is shared between them is `tui::zone_at`, the lookup — never the measurement, which
  stays in the loop that draws the thing.
- **Picker command bars may wrap, but their hit zones wrap with them.** `PickerMode` defaults to
  one action row; the Central Menu requests two because it exposes every direct route. The picker
  balances whole pills by their rendered widths, leaves one terminal row between wrapped rows,
  measures each row with `tui::pill_row`, and stores the row coordinate beside its zones. Never
  split or reposition the spans independently of those zones.
- **The preview clips; it must never wrap.** Every body goes through `clip`/`clip_line`
  (`src/projects/preview.rs`) so one card line is exactly one screen row — that is what makes
  `preview_scroll` mean what it says and `preview_len`/`preview_rows` bound it correctly.
  `draw_preview` therefore has no `Wrap`. Re-adding one, or emitting an unclipped line,
  breaks the scroll silently: the offset drifts from the content instead of erroring. The
  pane's width reaches the worker through `App::preview_width`, published by `projects/view.rs`, which is
  why `ProjectsSurface::after_draw` requests a preview only after the host has drawn geometry.
- **Nothing uses `jq` — keep it that way.** No code path shells out to it: the bash layer reads
  herdr's JSON with the awk-based `json_string_value` / `json_bool_value` in `bin/lib.sh`, and the
  Rust layer uses `serde_json` (`data.rs`, `projects/preview.rs`). It is not a documented requirement, so a
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
  lose the way home for good. Default is `off`. Tests may exercise every level, because the test
  build points `chrome::config_path` at its scratch directory (see the coverage rule below); a
  test that writes that scratch herdr config holds `CHROME_LOCK` in `zen.rs`.
- **`socket.rs` is a deliberate exception to the `CommandRunner` rule, not a precedent.**
  `pane.graphics.*` is absent from `herdr pane --help` and reachable only over the socket, so the
  gutter scrim has no CLI path. Everything else must keep shelling out through `CommandRunner`,
  which is what keeps the IO edge mockable. herdr composites the scrim **opaque** regardless of the
  alpha byte, so there is no translucency knob to add — and the scrim is painted over *parked
  gutters*, never over live panes, because a pane that redraws would punch through it.
- **Two build profiles, because two consumers want opposite things.** `release` stays
  `opt-level = "s"` with no LTO: it is what `bin/picker.sh` falls back to for an offline install or
  a linked checkout, compiled on the user's machine while they wait for a pane. `dist` (`opt-level
  = 3`, thin LTO, one codegen unit) is what `.github/workflows/release.yml` ships, where the
  compile time is CI's and the run time is the installer's. Archives are packaged from
  `target/<triple>/dist/`; `bin/lib.sh` still builds and reads `target/release/`. Do not collapse
  them — either the local fallback gets eight times slower or every shipped binary gets slower.
- **Nothing is excluded from the coverage denominator, and the gate is 98%.** Some code genuinely
  cannot run under a unit test — `surface::run` and `preroll` claiming a real terminal, the
  `exec` that replaces the process, a stdin prompt's wrapper — and it still counts against
  `MINIMUM` in `bin/coverage.sh`. An exclusion list (or `#[cfg(not(test))]`, which is one in
  disguise) is a second thing to argue about and a place for the number to quietly stop meaning
  what it says; the answer to a low file is a test, or an honest seam. The seams that exist are
  narrow and each replaces something a test has no business starting: `surface::Host`
  (`TerminalHost` in production, `ScriptedHost` driving the real host loop on a `TestBackend`),
  injected runners on every mode and background step, `fn` pointers for the clipboard, URL
  opener, typed confirmations, process replacement, and Zen's chrome restore, prompts that read
  any `BufRead`, `PortWorker::seeded`, and `CatalogWorker::disconnected`. A test that would fire a
  real notification uses `Notifier::silent`.
- **Tests never touch the developer's machine state.** In the test build, `state::state_dir`, the
  plugin's `config::config_path`, `chrome::config_path` (herdr's config), and the herdr socket all
  resolve under `state::test_scratch()`, one directory per test process, and `trace` always writes
  to a scratch log. A background step once reached the real `review_archive::set` from a test and
  left a fixture slug in the developer's archive; that class of leak is now structurally
  impossible. The real path rules are pure `*_from` functions tested on their own. Never add a
  test that reads or writes `$HOME` directly — take the directory as a parameter instead, as
  `codex::load_from` and `claude::load_with` do.
- **Version sync:** `Cargo.toml` and `herdr-plugin.toml` versions must match; `tests/manifest_spec.sh`
  enforces it. `bin/release.sh` bumps both, so bump through it rather than by hand.
- **The changelog is the release notes.** Every user-facing change adds a line to
  `CHANGELOG.md`'s `[Unreleased]` section _in the same commit_; `bin/release.sh` promotes that
  section to a dated one and tags. The tag workflow builds four native archives, generates
  `SHA256SUMS`, and publishes that section verbatim after all targets pass. Commits are not
  Conventional Commits and nothing comes from `git log` — an empty `[Unreleased]` aborts.
- **No launch or navigation hot path waits on the network.** `update.rs` spawns a detached
  `--update-check` child (own process group, no stdio) that runs `git ls-remote` and writes a cache;
  the Projects Picker only reads that file, so the badge lands on a _later_ launch.
  Do not "simplify" this into a thread: the picker frequently exits in under a second and the fetch
  takes several, so the cache would never be written. `git ls-remote` uses Git's HTTPS transport
  on purpose — no `jq`, no 60/hour unauthenticated API limit, no auth. Everything fails silently.
  Git's pull-request row may call `gh pr list`, but only after explicit activation and through a
  background effect. Clone/update commands may fetch because the user invoked that work directly.
  The `usage` pane is allowed to fetch *while you watch* because it exists to answer a question
  whose only correct answer is the current one, and it is a pane you opened on purpose rather than
  a hot path — but it still never blocks the first frame: the offline provider is loaded before
  the terminal is claimed, the networked one runs on a worker thread bounded by
  `usage.timeout_ms`, and its card sits in `Slot::Loading` until the answer arrives. No other
  surface may read a credential or add an in-process HTTP client.
- **`usage` is the only code that reads a credential, and the token must never reach argv.**
  `argv` is readable through `ps` by every process the user owns, for the whole life of the call,
  so `Claude::fetch` hands `curl` its `Authorization` header through stdin as a `--config -` file.
  That is the entire reason `CommandRunner::output_stdin` exists — do not add a second caller
  without the same justification, and do not "tidy" the header back into a `-H` flag. The token is
  never cached, written, traced, or drawn; `the_token_reaches_curl_through_stdin_and_never_through_argv`
  in `usage.rs` pins it down by asserting the secret is absent from `runner.calls()`.
- **The account line reads credential-adjacent files, and reads only named, non-secret labels.**
  Codex exposes no command that prints its address, so `identity_from_codex_auth` decodes the ID
  token in `~/.codex/auth.json` — payload only, signature unverified on purpose, because these are
  labels and nothing here trusts them. It takes exactly two claims: `email`, and
  `chatgpt_subscription_active_until` under the namespaced `https://api.openai.com/auth` claim
  (the `renews` row). One read and one decode for both, which is why `codex_account` became
  `codex_identity`. The access and refresh tokens sitting beside them in that file are read past
  and dropped: never log, draw, or forward anything from it but those two values. Claude's address
  and plan come from `~/.claude.json`, an ordinary settings file. `base64url_decode` is hand-rolled
  and refuses padded input; do not swap in a base64 crate for sixty-four characters.
- **A renewal date is shown only when it is a fact, never when it is arithmetic.** Codex publishes
  one; Anthropic publishes none anywhere local — `~/.claude.json` carries `subscriptionCreatedAt`
  and `billingType: apple_subscription`, and the usage endpoint carries no period end, so a Claude
  renewal could only be a monthly-anniversary guess against a date Apple actually owns. The card
  says `unknown` instead. `format_renewal` also answers `unknown` for a date already **past**: the
  Codex claim only refreshes while Codex runs, so a machine left alone for a month still holds the
  previous period's date, and a stale date under a heading that says *renews* reads as one that is
  coming — wrong in the reassuring direction, which is the failure this popup exists to prevent.
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
  as two unrelated widgets. `every_card_puts_its_rows_at_the_same_height` pins it. The fact lists
  are built once, by `card_facts`, and used for *both* the count and the render — a row that the
  drawing layer appends (`renews`) but the sizing layer does not count is a card one row too short.
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
