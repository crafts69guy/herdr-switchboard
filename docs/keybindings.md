# Keybindings

Switchboard pickers share navigation, filtering, mouse support, and a live command bar. Projects
also exposes `?` for the bindings active after configuration overrides; the other mode pickers
show their resolved action caps in the command bar.

## The prefix concept

A prefix says what *kind* of thing a key does. It never says which mode you are in, so the same
chord means the same thing while you type and while you navigate, and on every picker:

| Prefix | Meaning |
| --- | --- |
| `enter` | Run the selected row's primary action. `ctrl-enter` / `alt-enter` are its variants where a picker has them. |
| `ctrl-<key>` | Act on the selected row — open it, update it, remove it, copy it, send it, star it. |
| `alt-<key>` | Change the view or the app — preview, sort, clone, changelog, settings, plugin update. Also the *heavier* form of the `ctrl` verb on the same letter, such as Ports' `ctrl-x` TERM and `alt-x` KILL. |

Two vocabularies are deliberately outside the rule. **Motion** follows the idiom of its mode:
readline `ctrl-j` / `ctrl-n` / `ctrl-k` / `ctrl-p` while typing, Vim `j` `k` `g` `G` `ctrl-d`
`ctrl-u` while navigating. **Query editing** exists only where there is a query: `ctrl-u` clears it
and `alt-backspace` deletes a word, both Insert-only.

Six chords belong to the picker itself and cannot be rebound or claimed by a mode: `ctrl-c` closes,
`ctrl-u` clears the query, `alt-,` opens settings, `alt-j` / `alt-k` scroll the preview, and `tab` /
`shift-tab` move between groups.

There is no `space` leader. Space can only be a leader in Normal mode — while typing it is a
character you meant — so anything living there was forced to change prefix with the mode, which is
exactly the inconsistency this layout removes.

## Projects

Pickers start in Normal mode by default. Press `i` or `/` to filter, then `esc` to return to Normal.
Set `common.keymode = "insert"` to restore a type-first start.

Every chord below works in **both** modes. Normal adds the bare aliases in the last column — always
the same letter as the chord, one keystroke shorter.

| Key | Action | Bare, Normal only |
| --- | --- | --- |
| `enter` | Open the selection with its default action. | |
| `ctrl-t` / `ctrl-v` / `ctrl-o` / `ctrl-w` | Open in a tab / split / current pane / workspace. | `t` `v` `o` `w` |
| `ctrl-r` / `ctrl-x` | Update / remove the selected repo. | |
| `ctrl-y` / `ctrl-a` / `ctrl-s` | Copy the absolute path / send it to an agent / star the row. | |
| `alt-p` / `alt-j`, `alt-k` | Toggle / scroll the preview. | `p` |
| `alt-s` | Cycle the sort order. | |
| `alt-l` / `alt-h` | Clone flow / changelog. | |
| `alt-,` / `alt-u` | Settings / update Switchboard. | |
| `tab` / `shift-tab` | Move through All, Agents, Workspaces, Repos, Worktrees, and Starred. | `L` / `H` |
| `?` / `ctrl-c` | Help / close. | `q`, `esc` |
| `ctrl-u`, `alt-backspace`, `backspace` | Clear query / delete word / delete character. | Insert only |
| `ctrl-j`, `ctrl-n` / `ctrl-k`, `ctrl-p` | Move down / up (Insert). | `j` `k` `g` `G` `ctrl-d` `ctrl-u` |

Update and removal apply only to repository rows, not linked worktrees. Removal always requires the
repository name as typed confirmation.

## Kind-aware Enter

| Selected row | `enter` does |
| --- | --- |
| Agent | Focus the live agent. |
| Workspace | Focus the live workspace. |
| Repo | Open it in `projects.default_target`. |
| Worktree | Open its linked checkout in `projects.default_target`. |

The resting Projects list uses `projects.sort`; a non-empty query switches to fuzzy-score order.
Successful opens update `${XDG_STATE_HOME:-~/.local/state}/herdr-switchboard/recent.tsv`.

