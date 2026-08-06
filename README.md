# herdr-switchboard

![herdr 0.8.0+](https://img.shields.io/badge/herdr-0.8.0%2B-lightgrey)
![ghq required](https://img.shields.io/badge/ghq-required-green)
![platform macOS | Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue)
![license MIT](https://img.shields.io/badge/license-MIT-green)

A colorful [herdr](https://herdr.dev) command palette with searchable pickers:
**Projects** for running agents/workspaces/ghq repos/worktrees, **AI Agents** for starting installed
Herdr integrations, **Commands** for exact shell history and presets, and **Ports** for live local
TCP listeners. Open the central menu or bind each picker directly.

Where `ghq list | fzf | cd` can only change a directory, this uses herdr as a multiplexer:
jump to a live agent, switch workspaces, or open a repo/worktree exactly where you want it —
a new workspace, tab, split, or the current pane. It is a Rust TUI (ratatui + nucleo); no
fzf required.

> [!WARNING]
> **This plugin is early, in active development, and experimental.** It is pre-1.0
> (`0.x`): behaviour, keybindings, configuration keys, and the on-disk state format can
> change between releases, sometimes without a migration path. It also tracks herdr's own
> fast-moving CLI/socket API and may break as herdr evolves. Expect rough edges,
> pin a version if you need stability, and please
> [report issues](https://github.com/crafts69guy/herdr-switchboard/issues). The one destructive
> destructive actions always require typed confirmation: repository removal asks for
> the repo name, while Port TERM/KILL asks for the signal word and revalidates process identity.

![The herdr-switchboard switcher: a fuzzy list with live preview and the `?` keybindings popup open](docs/switcher.png)

## Requirements

|                                                                          |                                                     |
| ------------------------------------------------------------------------ | --------------------------------------------------- |
| **herdr** ≥ 0.8.0                                                        | the host multiplexer and AI integration launcher    |
| **[`ghq`](https://github.com/x-motemen/ghq)**                            | repository source                                   |
| _fallback_ **[Rust / `cargo`](https://rustup.rs)**                       | only needed if a release binary cannot be downloaded |
| **[`tuicr`](https://github.com/agavra/tuicr)** ≥ 0.20.0                  | the git menu's review tool (`brew install tuicr`)   |
| _optional_ **[`gh`](https://cli.github.com)**                            | the git menu's pull-request row (hidden without it) |
| _optional_ **[`lazygit`](https://github.com/jesseduffield/lazygit)**     | staging/commit from the git menu                    |
| _optional_ **[`eza`](https://github.com/eza-community/eza)**             | richer preview tree                                 |

## Install

```sh
herdr plugin install crafts69guy/herdr-switchboard
```

Bind a key in `~/.config/herdr/config.toml` (see [`examples/keybindings.toml`](examples/keybindings.toml)):

```toml
[[keys.command]]
key = "prefix+space"
type = "plugin_action"
command = "switchboard.menu"
description = "Switchboard menu"
```

Reload, then press `prefix+space`:

```sh
herdr server reload-config
```

## Keybindings

The picker works like a Telescope/LazyVim picker: it opens **typing** (Insert mode), and
`esc` drops to **Normal** mode for Vim motions. A `NORMAL` / `INSERT` tag on the search box
says which mode owns the keys, `?` shows the live cheatsheet for that mode, and the
command bar re-labels itself per mode. Press `i` or `/` in Normal to type again.

**Accept** (`enter`) is kind-aware in either mode:

| Highlighted   | `enter`                                                                   |
| ------------- | ------------------------------------------------------------------------- |
| **agent**     | jump to it (`herdr agent focus`)                                          |
| **workspace** | switch to it (`herdr workspace focus`)                                    |
| **repo**      | open it in `default_target` — a new workspace unless configured otherwise |
| **worktree**  | open its linked checkout in `default_target`                              |

**Insert mode** (type to filter; `esc` → Normal, `^c` closes):

| Key                | Does                                                                     |
| ------------------ | ------------------------------------------------------------------------ |
| `↵` · `⌥↵`         | open · switch to the **clone** flow                                      |
| `^j`/`^n` · `^k`/`^p` | down · up                                                             |
| `^t` · `^v` · `^o` | open in a new **tab** · **split** · the **current pane** (`cd`)          |
| `⌥w` · `^r` · `^x` | to a **workspace** · `ghq get -u` · **remove**                          |
| `tab` / `⇧tab`     | cycle groups (All → Agents → Workspaces → Repos → Worktrees)              |
| `⌥p` · `⌥s` · `⌥j`/`⌥k` | toggle preview · cycle sort · scroll the preview                    |
| `^u` · `^w` · `⌫`  | clear the query · delete a word · delete a char (readline)               |
| `⌥,` · `⌥c` · `⌥u` · `?` | settings · changelog · update the plugin itself · this cheatsheet  |

**Normal mode** (`esc` from Insert; `i` or `/` returns): bare `h`/`j`/`k`/`l` motion is Vim's —
`j`/`k` move, `g`/`G` top/bottom, `^d`/`^u` page, `H`/`L` prev/next group. Frequent opens sit
on unshifted keys — `t` tab, `v` split, `o` cd, `w` workspace, `p` toggle preview — and the
**`␣` leader** groups the rest: `␣u` update repo, `␣x` remove, `␣c` clone, `␣s` sort,
`␣l` changelog, `␣,` settings. `q` or `esc` closes. (`?` always shows the exact, current bindings.)

**Anywhere:** the **wheel** scrolls the pane under the pointer (card over the preview, list
elsewhere); a **click** selects an entry, filters on a group tab, or runs a command-bar pill.

Sorting defaults to `recent`, so repos you opened last float to the top; opens are recorded
in `${XDG_STATE_HOME:-~/.local/state}/herdr-switchboard/recent.tsv`. While you type, fuzzy score
orders the list — sort only applies to the resting, no-query list.

**Remapping.** Every binding is a `chord → action` entry you can change in `config.toml`:

```toml
[keys.projects]
tab = "ctrl-y"              # cycle groups on ^y instead of Tab
split = "ctrl-x"            # split on ^x instead of ^v
down = "ctrl-j,ctrl-n"      # one action, several chords
```

A chord is a key with optional `ctrl-` / `alt-` / `shift-` prefixes. The full list of action
names is in [`examples/config.toml`](examples/config.toml), and the footer + `?` cheatsheet
re-render from your bindings, so they always show what you actually set.

**Start mode.** `keymode = "normal"` opens the picker in Normal mode (Vim-first) instead of
Insert. Normal mode is always one `esc` away either way.

## Actions

Bind any of these as a Herdr `plugin_action`:

| Action                                                   | Does                                                           |
| -------------------------------------------------------- | -------------------------------------------------------------- |
| `switchboard.menu`                                               | searchable central menu                                        |
| `switchboard.projects`                                           | Projects: running agents, workspaces, repos and worktrees       |
| `switchboard.agents`                                             | start an installed AI integration in a pane, tab, or workspace  |
| `switchboard.commands`                                           | Commands: shell history and configured presets                  |
| `switchboard.ports`                                              | Ports: live listeners, owner process and safe actions           |
| `switchboard.settings`                                           | package settings (`Common / Projects / Commands / Ports`)       |
| `switchboard.git`                                                | the git menu for the current repo, in its own pane (bind to `prefix+g`) |
| `switchboard.zen-toggle`                                         | put **this** pane in zen, or bring it back (opens no picker; bind to `prefix+z`) |
| `switchboard.zen`                                                | Zen: pick which pane to zen                                     |
| `switchboard.clone`                                              | the clone flow                                                 |
| `switchboard.changelog`                                          | what changed, with your installed version marked               |
| `switchboard.update`                                             | install a newer version (refuses to touch a `link`ed checkout) |
| `switchboard.open-workspace` · `switchboard.open-tab` · `switchboard.open-split` | the switcher with `enter`'s repo target forced                 |

### AI Agents

AI Agents is available as `alt-a` in the central menu, or bind it directly:

```toml
[[keys.command]]
key = "prefix+a"
type = "plugin_action"
command = "switchboard.agents"
description = "Start an AI agent"
```

It reads `herdr integration status` and lists only installed integrations. `Enter` starts
the selected AI in the pane that opened Switchboard, `ctrl-t` creates and focuses a new tab, and
`alt-w` creates and focuses a new workspace. New targets inherit the origin pane's cwd; if startup
fails, Switchboard closes the tab/workspace it created instead of leaving an empty target behind.
The three actions can be remapped under `[keys.agents]` as `pane`, `tab`, and `workspace`.

### Commands

Commands merges zsh, Bash, or fish history with `[commands].presets`, deduplicating by the exact
command text. It excludes common credential patterns before persistence; add Rust regexes through
`history_exclude`. Forgotten commands are fingerprinted in a denylist so the next shell import does
not bring them back. `alt-s` cycles frecency/recent/frequency/alphabetical ordering. Enter fills the origin pane, `ctrl-enter` runs there, `alt-enter` runs from the
most recent historical cwd, `ctrl-y` copies, and `ctrl-x` forgets. Multiline execution requires a
typed confirmation and the full command is never included in notifications.

### Ports

Ports refreshes native TCP listener data off the input loop and groups IPv4/IPv6 addresses for the
same PID and port. Search fields include `port:`, `address:`, `pid:`, `process:`/`proc:`, `cwd:`,
`repo:` and `user:`; quoted values and negated filters are supported. Enter copies
`localhost:PORT`, `ctrl-enter`/`alt-enter` open HTTP/HTTPS, and `ctrl-w` opens the process cwd as a
workspace. TERM (`ctrl-x`) and KILL (`alt-x`) each require confirmation, ownership, and a fresh
PID + process-start identity check; Switchboard never signals a parent or process group.

### Zen

Zen gives one pane the screen: it moves into a tab of its own, centred at `zen_width` (70% by
default) between two dimmed gutters, and toggling again puts it back beside the pane it came from.
`switchboard.zen-toggle` acts on the pane you press it in and never opens a picker, which is what
makes it worth a dedicated key; `switchboard.zen` (`⌥z` in the central menu) opens a picker so you
can zen some *other* pane, and `ctrl-x` there leaves zen.

Only the zen'd pane ever moves — its neighbours keep running, untouched, in the original tab, and
the pane's own process survives the move. Herdr has no pane opacity or dim-inactive setting, so the
gutters are dimmed by painting over them through Herdr's graphics API. That needs
`[experimental] kitty_graphics = true` in Herdr's own config; without it the gutters are blank
rather than dark and nothing errors. Set `zen_scrim = false` if you would rather they stayed plain.

Leaving zen puts every pane back. In a two-pane tab — and wherever the zen'd pane's neighbour was
a single pane rather than a nested group — the original layout is reproduced exactly. In a tab
whose splits nest more deeply, Herdr 0.8.0 offers no way to re-insert a pane at an arbitrary point
in the split tree, so the pane returns beside its nearest former neighbour and the nesting can
differ; Switchboard says so with a notification rather than rearranging your tab quietly. Nothing
is ever lost or restarted.

Zen is deliberately not built on `herdr pane zoom`: splitting a zoomed tab silently cancels the
zoom, so gutters and zoom cannot coexist. If you want plain fullscreen with no centring, Herdr's
own `prefix+z` already does it.

## Git menu

The git menu is its **own herdr pane**, opened by the `switchboard.git` action — bind it to `prefix+g`.
It acts on the repo the pane you launched from is sitting in, and it loads nothing else: no
agents, no workspaces, no repository index, no preview. That is the whole point of it being a
separate pane rather than an overlay on the switcher.

> **There is no `⌥g` in the switcher any more.** Reviewing an arbitrary repo you fuzzy-jumped to
> is gone with it; `cd` there (`^o`) and press `prefix+g`.

Walk it with `↑`/`↓`, `enter` runs the row, a mnemonic letter runs it directly, `esc` closes.
Whatever you pick **takes over this pane** — quit the tool and you are back where you started.

| Key | Row                       | Runs                                                                 |
| --- | ------------------------- | -------------------------------------------------------------------- |
| `d` | review **worktree**       | `tuicr -w` — staged and unstaged together                            |
| `b` | review **branch**         | `tuicr -r <base>.. -w` — base auto-detected, or pinned via `base_branch` |
| `h` | review **commits**        | `tuicr` — tuicr's own commit selector                                |
| `a` | review **all files**      | `tuicr -A`                                                           |
| `p` | review **pull request**   | `gh pr list` → `tuicr pr <n>` (hidden when `gh` is not installed)    |
| `r` | **saved reviews**         | `tuicr review list` → `tuicr review comments --session <slug>`       |
| `l` | **lazygit**               | stage / commit / push (shown only when `lazygit` is installed)       |

`p` and `r` open a sub-list: **type to fuzzy-filter**, `↑`/`↓` (or `^n`/`^p`) move, `enter` picks,
and `esc` clears the filter before backing out to the menu.

**`r` reads comments, it does not reopen a review.** `--session` exists only on tuicr's headless
`review add` / `review comments`; the TUI does not take it.

Reviews open in [`tuicr`](https://github.com/agavra/tuicr) (`brew install tuicr`, ≥ 0.20.0), which
is a **hard requirement** — the menu is a review menu. Its colours come from tuicr's own
`theme` setting, which [hue-theme](https://github.com/crafts69guy/hue-theme) generates; this
plugin writes nothing into tuicr's config, because a theme it half-agreed with would make tuicr
exit 2 and take every review down with it.

Add your own rows in `menu.conf` (`key|icon|label|shell command`) beside `config.toml`.

## Configuration

Settings live in namespaced TOML in the plugin config dir (`herdr plugin config-dir switchboard`).
Edit it directly, copy [`examples/config.toml`](examples/config.toml), use
`switchboard.settings`, or press `⌥,` in Projects. Tab/shift-tab switches
`Common / Projects / Commands / Ports`; `↑`/`↓` moves and `enter` changes a value. Edits are
drafts: a `●` marks each changed row, `a` applies them all to `config.toml`, and `esc` discards
them. Applying takes effect in the running switcher — the list re-sorts, sources and preview
reload, colours and key rebinds update on the spot; no relaunch or server reload needed.

Every key is documented in `examples/config.toml`. The ones you're most likely to want:

| Key                                     | Values                                                                      |
| --------------------------------------- | --------------------------------------------------------------------------- |
| `default_target`                        | `workspace` (default) · `tab` · `split` · `pane`                            |
| `default_tab`                           | `all` (default) · `agents` · `workspaces` · `repos` · `worktrees`            |
| `include_agents` / `include_workspaces` | blend agents/workspaces into the list                                       |
| `include_worktrees`                     | list linked Git worktrees (`true` by default)                               |
| `sort`                                  | `recent` (default) · `name` · `kind`                                        |
| `keymode`                               | start mode: `insert` (default) · `normal` (Vim-first)                       |
| `[keys.projects]` / `[keys.commands]` / `[keys.ports]` | picker-specific keymaps                                  |
| `label`                                 | workspace/tab label: `repo` · `owner-repo` · `path`                         |
| `preview` / `preview_readme`            | the preview pane                                                            |
| `clone_source`                          | seed the clone prompt from the `clipboard` (default) or start blank         |
| `base_branch`                           | base for the git menu's branch review (blank = auto-detect)                 |
| `split_direction` / `split_ratio`       | geometry for split targets                                                  |
| `zen_width` / `zen_scrim` / `zen_scrim_color` | zen's centred width (20–95), and whether/what colour the gutters are dimmed |
| `update_check`                          | ask GitHub once a day whether a newer version is tagged (`true` by default) |
| `notifications` / `notification_position` | herdr toasts, and which corner they land in                               |
| `notification_sound`                    | `auto` (per-event, default) · `none` · `done` · `request`                   |

The switcher is themed from herdr's `[theme.custom]`, and previews each kind as a card —
a header with the entry's state as a pill, aligned `label value` rows, then bodies under
captioned rules. Repos and worktrees show branch · clean/dirty · last commit, a file tree,
and a README excerpt rendered as markdown; agents show what they are doing and their recent
output, in the agent's own colours; workspaces list their tabs, each with its live status.
Long cards scroll with `alt-j` / `alt-k`.

Switchboard only reads its namespaced configuration from the `switchboard` plugin config directory.
Bind `switchboard.projects` for the direct Projects picker or `switchboard.menu` for the Central
Menu.

`update_check` only ever shows `↑ v0.6.0` in the command bar — it never installs anything.
Set it to `false` to disable that daily check. A managed install can still contact GitHub once
to fetch its version-matched, checksummed switcher binary; linked development checkouts build
their local source instead.

## How it works

Each action starts in `bin/action.sh`, which captures the origin pane id and cwd before the
new pane steals focus, then opens that pane — an overlay for the picker, the git menu, and the
clone flow, a popup for the changelog.

The picker itself is the Rust TUI in `src/`. On a managed install, `bin/picker.sh` selects a
versioned macOS/Linux release binary for the host architecture and verifies its SHA-256;
offline or linked checkouts fall back to Cargo. A small typing-cat bootstrap animates during
that one-time preparation.

The Projects TUI reads `herdr agent list`, `herdr workspace list`, `ghq list`, and Git's stable
`worktree list --porcelain -z` output **synchronously, before it claims the terminal** — the
whole set costs around 35 ms, so the first thing painted is the loaded list rather than a
placeholder. (An empty result never claims the terminal at all: it hands the pane straight to
the clone flow.) It fuzzy-filters with nucleo and previews the selection as a card drawn in
your herdr theme colours — `bin/preview.sh` supplies only the repo/worktree file tree. On
accept it maps the key to a herdr CLI verb — `agent focus`, `workspace focus`,
`workspace create`, `tab create`, `pane split`, `pane send-text` — always targeting the
captured origin pane or a real id from herdr, never a guessed one.

The git menu is the same binary in `--git` mode, in a pane of its own, and picking a row
`exec`s `bin/review.sh` over it. One static ASCII-cat frame is drawn first and left on screen
for tuicr to paint over, because tuicr can take a second or more to read a large diff before
its own first frame. The changelog viewer is `--changelog` mode; the settings form is a
floating overlay inside the switcher itself (`⌥,`), not a separate pane. Only the clone flow
is still bash (`bin/get.sh`).

### Measuring it

Set `SWITCHBOARD_TRACE=1` and every launch appends tab-separated timings to
`$XDG_STATE_HOME/herdr-switchboard/trace.log` (or `$SWITCHBOARD_TRACE_FILE`). Nothing is ever written to
stdout or stderr — the TUI owns the terminal for its whole life.

```sh
SWITCHBOARD_TRACE=1 herdr plugin action invoke projects --plugin switchboard
awk -F'\t' '$2 == "frame.first_list" { print $1 }' ~/.local/state/herdr-switchboard/trace.log
awk -F'\t' '$2 == "preview.render" { n++; t += $3 } END { print t / n }' \
  ~/.local/state/herdr-switchboard/trace.log
```

The budgets the current code is held to: first list < 100 ms, keystroke-to-frame < 16 ms,
preview render < 50 ms mean and < 70 ms at the tail.

## Contributing

Issues and pull requests are welcome. Start here:

```sh
git clone https://github.com/crafts69guy/herdr-switchboard
cd herdr-switchboard
herdr plugin link "$PWD"        # install this checkout
herdr server reload-config
```

Before you open a PR:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
bash tests/manifest_spec.sh      # manifest contract, version sync, bash syntax
bash tests/update_guard_spec.sh  # the update guard (herdr is stubbed, never called for real)
bash tests/bootstrap_spec.sh     # target mapping, release checksum, atomic install
```

CI runs those checks on every change. Tagged releases additionally build four native binaries
(macOS/Linux, arm64/x86_64) and publish them only after every target succeeds. Two things are
easy to miss:

- **Any user-visible change adds a line to `CHANGELOG.md`'s `[Unreleased]` section in the
  same commit** — `bin/release.sh` promotes that section and the release workflow publishes it
  verbatim as the GitHub release notes; nothing is generated from `git log`.
- **Don't bump versions by hand.** `Cargo.toml` and `herdr-plugin.toml` must match, and
  `bin/release.sh` bumps both.

Layout, keybinding, and herdr CLI changes need manual exercise in a real herdr session —
there is no test runner for the overlay. Please attach a screenshot when visual output
changes, and test `ctrl-x` against disposable repos.

[`AGENTS.md`](AGENTS.md) has the full conventions: module layout, coding style, testing, and
the safety rules around herdr ids and destructive flows.

## Changelog

Run `switchboard.changelog` to read it in a popup with your installed version marked, or see
[`CHANGELOG.md`](CHANGELOG.md). Releases are tagged `vX.Y.Z`; to update, re-run the install
command (it re-fetches the ref) or use the `switchboard.update` action. Watch the repository
(Watch → Custom → Releases) to hear about new versions.

## License

MIT — see [`LICENSE`](LICENSE).
