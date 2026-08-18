# Changelog

All notable changes to this plugin are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A Usage popup.** Bind `switchboard.usage` or open it from the central menu to see how much of
  each AI subscription is spent and when it resets. Each agent gets a donut for the window closest
  to running out, a bar for every other window, and the numbers that give the percentage its
  context: what the session spent, how much of the context window the last turn used, whether
  credits are on. Every card dates itself — `as of 12m ago` — because Codex reports whatever its
  last session wrote, and a two-day-old percentage read as current is the mistake this popup exists
  to prevent. Resets are given both ways, `in 1d 12h` and `resets 08:53 Wed`.

  Codex reads the exact numbers OpenAI returns, straight out of its own session log, with no
  network at all. Claude Code publishes nothing to disk, so its card asks the same endpoint the
  in-session `/usage` command asks, using the OAuth token Claude Code already stores; that request
  runs on a worker with a timeout, so the popup opens instantly with Codex on screen and fills
  Claude in when it lands. Claude also grades its own limits, and the card colours them the way the
  provider grades them rather than by a threshold of ours; anything ungraded falls back to
  `usage.warn_percent` and `usage.alert_percent`.

  Each card names the account it is reporting on — the two agents are routinely signed in as two
  different people, and a quota means little without knowing whose it is. Codex's address comes
  from the ID token it stores, Claude's from the profile Claude Code caches, and that profile is
  also where the Claude card gets its plan, since the usage endpoint names none.

  Press `r` to read both again. Any agent that cannot be read says why on its own card and leaves
  the other one alone. Choose the providers and thresholds under `[usage]`, or from the settings
  overlay.

- **Zen can hide Herdr's own chrome.** The new `zen.chrome` setting (`off` by default, or
  `panes` / `full`) suppresses the pane borders and gaps, the scrollbar column, the tab row and
  the sidebar for the length of a zen session. None of those have a per-pane switch, so anything
  but `off` rewrites the matching `[ui]` keys in Herdr's *own* `config.toml` and reloads Herdr,
  then restores them exactly on the way out: comments and unmanaged keys survive, keys already at
  the wanted value are left alone, and your untouched config is copied to
  `$XDG_STATE_HOME/herdr-switchboard/herdr-config.backup.toml` before the first write. If a
  restore fails or Herdr is killed mid-session, `herdr-switchboard zen chrome-restore` puts it
  back. Herdr only applies `sidebar_start_collapsed` on its next launch, so `full` may leave the
  sidebar in place — zen measures that and says so rather than failing silently.

- **Zen mode.** `switchboard.zen-toggle` gives the current pane the screen: it moves to a tab of
  its own, centred at `zen.width` (default 70%) between two dimmed gutters, and toggling again
  puts it back where it was. `switchboard.zen` opens a picker to zen any pane instead of the
  current one. The gutters are dimmed by painting over them through Herdr's graphics API, which
  needs `[experimental] kitty_graphics = true`; without it the gutters are simply blank. Tune it
  with `zen.width`, `zen.scrim`, and `zen.scrim_color`, or from the settings overlay. Neighbouring
  panes are never touched — only the zen'd pane moves, and its running process survives the move.
  Leaving zen restores a two-pane tab exactly; a more deeply nested tab gets every pane back beside
  its former neighbour, with a notification when the split nesting could not be reproduced.

- **An AI Agents picker.** Bind `switchboard.agents` or open it from the central menu to list the
  AI integrations installed in Herdr. `enter` starts the selected AI in the origin pane, `ctrl-t`
  starts it in a new tab, and `alt-w` starts it in a new workspace; failed starts clean up any
  tab or workspace Switchboard created. The focused picker opens as a compact centered popup rather
  than a mostly empty full-screen overlay. This requires Herdr 0.8.0 or newer.

### Changed

