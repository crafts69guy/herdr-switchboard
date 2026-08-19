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

## Usage

The Usage popup answers one question — how much of each AI subscription is spent, when the window
resets, and when the plan renews. Each agent gets a card:

- A **donut** for the window closest to running out, with its percentage in the middle.
- A **bar per window**, so a plan with a five-hour and a weekly bucket shows both.
- A **facts block**: which account the numbers belong to, what the session spent, how much of the
  context window the last turn used, whether credits are on, whether a limit has actually been
  hit, and **when the subscription renews** — `renews  13 Sep · in 25d`.
- A line saying **how old the reading is** and the wall clock the window rolls over at:
  `as of 12m ago · resets 08:53 Wed 19 Aug`. The calendar date is there only when the rollover is
  not today — a window resetting this evening reads `resets 23:33 Mon` — so seeing a date is
  itself the signal that the wait crosses a day. The bar rows say it the other way round, as a
  countdown that picks up the same date when it needs one: `1d 12h · 19 Aug`.

That last line matters more than it looks. Codex reports whatever its last session wrote, so on a
machine that has not run Codex since Tuesday the percentage *is* Tuesday's — and a stale number
that looks live is the one you would plan around.

Every card uses the same row heights, taken from the busiest one, so two agents with different
numbers of windows still line up.

Press `r` to read everything again, `esc` to close.

### Where the numbers come from

- **Codex** records the rate limits OpenAI returns on every turn into its own session log, so the
  numbers are exact and reading them needs no network. Switchboard reads the end of the newest
  rollout file rather than the whole thing, and falls back through the three most recent sessions
  if the newest one has not made a request yet.
- **Claude Code** persists no quota anywhere: its local stats are token accounting, and a
  transcript only learns about a limit after it has already been hit. The real number comes from
  the endpoint the in-session `/usage` command calls, so this card calls it too, with the OAuth
  token Claude Code already stores (the macOS keychain, or `~/.claude/.credentials.json`). macOS
  asks for keychain permission the first time. The token is passed to `curl` through a pipe, never
  on a command line, because a command line is readable by every process you own.

This is the only surface in the plugin that makes a network request, and the only one that reads a
credential. The request runs on a worker thread with `usage.timeout_ms`, so the popup opens
immediately with Codex already on screen and fills the other card in when it arrives.

### Which account

Every card names the account it reports on, because the two agents are routinely signed in as two
different addresses and a percentage means little without knowing whose it is.

Codex has no command that prints it — `codex login status` says only "Logged in using ChatGPT" — so
the address comes from the `email` claim of the ID token in `~/.codex/auth.json`. The signature is
not verified, because nothing here trusts the token, it only labels a card.
Claude Code caches its own profile in `~/.claude.json`, which is a settings file rather than a
credential store, and that is also where the Claude card gets its plan — the usage endpoint names
none.

Nothing from either file is logged, drawn, or sent anywhere except the labels described here.

### When the plan renews

A window reset says when you may work again. A **renewal** says when the plan is charged and its
allowance starts over — a different question, and the one that decides whether pacing is worth it
at all. It is the `renews` row on each card.

Codex states it: the same ID token that carries the address also carries
`chatgpt_subscription_active_until`, so the date is read offline, from a file already open, with no
extra request. Anthropic states it nowhere readable — not in the usage endpoint, not in
`~/.claude.json` — so the Claude card says `unknown` rather than counting a month from the day the
subscription was created. On this account that is billed through Apple, the real charge date is
Apple's and is not visible here at all; a computed date would look like a fact and be a guess.

A date that has already passed also reads as `unknown`. Codex's claim is only refreshed while Codex
runs, so a machine left alone for a month still holds the previous period's date — and a stale date
under a heading that says *renews* reads as one that is coming.

### Colour

Claude grades each of its limits itself, and the card uses that grade — the provider knows what its
own plan considers close to the edge. Codex grades nothing, so its windows use
`usage.warn_percent` (yellow) and `usage.alert_percent` (red). The two cards can therefore colour by
two different rules, which is deliberate: a provider's own word beats a threshold invented here.

### When something cannot be read

Anything unreadable says so on its own card — a denied keychain, a machine with no Codex sessions,
an endpoint that changed shape — and never takes the other agent's card down with it. A plan bucket
the account does not have (Opus-only weekly, for instance) is left out rather than drawn as zero.
Copilot, Cursor, OpenCode, and Gemini publish no quota anywhere readable, so they are not listed.

Choose which providers appear, and in what order, with `usage.providers`.

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
