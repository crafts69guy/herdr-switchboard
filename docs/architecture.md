# Architecture and performance

## Launch flow

Public plugin actions enter through `bin/action.sh`. It captures the origin pane ID and cwd before
opening the requested Herdr surface:

- Compact popups: Menu, AI Agents, Usage, standalone Settings, and Changelog.
- Full overlays: Projects, Commands, Ports, Zen, Git, Clone, and Update.
- No pane: `switchboard.zen-toggle` acts directly on the origin pane.

The in-Projects settings form and cheatsheets are floating TUI overlays, distinct from Herdr popup
panes.

## Runtime

`bin/picker.sh` selects the versioned release binary for macOS or Linux and verifies its SHA-256.
Offline installs and linked checkouts fall back to a local Cargo build. A small typing-cat frame
covers one-time preparation.

The Rust TUI lives in `src/` and shares filtering, navigation, command bars, and preview rendering
across picker modes. Projects loads Herdr agents and workspaces, ghq repositories, and Git worktree
metadata before claiming the terminal, so its first frame contains a usable list. An empty Projects
result hands off to Clone.

Repository and worktree cards include Git state, recent commit information, a file tree, and an
optional rendered README excerpt. Agents and workspaces use live Herdr metadata. Selection actions
target the captured origin pane or an ID returned by Herdr; they do not guess identifiers.

Usage draws itself rather than reusing the picker, because its signal is colour: a quota donut has
to change colour at a threshold, and a picker preview is plain text. Its offline provider is read
before the terminal is claimed and its networked provider on a worker thread, so the first frame is
never waiting on a socket.

Git runs the same binary in Git mode and then replaces the pane process with `bin/review.sh` and
the selected review tool. Changelog and standalone Settings use lightweight Rust popup modes. Clone
remains an interactive Bash flow in `bin/get.sh`.

## Tracing

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
