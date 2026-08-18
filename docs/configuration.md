# Configuration

Switchboard reads `config.toml` from its plugin-specific config directory:

```sh
herdr plugin config-dir switchboard
```

The file is typed, namespaced TOML. Unknown or legacy top-level keys are rejected. Start from
[`examples/config.toml`](../examples/config.toml), invoke `switchboard.settings`, or open the
in-Projects form with `alt-,`.

## Settings form

The form has Common, Projects, Commands, and Ports tabs. `tab` and `shift-tab` switch tabs;
arrow keys move; `enter` changes a value. Edits remain drafts until `a` applies all of them.
`esc` or `q` discards the draft.

When the form is opened inside Projects, applying refreshes that picker: sources, sorting,
previews, colours, notifications, and bindings update without reopening it or reloading Herdr.
The standalone settings action persists the same namespaced values for subsequent picker launches.

## Sections

### `[common]`

| Key | Purpose |
| --- | --- |
| `keymode` | Start in `insert` or `normal`. |
| `title_color` | Theme slot used for picker captions. |
| `transparency` | Picker background transparency behaviour. |
| `update_check` | Check daily for a newer tagged release. |
| `notifications` | Enable Herdr notifications. |
| `notification_position` | Choose the notification corner. |
| `notification_sound` | Use `auto`, `none`, `done`, or `request`. |

The daily update check displays the available version in the command bar; it never installs an
update. `switchboard.update` is the explicit installation action.

### `[projects]`

| Key | Purpose |
| --- | --- |
| `default_target` | `workspace`, `tab`, `split`, or `pane`. |
| `default_tab` | `all`, `agents`, `workspaces`, `repos`, or `worktrees`. |
| `include_agents`, `include_workspaces`, `include_worktrees` | Control list sources. |
| `sort` | `recent`, `name`, or `kind` for an empty query. |
| `label` | `repo`, `owner-repo`, or `path` for created targets. |
| `split_direction`, `split_ratio` | Split geometry. |
| `preview`, `preview_position`, `preview_size` | Preview layout. |
| `preview_readme` | Include rendered README excerpts. |

### `[commands]`, `[ports]`, `[clone]`, and `[git]`

- `commands.history_limit`, `commands.history_exclude`, and `commands.sort` control imported shell
  history.
- `[[commands.presets]]` adds a `label`, exact `command`, and `cwd` (`origin` or an absolute path).
- `ports.refresh_interval_ms` controls listener refresh and must be at least 250.
- `clone.source` chooses `clipboard` or an empty prompt; `clone.open_after` controls handoff.
- `git.base_branch` pins branch review; an empty value enables automatic detection.

### `[zen]`

- `width` is the focused pane's percentage and accepts 20 through 95.
- `scrim` enables gutter painting.
- `scrim_color` is an `#rrggbb` colour.
- `chrome` accepts `off`, `panes`, or `full`.

See [Zen mode](zen.md) before enabling `zen.chrome`; non-default values temporarily rewrite global
Herdr UI settings.

## Keymaps

Use `[keys.<picker>]` tables with action-to-chord entries:

```toml
[keys.projects]
split = "ctrl-x"
down = "ctrl-j,ctrl-n"

[keys.agents]
workspace = "alt-a"

[keys.commands]
copy = "ctrl-g"
```

See [Keybindings](keybindings.md) for defaults and chord syntax.

## State and network access

Recent Projects selections live at:

```text
${XDG_STATE_HOME:-~/.local/state}/herdr-switchboard/recent.tsv
```

The Usage popup is the one surface that makes a request while you watch. Codex is read from disk;
Claude Code's card calls the usage endpoint behind the in-session `/usage` command, reading the
OAuth token Claude Code stores in the macOS keychain or `~/.claude/.credentials.json`. Set
`usage.timeout_ms` to bound it, or drop `"claude"` from `usage.providers` to switch it off
entirely. The token is never cached, logged, traced, or passed on a command line.

Managed installs fetch a version-matched release binary and verify its SHA-256. Linked development
checkouts build local source instead. The update action refuses to replace a linked checkout.
