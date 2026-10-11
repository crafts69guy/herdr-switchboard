# Herdr agent coordination research

Reviewed 2026-10-11 against official documentation. These are documented capabilities; proposed
Switchboard behavior is separated below. Installed CLI syntax was also inspected; runtime behavior
must be verified against the installed server rather than inferred from a documentation version.

## Verified capabilities and boundaries

- A workspace groups project tabs, panes and agents. Multiple terminal clients can attach to the
  same session; separate named sessions have separate identities.
  Sources: [Quick start](https://herdr.dev/docs/quick-start/), [Concepts](https://herdr.dev/docs/concepts/).
- `agent prompt` submits text and Enter, rejects blocked agents and optionally waits for lifecycle
  state. It does not identify individual task turns. A timeout or stalled prompt may already have
  delivered input; inspect before retrying. Reading recent terminal output may not recover full
  alternate-screen history. Source: [Agent automation](https://herdr.dev/docs/agent-automation/).
- Worktree creation supports a base revision and no-focus operation; it returns the workspace,
  tab, root pane and checkout records. Agent startup uses an existing shell pane. Agent names
  identify live occupants and are cleared when replaced.
  Sources: [CLI reference](https://herdr.dev/docs/cli-reference/), [Socket API](https://herdr.dev/docs/socket-api/).
- Native `agent_session` references are optional identity metadata, not shared conversation
  content. Restoring a native conversation is not transferring its context to another provider.
  Sources: [Socket API](https://herdr.dev/docs/socket-api/), [Session state](https://herdr.dev/docs/session-state/).
- Remote automation exists through `--machine`, but requires consistent endpoint selection and
  does not make local file paths available remotely. It is outside this implementation.
  Source: [Connecting machines](https://herdr.dev/docs/connecting-machines/).

## Context guidance and resulting design choices

Anthropic recommends curating high-signal context, fetching referenced information when needed,
and keeping structured notes for long-running work. This supports source-written task summaries,
file references and checkpoints rather than copying terminal transcripts.
Source: [Effective context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents).

Switchboard adds its own task ID, explicit claim/report protocol and generation fencing because
Herdr lifecycle observations cannot prove completion of a specific assignment. These are
Switchboard contracts, not guarantees from Herdr. Each task receives a worktree from a pinned
commit; handoff keeps that worktree. Results require review before integration. Private state
retains context, while the existing Herdr prompt seam carries only a reference and protocol text.

Remaining limits: terminal delivery still has a check/write race; filesystem ownership is
cooperative; agent test reports require review; opaque commands cannot guarantee exactly-once
delivery across crashes. Uncertain outcomes are retained for inspection and never automatically retried.
