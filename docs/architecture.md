# Architecture and performance

Switchboard uses hosted state machines with explicit external effects. The goal is not a framework;
it is a small set of deep modules whose seams keep terminal lifecycle, domain decisions, and
external work from leaking into callers.

```text
argv / environment
       │
       ▼
 typed Config + mode composition root
       │
       ├── ProjectCatalog ── CommandRunner ── herdr / ghq / git
       │
       ▼
 Surface model ── event → typed Transition
       │                    ├── Redraw → render(Frame)
       │                    ├── Step → background adapter → tick result
       │                    └── Exit(output) → restore → interactive effect
       ▼
 surface::run ── crossterm / ratatui
```

## Launch flow

Public plugin actions enter through `bin/action.sh`. It captures the origin pane ID and cwd before
opening the requested Herdr surface:

- Compact popups: Menu, AI Agents, Usage, standalone Settings, and Changelog.
- Full overlays: Projects, Commands, Ports, Zen, Git, Clone, and Update.
- No pane: `switchboard.zen-toggle` acts directly on the origin pane.

The in-Projects settings form and cheatsheets are floating TUI overlays, distinct from Herdr popup
panes.

`bin/picker.sh` selects the versioned release binary for macOS or Linux and verifies its SHA-256.
Offline installs and linked checkouts fall back to a local Cargo build. A small typing-cat frame
covers one-time preparation.

The Projects Picker loads Herdr agents and workspaces, ghq repositories, and Git worktree metadata
before claiming the terminal, so its first frame contains a usable Navigator. An empty result hands
off to Clone. Repository and worktree Inspectors include Git state, recent commits, a file tree, and
an optional README excerpt. Selection actions use the captured origin or an ID returned by Herdr;
they never guess identifiers.

Usage keeps its specialized quota visualization. Its offline provider is read before terminal
claim and its networked provider runs on a worker thread, so the first frame never waits on a
socket. It is the only credential-reading surface and the only in-process HTTP client. Git replaces
its pane process with `bin/review.sh` after selection; on-demand pull-request/list and file-count
effects run in the background while its surface remains responsive. The update check uses a
detached child, and Clone remains an explicitly invoked Bash flow in `bin/get.sh`.

## Module seams

| Module | Interface | Implementation hides |
| --- | --- | --- |
| `surface` | `Surface`, `Transition`, `run` | Terminal lease, mouse capture, polling, ticks, redraw, teardown |
| `config` | Typed section fields, `parse`, `try_load`, finite `value_for_cli` | Namespaced deserialization, defaults, validation |
| `source::ProjectCatalog` | `new`, `load`, canonical `kinds` | Source enablement and load order |
| `data` | Source loaders, entry and browse types, `Theme` | Response parsing and presentation mapping |
| `keymap` | `Chord`, `Action`, `Keymap`, canonical chord conversion | Mode tables, overrides, labels |
| `main` / `picker` / `git` | `Surface` adapters and typed outputs | Surface-specific state reduction and composition |
| `action` | `Accept`, `dispatch`, `open_target` | Restored-terminal effects and process replacement |
| `runner` | `CommandRunner` | `SystemRunner` and `MockRunner` process adapters |

## Interaction model

Normal mode is the default. `i` or `/` enters Insert mode, and `esc` returns to Normal. Projects
uses the same state at every width:

- `>= 120` columns: Context, Navigator, Inspector.
- `80..119` columns: Navigator and Inspector.
- `< 80` columns: Navigator only; preview geometry is cleared so hidden content cannot capture
  mouse input.

The command vocabulary is typed. Projects uses `keymap::Action`; each shared picker supplies its
scoped `ActionSpec` values while the host provides lifecycle behavior. A displayed key cap is
derived from the same parsed chord that handles the event.

## Extension rules

- A new terminal mode implements `Surface`; it never calls `ratatui::init`, `event::poll`, or
  `event::read` directly.
- A new project source is added to `ProjectCatalog` and keeps its response parser in `data`.
- A new external call goes through `CommandRunner`. Slow work initiated by a surface runs as a
  typed background effect, as Git list loading does.
- A new configuration value is a typed section field. Add it to `value_for_cli` only when a Bash
  entrypoint genuinely consumes it.
- Interactive or process-replacing work returns a typed surface output and runs after the host has
  restored the terminal.

## Tracing and budgets

Set `SWITCHBOARD_TRACE=1` to append tab-separated timings to
`$XDG_STATE_HOME/herdr-switchboard/trace.log`, or set `SWITCHBOARD_TRACE_FILE` to another path.

```sh
SWITCHBOARD_TRACE=1 herdr plugin action invoke projects --plugin switchboard
awk -F'\t' '$2 == "frame.first_list" { print $1 }' ~/.local/state/herdr-switchboard/trace.log
awk -F'\t' '$2 == "preview.render" { n++; t += $3 } END { print t / n }' \
  ~/.local/state/herdr-switchboard/trace.log
```

The current performance budgets are:

- First Projects list under 100 ms.
- Keystroke-to-frame under 16 ms.
- Preview rendering under 50 ms mean and 70 ms at the tail.

Treat timings as machine- and repository-dependent measurements rather than fixed launch costs.
