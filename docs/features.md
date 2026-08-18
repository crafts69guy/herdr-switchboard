# Features and safety

## AI Agents

The AI Agents picker reads `herdr integration status` and lists installed integrations. It can
start the selected integration in:

- The pane that opened Switchboard with `enter`.
- A focused new tab with `ctrl-t`.
- A focused new workspace with `alt-w`.

New targets inherit the origin pane's cwd. If startup fails, Switchboard closes a tab or workspace
it created instead of leaving an empty target behind. Remap these actions under `[keys.agents]` as
`pane`, `tab`, and `workspace`.

## Commands

Commands combines zsh, Bash, or fish history with `[[commands.presets]]`, deduplicated by exact
command text. It supports filling, running, copying, and forgetting commands without normalising
their contents.

Before persistence, common credential patterns and expressions from `commands.history_exclude` are
removed. Forgotten commands are fingerprinted in a denylist so the next shell import does not add
them again. Multiline execution requires typed confirmation, and notifications never include the
full command.

The available orders are frecency, recent, frequency, and alphabetical.

## Ports

Ports refreshes native TCP listener data away from the input loop and groups IPv4 and IPv6
addresses that belong to the same PID and port. Structured filters include:

- `port:`, `address:`, and `pid:`
- `process:` or `proc:`
- `cwd:`, `repo:`, and `user:`

Quoted values and negated filters are supported. The selected endpoint can be copied, opened as
HTTP or HTTPS, or opened by process cwd in a workspace.

### Signal safety

TERM and KILL are restricted to processes owned by the current user. Before signalling,
Switchboard:

1. Requires the literal confirmation word `term` or `kill`.
2. Refreshes process information.
3. Revalidates PID, listener, and process start identity.
4. Signals only that PID, never its parent or process group.

This prevents an old picker row from targeting an unrelated process after PID reuse.

## Repository removal

Projects removal accepts only repository rows and asks for the repository name before deletion.
Test destructive flows against disposable repositories.