- **The README is now a concise entry point instead of a monolithic reference.** Quick start,
  feature discovery, core actions, and namespaced configuration stay on the front page; detailed
  keybindings, safety behaviour, Zen, Git, configuration, and architecture now live in focused
  guides under `docs/`. Stale pre-1.0 wording, flat configuration names, dependency scope, popup
  placement, update-version examples, and contributor checks were corrected at the same time.

### Fixed

- Opening AI Agents or another popup from the Central Menu now waits for the menu popup to close,
  so Herdr no longer rejects the replacement as `popup already open`.
- The AI Agents picker now closes immediately after a selection while startup readiness is checked
  in the background; failed starts still clean up new tabs or workspaces and report the error with
  a notification.
- Linked development checkouts now rebuild their release binary when Rust sources or Cargo metadata
  are newer, instead of silently running an old same-version binary after source changes.

## [1.0.0] - 2026-08-04

### Breaking

- Removed the old `ghq` configuration/state migration and flat-config compatibility path;
  Switchboard now reads only namespaced configuration from its own plugin directory.

### Changed

- The Menu, Commands, and Ports pickers now render in the Projects picker's visual
  language: borders recede in `overlay0` instead of shouting in the mode accent, every
  panel caption uses `title_color`, and the selected row is a subtle `surface1` bar
  rather than a full-width inverted accent block.
- Those three panes no longer repeat their name: herdr already draws the pane frame and
  its title, so the pane title is now a single icon (as Projects and Git already were)
  and the mode name moved onto the list panel.
- All three panels are transparent, so the terminal shows through consistently. The
  Search and Preview panels previously painted an opaque `panel_bg` while the list did
  not, which read as two different themes inside one pane.
- The Preview panel gained the Projects picker's `⌥jk n/total` scroll indicator, shown
  only when the card runs past the fold, and its scroll now clamps to the content.
- Picker rows are laid out against the panel's real width. A row longer than the panel
  now ends in an ellipsis instead of being cut mid-word wherever the border fell, and a
  row's trailing tag sits in its own right-hand gutter instead of a column that went
  ragged as soon as one entry ran long.
- Commands stopped printing `shell` on every row — it was the source of all but a handful
  of entries. The badge now appears only for a preset or a Switchboard command, and the
  right-hand gutter carries how long ago the command was last used (`2h`, `3d`), which is
  what the list is sorted by.
- Commands draws each row's leading word — the program — in the entry's colour, bold, so
  a wall of history has something to scan down, and gives the list 58% of the body rather
  than 42, since its rows are long and its preview card is short.
- The Commands preview reports `last used` as a relative age plus a readable UTC date
  instead of a raw unix timestamp, and labels the selection count as `n ×`.

## [0.11.0] - 2026-08-04

### Breaking

- Renamed the package, binary, plugin id, release assets, runtime environment variables, and state
  directory from **herdr-ghq / `ghq`** to **herdr-switchboard / `switchboard`**. Existing flat
  configuration and state are migrated once, validated, and then removed; no legacy alias or backup
  is retained. Change `ghq.menu` bindings to `switchboard.projects`.

### Added

- Added the searchable `switchboard.menu` central menu and direct actions
  `switchboard.projects`, `switchboard.commands`, `switchboard.ports`, and
  `switchboard.settings`, so every picker can have its own Herdr key binding.
- Added a Commands picker that merges shell history, presets, and Switchboard selection history;
  supports quoted/negated field filters; excludes likely secrets; and can fill, run, copy, or forget
  an exact command. Multiline execution requires typed confirmation.
- Added a native Ports picker for live TCP listeners with process metadata, automatic refresh,
  URL/copy/workspace actions, and identity-checked TERM/KILL confirmations.
- Added four package Settings tabs (`Common / Projects / Commands / Ports`) and semantic Herdr
  notifications for command-delivery and listener-signal outcomes.

### Changed

- Configuration is typed, namespaced TOML. Commands and Ports share a colorful picker engine with
  stable selection, mode-specific remaps, inline filter diagnostics, mouse support, preview, and a
  live command bar.

