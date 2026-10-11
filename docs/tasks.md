# Tasks

Tasks coordinates agents across panes, tabs, workspaces and terminal windows attached to the same
local Herdr server. Open **Tasks** in the Central Menu, or invoke the `switchboard.tasks` action.
No default Herdr binding is added. `enter` focuses the assigned agent; `ctrl-enter` focuses its
worktree workspace. The worktree is also discoverable in Projects. Use `status:active`,
`agent:codex`, `repo:project`, and fuzzy text to filter. `alt-j` / `alt-k` scroll the preview.
Bindings live in `[keys.tasks]` as `focus` and `task_worktree`.

## CLI and context

Run the installed Switchboard binary from inside a Herdr pane. For a linked checkout, build with
`cargo build` and use `target/debug/herdr-switchboard`; agents receive its absolute executable path.
The commands below use `herdr-switchboard` for brevity. They are fish-compatible.

Create a JSON request file (maximum 64 KiB):

```json
{
  "goal": "Add an empty-state message to the search results",
  "repo": "/absolute/path/to/repo",
  "base": "HEAD",
  "scope": "Search results renderer and its tests",
  "acceptance": "An empty query result renders a useful message; tests pass",
  "constraints": "Keep the public search API unchanged",
  "references": ["src/search.rs", "AGENTS.md"],
  "decisions": "Reuse the current theme",
  "tried": "Existing tests establish the nonempty result layout",
  "open_questions": "",
  "expected_result": "Summary, changed files and commands/tests run"
}
```

Required: `goal`, absolute `repo`, `scope`, `acceptance`, `expected_result`. `base` defaults to HEAD;
the remaining fields default empty. Unknown fields are rejected. Resolve file references relative
to the task worktree; external references must identify their source explicitly. Context is data
selected by the source agent, not an automatically captured conversation or terminal transcript.
Do not include credentials in any request, report or checkpoint.

```fish
herdr-switchboard task delegate --file request.json --kind codex
herdr-switchboard task list
herdr-switchboard task show t00000001
herdr-switchboard task wait t00000001 --timeout 120
```

Choose a kind supported and installed in Herdr. Delegation creates a uniquely named branch and
worktree workspace, starts the agent without changing focus, then submits a pointer to the task.
The base is resolved to a commit SHA before creation. Uncommitted source changes are **not copied**;
`source_dirty` and a visible warning explain this. Neither dependencies nor credentials are copied
or installed. Agents handle project setup within the task's constraints.

Every response has `schema_version: 1` and either `result` on stdout or `error` on stderr. Errors
exit 1 and include a stable `code`; successful commands exit 0. IDs are scoped to the inherited
Herdr endpoint. `show` includes the task, separate `agent_status`, and protocol instructions.
`wait` requires a timeout in seconds (1–86400); it returns when the task settles or needs attention,
including an unavailable/blocked agent. Inspect the returned task status: a successful wait does
not necessarily mean successful work. A timeout never sends another prompt.

## Receiving and reporting

Only the assigned agent pane can claim/report/release. The CLI verifies the live named agent,
its pane, cwd, native conversation identity when available, and the server socket instance.

```fish
herdr-switchboard task claim t00000001 --generation 1
herdr-switchboard task report t00000001 --generation 1 --file result.json
```

Example result:

```json
{
  "status": "completed",
  "summary": "Added the empty-state message and regression test",
  "tests": ["cargo test empty_results: passed"],
  "artifacts": ["src/search.rs"]
}
```

Status must be `completed`, `failed` or `needs_help`; all four fields are required. Report checks
the current Git HEAD, branch, dirty state and changed/untracked paths itself. Test claims remain
the agent's report, not independent certification. An identical report is idempotent; a different
report cannot replace a terminal result. `needs_help` can later become completed/failed or be released.
Neither task completion nor agent Done merges code, deletes the branch or removes the worktree.
Review the result first, then integrate through your normal Git workflow.

## Continuing with another agent

The current owner writes a checkpoint and releases the task:

```json
{
  "progress": "Renderer and unit tests are implemented; changes are uncommitted",
  "decisions": "Reused the existing muted text style",
  "tried": "Unit tests passed",
  "remaining": "Exercise narrow terminal layouts",
  "next_step": "Run the render tests at 40 and 80 columns",
  "verification": "cargo test empty_results passed"
}
```

```fish
herdr-switchboard task release t00000001 --generation 1 --file checkpoint.json
# The releasing agent must now stop editing this task.
herdr-switchboard task handoff t00000001 --kind claude
```

Handoff adds a tab and new agent in the existing task workspace. It keeps dirty files and previous
checkpoints, increments generation, and requires the new owner to claim that generation. A late
report from generation 1 cannot modify generation 2. Old panes remain open; Switchboard never
interrupts another agent or answers an approval prompt. Ownership is cooperative, not filesystem
write isolation.

## Recovery and storage

The private registry is stored beneath `$XDG_STATE_HOME/herdr-switchboard/tasks/` (default
`~/.local/state/herdr-switchboard/tasks/`), partitioned by socket path, with owner-only files and
locked atomic updates. It survives closing the picker. Malformed or unsupported state is refused,
never replaced with an empty registry. Preserve damaged files for recovery.

Provisioning records `step` before external mutations. If a create/start/send response is lost,
the task is retained as `unconfirmed`; inspect `task list`, `task show` and Herdr before taking
another action. There is no automatic retry. If the assigned agent received the prompt it can
claim the same generation, proving receipt. Failure before assignment requires a new explicit
delegation after inspecting retained resources. Do not blindly repeat delegate: it creates a new task.

Busy, blocked and unknown agents receive no task prompt. Resolve their UI manually; the assigned
agent can then read and claim the retained task. A stale server or replaced agent cannot report
against old ownership. Released tasks can be explicitly handed off after checking the restored
workspace; active tasks whose owner is permanently lost require manual recovery of their worktree
and checkpoint. No automatic recovery claims ownership of unfinished edits.

Task results and worktrees are retained until deliberately removed through existing workflows.
This version has no scheduler, auto-merge, remote transport or context summarization API. Older
Herdr builds without worktree/agent commands cannot provision Tasks; other Switchboard features
retain their existing minimum version. See the [research notes](research/agent-coordination.md).

## Agent checklist

1. Source: write a focused request, identify the base commit, choose the agent kind, delegate once.
2. Receiver: show, claim, read the repository's agent rules and references, then work in its worktree.
3. Report evidence and remaining risks. Use `needs_help` for questions; never infer completion from idle.
4. To transfer: checkpoint, release, stop editing; the next agent claims the new generation.
5. Coordinator: wait/show, review reported changes and tests, integrate separately.
