# Keybindings

Switchboard pickers share navigation, filtering, mouse support, and a live command bar. Projects
also exposes `?` for the bindings active after configuration overrides; the other mode pickers
show their resolved action caps in the command bar.

## Picker modes

Pickers start in Normal mode by default. Press `i` or `/` to filter, then `esc` to return to Normal.
Set `common.keymode = "insert"` to restore a type-first start.

### Projects: Insert mode

| Key | Action |
| --- | --- |
| `enter` / `alt-enter` | Open the selection / enter the Clone flow. |
| `ctrl-j`, `ctrl-n` / `ctrl-k`, `ctrl-p` | Move down / up. |
| `ctrl-t` / `ctrl-v` / `ctrl-o` | Open in a tab / split / current pane. |
| `alt-w` | Open in a workspace. |
| `ctrl-r` / `ctrl-x` | Update / remove the selected repo. |
| `tab` / `shift-tab` | Move through All, Agents, Workspaces, Repos, and Worktrees. |
| `alt-p` / `alt-s` | Toggle preview / cycle sort order. |
| `alt-j`, `alt-k` | Scroll the preview. |
| `ctrl-u`, `ctrl-w`, `backspace` | Clear query / delete word / delete character. |
| `alt-,` / `alt-c` / `alt-u` | Settings / changelog / update Switchboard. |
| `?` / `ctrl-c` | Help / close. |

Update and removal apply only to repository rows, not linked worktrees. Removal always requires the
repository name as typed confirmation.

### Projects: Normal mode

| Key | Action |
| --- | --- |
| `j`, `k` / `g`, `G` | Move / jump to the top or bottom. |
| `ctrl-d`, `ctrl-u` | Page down / up. |
| `H`, `L` | Previous / next group. |
| `i`, `/` | Enter Insert mode. |
| `enter` | Use the selection's default action. |
| `t` / `v` / `o` / `w` | Open in a tab / split / current pane / workspace. |
| `p` / `alt-j`, `alt-k` | Toggle / scroll the preview. |
| `space u` / `space x` / `space c` | Update repo / remove repo / Clone flow. |
| `space s` / `space l` / `space ,` | Sort / changelog / settings. |
| `space U` | Update Switchboard. |
| `?` / `q`, `esc` | Help / close. |

## Kind-aware Enter

| Selected row | `enter` does |
| --- | --- |
| Agent | Focus the live agent. |
| Workspace | Focus the live workspace. |
| Repo | Open it in `projects.default_target`. |
| Worktree | Open its linked checkout in `projects.default_target`. |

The resting Projects list uses `projects.sort`; a non-empty query switches to fuzzy-score order.
Successful opens update `${XDG_STATE_HOME:-~/.local/state}/herdr-switchboard/recent.tsv`.

## Other pickers

All pickers retain the shared search and navigation controls. Their command-specific defaults are:

| Picker | Keys |
| --- | --- |
| AI Agents | `enter` current pane, `ctrl-t` new tab, `alt-w` new workspace. |
| Usage | `r` re-read every provider, `esc` close. |
| Commands | `enter` fill, `ctrl-enter` run, `alt-enter` run from historical cwd, `ctrl-y` copy, `ctrl-x` forget, `alt-s` sort. |
| Ports | `enter` copy address, `ctrl-enter` HTTP, `alt-enter` HTTPS, `ctrl-w` workspace, `ctrl-x` TERM, `alt-x` KILL. |
| Zen | `enter` focus the selected pane, `ctrl-x` leave the active Zen session. |

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

[keys.commands]
copy = "ctrl-g"
```

A chord is a key with optional `ctrl-`, `alt-`, or `shift-` prefixes. Projects accepts multiple
comma-separated chords for one action; shared mode pickers use the first configured chord for their
single action slot. Footers and the Projects `?` popup render from the canonical parsed chord, so
display and input stay synchronized after remapping.

See [`examples/config.toml`](../examples/config.toml) for configuration structure.
