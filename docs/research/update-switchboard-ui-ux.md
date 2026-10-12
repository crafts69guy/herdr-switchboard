# Update Switchboard UI/UX research

Reviewed 2026-10-12 against the local updater, Ratatui documentation, Crossterm 0.28,
and Herdr v0.8.0 source. This note separates verified behavior from chosen design
and performance hypotheses. Current module ownership remains in
[architecture.md](../architecture.md).

## Findings

The existing update action uses a dedicated Herdr pane but presents Bash output,
ANSI styling, and a `press any key to close` prompt. Installation and binary
preparation run sequentially. Preparation failure is logged, but the final message
still says the update succeeded. These are observations from the pre-change local
updater, not results from running an installation.

Herdr supports noninteractive installation with `--yes`, pinning with `--ref`,
and replacing a GitHub-managed checkout. Locally linked checkouts are protected
from replacement. Source:
[Herdr 0.8.0 CLI reference](https://github.com/herdrdev/herdr/blob/master/docs/versions/0.8.0/website/src/content/docs/cli-reference.mdx).

In Herdr v0.8.0, `git_checkout` fetches the requested ref when supplied; without
one, it fetches `origin HEAD` and detaches at `FETCH_HEAD`. Therefore the old
unpinned install command does not guarantee the latest tagged release. Source:
[Herdr v0.8.0 installer source](https://github.com/herdrdev/herdr/blob/v0.8.0/src/cli/plugin.rs).

Ratatui rebuilds a frame in memory and emits differences between the current and
previous buffers. Its FAQ explicitly allows avoiding an async runtime when the
application does not otherwise need one. Sources:
[Rendering under the hood](https://ratatui.rs/concepts/rendering/under-the-hood/),
[Ratatui FAQ](https://ratatui.rs/faq/#when-should-i-use-tokio-and-asyncawait).

Crossterm's blocking `read` can be paired with `poll`; those APIs must remain on
the same thread and must not be mixed with `EventStream`. Terminal restoration
also matters on errors and panics. Sources:
[Crossterm 0.28 event API](https://docs.rs/crossterm/0.28.1/crossterm/event/index.html),
[Ratatui panic hooks](https://ratatui.rs/recipes/apps/panic-hooks/).

Determinate progress is appropriate when completion can be measured; unknown
duration calls for indeterminate feedback and explanatory text. Source:
[Microsoft progress controls](https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/progress-controls).

Color should not be the sole way to communicate state, and waiting, completion,
and errors should have explicit status text. These are applicable design
principles, not evidence of TUI screen-reader support or WCAG compliance. Sources:
[W3C use of color](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html),
[W3C status messages](https://www.w3.org/WAI/WCAG22/Understanding/status-messages.html).

## Chosen design

- Keep the dedicated Update pane, using the shared Rust `Surface` host and theme.
  Center a card of at most 72 columns by 16 rows inside the Herdr overlay; accommodate the actual terminal
  area, including smaller panes, through the existing layout patterns.
- Show the current version and resolved target version, a concise stage label,
  indeterminate activity feedback, and a consistent footer. Stage changes reflect
  actual command boundaries: checking installation/releases, installing, preparing
  the binary, and the final outcome. Do not infer download/build percentages or ETA.
- Resolve the highest stable numeric release tag and explicitly pass that tag to
  `herdr plugin install --ref ... --yes`. Reject prereleases and malformed tags;
  failure to resolve a release stops before installation rather than falling back
  to `HEAD`. Tag pinning selects a release; it is not a cryptographic authenticity
  guarantee or an immutable commit pin.
- Start slow work after the first frame on a worker through `CommandRunner` and
  return typed stage/outcome messages to the surface. Keep input and terminal
  ownership in `surface`; reuse the existing binary-preparation policy.
- Keep diagnostic details bounded and sanitized for display; expose them on
  demand without making raw subprocess output the primary screen. Do not allow
  child output to interleave with Ratatui rendering.
- While a command is running, closing requests display a waiting message and
  retain the pane until the operation completes. Do not label this as cancellation
  or introduce process-tree cancellation in this change.
- Keep success, blocked installation, error, and partial success visible until
  dismissed. If installation succeeded but binary preparation failed, explicitly
  say the installed version changed and preparation will retry on the next open.
  Do not claim rollback or report that the whole update failed before any changes.
- Preserve fail-closed handling of local or unreadable installation provenance.
  Give linked-checkout users manual maintenance guidance rather than attempting
  a managed reinstall.

These are Switchboard decisions derived from the findings, not requirements
imposed by Ratatui, Microsoft, or W3C. No new async runtime, animation library,
release channel setting, or generic job framework is needed.

## Performance and validation boundaries

Expected benefits are earlier visible feedback, responsive resize/input while
commands run, and fewer terminal-mode transitions. They remain hypotheses until
measured; a worker does not make installation or compilation itself faster.
Redraw on stage changes and only tick activity feedback while running. The tick
interval is an implementation choice, not a published optimal frame rate.

Validate the flow with fake command responses: stable-tag ordering and rejection,
local/unknown source refusal, install failure, preparation failure after successful
installation, success, and close requests during work. Render representative
states with the test backend at the intended overlay size and a small terminal.
Manual QA should verify theme consistency, resize behavior, final-message
readability, and terminal restoration in a disposable Herdr session.

If streamed output is added later, drain stdout and stderr safely rather than
waiting on a child whose pipes can fill; dropping a `Child` neither waits for it
nor terminates it. Sources:
[Rust process documentation](https://doc.rust-lang.org/std/process/),
[Rust Child documentation](https://doc.rust-lang.org/std/process/struct.Child.html).

## Implementation validation

Validated 2026-10-12 on macOS arm64:

- `bash bin/check.sh` passed: 885 Rust tests and all shell specifications.
- `bash bin/coverage.sh` passed the 98% line gate (98.01% on this host).
- `bash demo/render.sh update` recorded the
  [progress card](../media/update.png) and [result card](../media/update-result.png)
  in the isolated Herdr sandbox using an installer and release response that are
  entirely fake. Version 9.9.9 in these images is fixture data.
- The [synthetic PTY probe](../../demo/update-perf.py), run with
  `python3 demo/update-perf.py` after a release build, verified first-frame
  feedback before worker IO, resize during installation, ignored busy Esc, and
  normal exit after dismissing the result. Recorded samples measured 13 ms and
  548 ms first frames including process/PTY initialization, 0.2–0.3 ms resize
  repaint responses, and 0.093–0.128 CPU seconds including subprocesses over
  4.719–5.707 wall seconds. Both results produced zero terminal bytes during one
  idle second; these samples do not establish a latency distribution.

These are narrow synthetic observations, not an installation-throughput
comparison, optimal tick-rate claim, or production latency guarantee. The model
tests also verify that continuous input cannot starve worker completion or dismiss
a result before its first frame. No real release installation, memory benchmark,
or accessibility audit was performed. Linux coverage still belongs to CI.

A forcibly closed pane or terminated process remains outside the
waiting-before-close guarantee; Switchboard does not implement its own rollback.