Repo and Worktree stars are local and persistent. A peach `★` marks them in every group, and the
final `★ Starred` group keeps the same search, sort, and item actions as the ordinary catalogue.
Agent and Workspace rows are live identities and cannot be starred.

Copy and send are available for Agent, Repo, and Worktree rows with an absolute path. Agent rows
refresh `foreground_cwd` before acting; Repo and Worktree rows use the path shown in the Inspector.
The copied value is fully expanded even when the Inspector renders the home prefix as `~`.
Workspace rows keep both actions disabled because they can contain more than one repository.

`ctrl-a` sends directly to the captured origin agent only when it remains promptable in the same
worktree. Otherwise an agent picker prefers that worktree and falls back to all promptable running
agents. A failed prompt stays open for retry; success closes Projects.

## Other pickers

All pickers retain the shared search and navigation controls, and read the same prefix concept.
Their surface-specific defaults are:

| Picker | Keys |
| --- | --- |
| AI Agents | `enter` current pane, `ctrl-t` new tab, `ctrl-w` new workspace. |
| Node Versions | `enter` use or install, `ctrl-enter` use, `alt-enter` install, `ctrl-d` default, `ctrl-x` uninstall, `alt-r` refresh. |
| Usage | `r` re-read every provider, `esc` close. |
| Commands | `tab` / `shift-tab` History/Starred, `ctrl-s` star, `enter` fill, `ctrl-enter` run, `alt-enter` historical cwd, `ctrl-y` copy, `ctrl-x` forget, `alt-s` sort. |
| Ports | `enter` copy address, `ctrl-enter` HTTP, `alt-enter` HTTPS, `ctrl-w` workspace, `ctrl-x` TERM, `alt-x` KILL. |
| Zen | `enter` focus the selected pane, `ctrl-x` leave the active Zen session. |

Three surfaces sit outside the concept on purpose, because their keys do not name a verb on a
selected row. The **Git menu**'s bare letters are mnemonics for each review it offers, the **Central
Menu**'s `alt-<letter>` names a destination rather than an action, and the **settings form** is a
form with no selection to act on. **Usage** and the **changelog** are viewers with no query, so
their bare keys cannot collide with typing.

## Mouse

The pointer works on every surface: the switcher, the mode pickers, the Git menu and its
sub-lists, the settings form, the changelog, and the Usage pane.

- The wheel scrolls the preview when the pointer is over it; elsewhere it moves the selection.
  Over the changelog and the settings form it walks their own content.
- Clicking a row selects it. **Clicking the row that is already selected runs it** — what Enter
  would do. Terminals report no double-click, so this is how a click both navigates and acts
  without a stray one launching anything.
- Clicking a group tab filters the list; clicking a settings tab switches groups.
- Clicking a command-bar pill does what the key printed on its cap does.
- Clicking outside the settings card closes it and discards the unsaved draft, exactly like `esc`.

## Remapping

Bindings are `action = "chord"` entries under a picker-specific table:

```toml
[keys.projects]
tab = "ctrl-y"
split = "ctrl-x"
down = "ctrl-j,ctrl-n"
star = "alt-f"

[keys.commands]
copy = "ctrl-g"
star = "alt-f"
```

Every picker has a table: `projects`, `agents`, `commands`, `ports`, `zen`, `menu`, and `fnm`. The
action ids are the ones printed on the command-bar pills.

A chord is a key with optional `ctrl-`, `alt-`, or `shift-` prefixes. Projects accepts multiple
comma-separated chords for one action; shared mode pickers use the first configured chord for their
single action slot. Footers and the Projects `?` popup render from the canonical parsed chord, so
display and input stay synchronized after remapping.

A Projects override binds the chord in **both** modes, so a remap cannot reintroduce the split the
concept removes. The previous keys are one block away if you want them back — set them together,
since a chord bound to one action is taken from whatever held it before:

```toml
[keys.projects]
workspace = "alt-w"
clone = "alt-enter"
changelog = "alt-c"
delete_word = "ctrl-w"
```

See [`examples/config.toml`](../examples/config.toml) for configuration structure.