## [0.10.0] - 2026-08-03

This release **replaces `hunk` with [`tuicr`](https://github.com/agavra/tuicr)** and moves the git
menu out of the switcher into a pane of its own. It is a breaking change: keys move, two menu rows
are gone, and `tuicr` is now a hard requirement (`brew install tuicr`, ≥ 0.20.0). Themes for it come
from [hue-theme](https://github.com/crafts69guy/hue-theme) — install that first, or reviews open in
tuicr's own colours.

### Removed

- **`⌥g` no longer opens a git menu in the switcher.** The menu is a herdr pane now, reached only by
  the `ghq.git` action (bind it to `prefix+g`). What you lose with it: reviewing an arbitrary repo
  you fuzzy-jumped to. `cd` there (`^o`) and press `prefix+g`. Two entry points would have drifted
  apart, and only one of them could be the fast one.
- **`s` review staged** — tuicr has no staged-only flag, `d` (`tuicr -w`) already covers staged and
  unstaged together, and lazygit is the better pre-commit surface.
- **`x` resolve conflicts** — tuicr's `-p` takes a single path prefix, so `hunk diff -- <files>`
  has no translation.
- **The commit history browser** — the scrolling, fuzzy-filterable `git log` list behind `h`. tuicr
  has a commit panel of its own, so `h` now opens that directly.
- **The startup animation.** The switcher no longer animates a cat while it loads.
- **`hunk` is no longer used or configured**, and the plugin writes nothing into tuicr's config.

### Added

- **A dedicated git menu pane.** `prefix+g` opens it straight onto the repo the pane is sitting in;
  it loads no agents, no workspaces, no repository index, and no preview on the way. Picking a row
  takes over that pane, and quitting the review returns you to where you started.
- **`a` — review all files** (`tuicr -A`).
- **`p` — review a pull request**: `gh pr list` in a fuzzy-filterable list, then `tuicr pr <n>`. The
  row is hidden when `gh` is not installed.
- **`r` — saved reviews**: the sessions from `tuicr review list`, with their comment counts. Note
  this **reads comments only** — tuicr's TUI cannot reopen a session, so there is no way back into a
  half-finished review yet.
- **`GHQ_TRACE=1`** writes tab-separated timings to `$XDG_STATE_HOME/herdr-ghq/trace.log` (or
  `$GHQ_TRACE_FILE`) — first list, keystroke-to-frame, and preview render, each measured
  separately. Never to stdout or stderr. See the README.

### Changed

- **The switcher opens about 10× sooner** — roughly 35 ms to the first list where it used to be
  430 ms. Almost all of that was the animation's own 420 ms minimum-visible floor, not the work: the
  sources are now read synchronously before the terminal is claimed, so the first thing painted is
  the list. A machine with thousands of repositories will see a blank pane for slightly longer
  before it appears.
- **Reviews open in `tuicr`, not `hunk`**: `d` → `tuicr -w`, `b` → `tuicr -r <base>.. -w`,
  `h` → `tuicr`. Every launch passes `--no-update-check`.
- The preview card is **about twice as fast** — roughly 50 ms on average where it used to take
  90 ms, and 65 ms at the tail where it used to reach 214 ms. The branch now comes from reading
  `.git/HEAD` rather than spawning `git symbolic-ref`, and the dirty check no longer walks untracked
  files. **Behaviour change:** a repository whose only changes are untracked files now reads as
  `clean`.
- The **git menu is sized to fit its command bar**, so a short menu no longer clips `esc close` to
  `esc clo`, and every row carries an icon.

### Fixed

- **Enter on an agent now actually switches to it, and the agent preview loads again.** herdr keys
  `agent focus`/`agent get` on the pane id, but the switcher was passing the terminal id, so every
  agent focus failed silently (`agent_not_found`) — the picker closed and left you on the current
  agent — and the preview card came up empty. Agents now carry their pane id as the target.
- Opening the git menu **no longer prints a stray commit SHA** onto the screen beneath the card.
  Base-branch detection ran `git rev-parse --verify` through the terminal-inheriting `status` path,
  and `rev-parse` echoes the resolved SHA on success; it now captures the output instead.
- The **Kitty/herdr graphics startup splash (the animated GIF cat) is gone**, along with the
  `[experimental].kitty_graphics` dependency. herdr 0.7.5 changed its socket API to serve one
  request per connection and close it on reply, which stranded the image on screen.

## [0.9.0] - 2026-07-22

### Added

- An animated **typing cat now appears as soon as the switcher opens**: compatible
  Kitty/Ghostty/WezTerm panes use the embedded `cat-typing.gif` frames when Herdr's experimental
  Kitty graphics proxy is enabled, while small or unsupported panes automatically keep the
  theme-coloured pixel-art fallback. Both paths preserve the terminal's transparent background;
  the image is cleaned up before picker content appears and
  its first frame is always shown for a 420 ms minimum, making 2–3 animation steps visible even
  when loading finishes immediately. Agents, workspaces, repositories, and linked worktrees
  load in the background. Inside Herdr, frames use its acknowledged pane-graphics API so a
  rejected image immediately reveals the pixel-art fallback instead of leaving an empty splash;
  direct terminal launches retain standard Kitty commands. Managed
  installs fetch a checksummed native binary for macOS/Linux on arm64/x86_64, so first open no
  longer waits on Cargo; offline and linked checkouts still fall back to a local build, with
  the same cat keeping the pane responsive. `esc` or `ctrl-c` cancels either loading stage.
- **Launching a `⌥g` review now shows the same typing cat while it opens.** A hunk review used to
  hand the pane a frozen screen while `hunk diff` walked a large repository; the review launcher
  now plays a short branded pre-roll that animates the cat (Kitty frames or the pixel-art fallback)
  while it warms the exact diff hunk is about to read, so the review opens onto the splash instead
  of a stall and hunk itself starts faster off the warmed cache. The cat stays up until the warm-up
  finishes (a 420 ms floor keeps it visible on an already-warm repo, a 5 s cap keeps a pathological
  one from stalling); `esc`, `enter`, or `ctrl-c` skips straight to the tool. The pre-roll hands off
  without a blank flash: it freezes a static “Opening review…” frame and leaves the screen in place
  for hunk to paint straight over, rather than tearing the terminal down between the two. Only hunk
  reviews get it — `lazygit` and custom `menu.conf` commands bring their own startup.
- A **Worktrees** tab now lists linked Git worktrees across every ghq repository without
  duplicating the main checkout already shown under Repos. Worktrees open in a workspace,
  tab, split, or pane and use the built-in Git menu at their own path; repo-only update and
  remove actions stay hidden. `include_worktrees` controls the source and defaults to `true`.
- `default_tab` chooses the active startup group (`all`, `agents`, `workspaces`, `repos`, or
  `worktrees`). Applying it in the settings overlay switches immediately; an empty, disabled,
  or unrecognised group safely falls back to All.

### Changed

- **The Workspaces preview is now a dashboard.** Instead of a bare tab list, it shows a
  pane/agent summary with a colour-coded status breakdown, the running agents with their
  status and current task, and the distinct repositories their panes sit in with each one's
  branch and a dirty marker — read from `pane list` so it reflects what the workspace is
  actually running.

## [0.8.0] - 2026-07-21

### Added

- **The git workflow is built in now — the separate `git-hub` plugin is folded into the
  switcher.** `^g` (Insert) or `␣g` (Normal) opens a git menu **overlay** over the list — the
  same floating-card shape as `⌥c`/`⌥,` — for the highlighted repo (or the pane you launched
  from). From it: review the **worktree**, **staged** changes, a **branch** (against an
  auto-detected `main`/`master`/`origin/*` base, or a pinned `base_branch`), or pick a commit
  from **history**; **resolve conflicts**; or drop into **lazygit** to stage. Reviews open in
  [`hunk`](https://github.com/modem-dev/hunk), a review-first terminal diff viewer, themed from
  your herdr `[theme.custom]`. `prefix+g` binds to the new `ghq.git` action to open the menu
  directly. Custom rows still come from `menu.conf` (`key|icon|label|command`).

- **Notifications can play a sound now.** A new `notification_sound` setting (`⌥,` →
  Notifications, or the config key) picks the toast sound: `auto` (default) fits the sound to
  the event — a `done` chime when a clone or self-update succeeds, a `request` tone when a
  clone fails or needs attention — while `none`, `done`, or `request` force one sound for
  every toast.

### Changed

- **Settings is now a floating overlay inside the switcher, like the `?` cheatsheet and the
  `⌥c` changelog.** `⌥,` (or `␣,` in Normal) draws it as a centred, rounded, two-column card
  **over** the list — so opening settings no longer replaces the whole picker, and closing it
  puts you back where you were. The highlighted row's hint is spelled out along the bottom.
  A `settings` pill now sits in the command bar, and clicking it opens the same card.

- **Settings changes are drafts now — nothing is written until you apply.** Cycling a value
  stages it (a peach `●` marks each changed row and the title shows `● unsaved`); `a` applies
  the whole draft to `config.toml` at once, and `esc` discards it. Previously every `↵` wrote
  to disk immediately.

- **Applying settings now takes effect in the running switcher**, not just on the next launch:
  `a` re-reads the config and re-derives the live state — the list re-sorts, the source toggles
  and label style reload, the preview and colours update, and key rebinds apply on the spot.

### Removed

- The standalone `ghq.settings` herdr action (and its pane) is gone: settings lives only in the
  switcher now, the way `remove` always has. Reach it with `⌥,`, the `settings` command-bar
  pill, or `?` → Settings.

- The cross-plugin `git-hub` handoff (`^g` used to open a tab and invoke `git-hub.menu`) is gone
  — the git menu is served in-process now. **Migrating:** the `git-hub` plugin is retired;
  `herdr plugin uninstall git-hub`, move `prefix+g` to the new `ghq.git` action, and install
  `hunk` (`brew install hunk`, or `npm i -g hunkdiff`) for the review pane. `nvim`/`codediff.nvim`
  are no longer required.

## [0.7.0] - 2026-07-20

### Added

- **A Telescope/LazyVim keymap: modal, remappable, and self-documenting.** The picker
  opens **typing** (Insert) and `esc` drops to a Vim **Normal** mode — bare `hjkl`/`gg`/`G`
  move, `i` or `/` return to Insert, the frequent opens sit on unshifted keys (`t`/`v`/`o`/`w`),
  and a **`␣` leader** groups the rest (`␣g` git, `␣u` update, `␣x` remove, `␣c` clone). A
  `NORMAL` / `INSERT` tag marks the mode. Insert is leaner and fixes the old readline traps:
  `^u`/`^w` now clear the line / delete a word, split moved off the XOFF-eating `^s` to `^v`,
  and update-repo is `^r`. Every binding is a `chord → action` entry you can rebind with
  `keys.<action> = "chord"` in `config.toml`, and **the command bar and the `?` cheatsheet
  render from the live keymap** — they always show your actual keys for the mode you're in.
  `keymode = "normal"` opens Vim-first. The settings dashboard is reachable from the picker
  now too (`⌥,`, or `␣,` in Normal), and appears in `?`. See `examples/config.toml` for the
  action names.

- **The settings dashboard is restyled to match the `?` cheatsheet** — settings are grouped
  into sections with title-coloured headings, each value shows as a filled pill (the selected
  one pops in the title colour, with a `▌` marker), and the list scrolls to keep the current
  setting in view. `preview_size` is now an adjustable setting, and `preview_position` gains
  `up` / `left`.

- Both plugin actions are now reachable from the switcher itself: `⌥c` reads the
  changelog and `⌥u` updates the plugin, alongside `^u` which updates the highlighted
  _repo_. Both are listed under `?`.

  `⌥c` draws over the list rather than replacing it, so reading what changed does not
  cost you your place — `esc` puts you back on the same entry. It shares the parser and
  renderer with the `ghq.changelog` pane, so the two cannot drift apart.

- **The switcher takes the mouse.** Click an entry to select it, a group tab to filter,
  and a pill on the command bar to run that command on the selection — the pills were
  always the list of what the keys do, so they are now the buttons for it too. A click
  dismisses a popup the way any key does. Nothing needs the mouse: every action still has
  its key, and the pills still say which.
- **The mouse wheel scrolls the pane under the pointer** — the card over the preview,
  the list anywhere else. The switcher asks for wheel and button reporting only, not the
  pointer motion crossterm's mouse capture would also turn on, so drags stay herdr's.
- **The preview scrolls**, with `⌥j` / `⌥k` — the `⌥` echo of the `^j` / `^k` that move
  the list, so the two panes move under the same fingers. The pane says `⌥jk 24/64` while
  there is anything below the fold, and stays quiet when the card fits. A card is 60-odd
  rows once an agent's output is in it, so most of it used to be simply unreachable: the
  scroll offset existed in the code but nothing was ever bound to it.

### Changed

- **An agent's output keeps the agent's colours.** herdr can hand back the escape
  sequences from the agent's own screen, so its diffs, syntax highlighting, and status
  line now read in the preview the way they read in the pane, instead of as flat text.
- **A README is rendered as markdown**, not dumped: headings in the title colour, bullets
  marked, inline `code` and `**bold**` styled, and links flattened to their text — a pane
  this narrow has no room for a URL, and the badges at the top of a README are mostly URL.
  It shares the renderer with the `⌥c` changelog popup.
- **The whole README is there**, where the card used to stop at 30 lines. That cut dates
  from a preview that could not scroll, when anything past the first screen was
  unreachable anyway; with `⌥j`/`⌥k` it only hid the text you had scrolled down to read.
  A 400-line bound remains for pathological files, and a card that hits it now says how
  many lines it left out rather than ending as if the README did.
- The preview is now a **card**: a header carrying the entry's name and its state as a
  filled pill, a column of aligned `label   value` rows, then each body under a
  captioned rule. It is drawn from your herdr `[theme.custom]` colours like the rest of
  the switcher, where it used to hardcode its own — a status pill here is now the same
  colour as that entry's bullet in the list, and the tab marker is the same `▌` the list
  marks its selection with.
- A **workspace preview lists its tabs** — each with its live status, pane count, and a
  marker on the active one. It only ever showed counts before.
- An **agent's recent output is clipped to the preview pane** instead of wrapped. The
  output arrives at the _agent's_ pane width, which is far wider than the preview, so
  wrapping shredded every line into fragments. Blank runs are collapsed too, so what you
  see is the output rather than the empty half of somebody's screen.
- **`jq` is no longer a requirement of any kind.** The preview was the last thing that
  called it; agents and workspaces are now read with `serde_json`, as the switcher's list
  already was. Nothing in the plugin shells out to `jq` any more, so it has been dropped
  from the requirements — including the claim that agents and workspaces needed it, which
  had not been true of the list itself for some time.

### Fixed

- **The agent preview showed raw JSON** instead of the agent. herdr nests the record
  under `result.agent`, and the preview read `result.agent_status` — which is not an
  error, just absent — so it printed the whole envelope as the agent's name and
  `unknown` as its status. The workspace preview had the same fault, and its tab list
  had been reading a field that does not exist. Agents and workspaces are now parsed in
  Rust rather than by jq filters.
- The repo preview no longer repeats the absolute path as the first line of its file
  tree; the card's own `path` row already carries it.

## [0.6.0] - 2026-07-16

### Added

- An **update action**: `ghq.update-plugin` installs the newest tagged version and
  rebuilds the switcher. It refuses to run against anything but an unambiguous GitHub
  install — a linked development checkout is left alone, with the manual steps printed
  instead, since installing over one would overwrite a working tree.
- An **update check**: once a day, the plugin asks GitHub whether a newer version is
  tagged and shows `↑ v0.6.0` at the end of the command bar. It never installs anything,
  and it yields to the keys rather than overdrawing them, so it goes unsaid on a narrow
  terminal. Turn it off with `update_check = "false"` for a plugin that makes no
  outbound requests at all.

  The switcher itself never touches the network: the check runs in a detached child
  process and leaves a cache the TUI reads. The picker often lives less than a second,
  and the fetch takes a few — a thread inside it would be killed before it finished.
  Offline, unreachable, or rate-limited, nothing is shown and the switcher opens as
  always.

- A **changelog viewer**: the `ghq.changelog` action opens this file as a popup, in the
  switcher's colours, with the version you are running marked `← installed`. It reads
  the `CHANGELOG.md` that ships beside the plugin, so it needs no network and always
  describes the code you actually have.

### Changed

- The settings dashboard is now part of the switcher's TUI instead of an fzf list, and
  opens as a session-modal popup sized to its content rather than a full-screen overlay.
  It reads as the form it is: no fuzzy prompt, no match counter, and no border label
  doubling herdr's own pane title. `↑`/`↓` walk it, `enter` cycles the value or
  edits `split_ratio` in place, `esc` closes. Needs herdr ≥ 0.7.4, already the declared
  minimum.

### Fixed

- Every setting is visible: the fzf dashboard cut off `notification_position` and
  truncated the `preview_position` hint. A window too short to fit the form now scrolls
  to keep the selection in view instead of silently hiding rows.
- Opening the switcher no longer fails on machines without `fzf`. Nothing in the plugin
  has used fzf since the settings dashboard moved into the TUI — the clone flow prompts
  with `read` — but `bin/action.sh` still refused to start the picker without it.

### Removed

- `fzf` is no longer a dependency.

## [0.5.0] - 2026-07-16

### Changed

- Previews now render on a worker thread instead of between a keypress and the next
  frame, so scrolling the list stays responsive on large repositories where
  `git status` dominates the ~100ms preview cost. The pane shows a `…` placeholder
  while a preview is in flight, and results the list has already scrolled past are
  dropped rather than drawn.

### Fixed

- The `open-workspace`, `open-tab`, and `open-split` actions behaved identically to
  plain `menu`: `bin/action.sh` exported `GHQ_FORCE_TARGET`, but nothing in the TUI
  read it, so Enter always fell back to `default_target`. A forced target now wins
  over `default_target`, and unrecognised values on either degrade to `workspace`
  rather than failing the open. Enter on an agent or workspace still focuses that
  entry — forcing a target only changes where a _repo_ lands.
- Panes that herdr reports with a terminal id but no agent label — stale or
  half-detected entries — no longer appear in the list as an agent named "agent".

## [0.4.0] - 2026-07-16

### Added

- `alt-p` toggles the preview pane at runtime.
- `tab` / `shift-tab` cycle the group filter (All → Agents → Workspaces → Repos),
  skipping empty groups; the active group is shown in the Switcher title.
- `alt-s` cycles the sort: `recent` (latest opened) → `name` → `kind`. Opens are
  remembered in `${XDG_STATE_HOME:-~/.local/state}/herdr-ghq/recent.tsv`.
- A `sort` key in the settings dashboard and the example config sets the startup
  default.

### Changed

- The default sort is now `recent`, so repositories you opened most recently float
  to the top of the resting list. While you are typing, fuzzy match score still
  orders the list.

## [0.3.4] - 2026-07-16

### Added

- A `?` keybindings cheatsheet popup (any key closes it).

### Changed

- List rows are now colourful: a kind icon, a bold primary name, and dim context.

## [0.3.3] - 2026-07-16

### Added

- A `title_color` config key (a `[theme.custom]` slot or a `#hex` value) colours the
  Search / Switcher / Preview box titles, defaulting to peach so they stand apart
  from the accent.

### Changed

- The documented keybinding for the switcher is now `prefix+space`.

## [0.3.2] - 2026-07-16

### Changed

- The command bar renders each key as a coloured background pill with dark ink, using
  full labels (open/tab/split/cd/workspace/git/update/remove/clone).
- The switcher:preview split now defaults to 4:6 (`preview_size = 60`).

## [0.3.1] - 2026-07-16

### Added

- `preview_position` (`right` | `down` | `up` | `left`) and `preview_size`.

### Changed

- The preview now defaults to the right (side-by-side). The command bar spans the
  full width regardless of preview position — something fzf could not do.

## [0.3.0] - 2026-07-16

### Changed

- The switcher is now a purpose-built Rust TUI (ratatui + nucleo) rather than an fzf
  wrapper, giving it full layout control: a Search box on top, the Switcher list, a
  Preview pane, and a full-width colourful command bar pinned to the bottom. The
  clone and settings flows stay on bash + fzf.
- `bin/picker.sh` is now a thin wrapper that builds the binary on first run and
  `exec`s it.

### Added

- **Requires [Rust / `cargo`](https://rustup.rs)** (`brew install rust`) to build the
  switcher on first launch.

## [0.2.3] - 2026-07-16

### Added

- `preview_position` (`down` | `right` | `up` | `left`) and `preview_size`.

### Changed

- The preview now defaults to the bottom, which is what makes an edge-to-edge command
  bar possible under fzf. Set `preview_position = "right"` to restore side-by-side.

## [0.2.2] - 2026-07-16

### Changed

- The command bar is compact (short labels, `·` separators) so every key including
  clone fits the list column without truncation, and the match counter sits at the
  right edge of the Search box.

## [0.2.1] - 2026-07-16

### Changed

- Adopted a component-box layout: a Search input box on top, a Switcher list below,
  and a Preview box on the right, dropping the outer wrapper border.
- Command hints moved into a full-width footer bar, each key in its own theme hue.
- The herdr overlay title is minimised to an icon.

## [0.2.0] - 2026-07-16

### Added

- **One list, three sources.** The picker is now a unified switcher blending running
  agents, open workspaces, and ghq repositories, with a kind-aware accept: `enter`
  jumps to an agent, switches to a workspace, or opens a repo in the default target.
- The open keys (`ctrl-w` / `ctrl-t` / `ctrl-s` / `ctrl-o`) act on a repo path or on
  an agent's cwd.
- A kind-aware preview: repos show a file tree, agents show recent output, workspaces
  show their tabs and panes.
- `include_agents` and `include_workspaces` config keys.

### Changed

- Rows carry a kind icon, a bold primary name, and dim context; repos drop the
  repeated `host/owner/` prefix and tag the host dimly.

### Notes

- Agents and workspaces require [`jq`](https://jqlang.github.io/jq/). Without it, the
  switcher degrades to repositories only.

## [0.1.0] - 2026-07-16

### Added

- Initial release: a one-key ghq repository switcher for herdr. Fuzzy-pick a repo and
  open it in a new workspace, tab, split, or the current pane, plus clone (`ghq get`),
  update, remove, and a handoff to the git-hub menu.

[Unreleased]: https://github.com/crafts69guy/herdr-switchboard/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.11.0...v1.0.0
[0.11.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.3.4...v0.4.0
[0.3.4]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.2.3...v0.3.0
[0.2.3]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/crafts69guy/herdr-switchboard/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/crafts69guy/herdr-switchboard/releases/tag/v0.1.0
