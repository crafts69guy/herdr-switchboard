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
| `projects` / `picker` / `git` | `Surface` adapters and typed outputs | Surface-specific state reduction and composition |
| `action` | `Accept`, `dispatch`, `open_target` | Restored-terminal effects and process replacement |
| `runner` | `CommandRunner` | `SystemRunner` and `MockRunner` process adapters |
| `usage` | `main`, feature-private `Provider` | Refresh runtime, quota adapters, time formatting, rendering |

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

## Architecture audit

This audit was taken on 2026-08-20 against Ratatui 0.29. External guidance was rechecked on the
same date. File length is a navigation signal, not a design rule: a long cohesive module can have a
better interface than several shallow pass-through modules. Production and test lines are recorded
separately because moving tests alone would make a file smaller without improving its structure.

| Feature file | Production lines | Test lines | Main responsibilities currently colocated |
| --- | ---: | ---: | --- |
| `usage.rs` | 1,683 | 915 | Provider protocols, credential and network adapters, parsing, time formatting, runtime, rendering |
| `git.rs` | 1,624 | 836 | State reduction, command effects, response parsing, configuration, runtime, rendering |
| `main.rs` | 1,108 | 479 | Crate composition, CLI dispatch, Projects model, reduction, runtime |
| `zen.rs` | 1,059 | 551 | Geometry, persistence, pane discovery, orchestration, picker mode |
| `settings.rs` | 1,019 | 244 | Setting catalogue, document persistence, form state, interaction, rendering |
| `commands.rs` | 997 | 169 | Catalogue, shell parsers, persistence, picker mode, terminal actions |

The repository already has the right architectural foundation:

- `surface` is a deep module: its small interface hides the entire terminal lease and event loop.
- `CommandRunner` is a real seam because production and test adapters both use it.
- Git already separates its IO-free `on_key` decision from typed background work.
- `View`, `Slot`, and similar enums model closed runtime states with exhaustive matches.
- `Provider` is appropriately a trait because providers are an open set with multiple adapters.

The problem is therefore not a missing application-wide pattern. It is that several feature
modules contain multiple cohesive implementations behind one physical file, while `main.rs` also
owns a complete feature. The redesign should deepen those feature modules without replacing the
universal host or creating a second event system.

### Pattern decision

The repository recommendation, inferred from the code above and the primary sources below, is
**feature-first deep modules with a local Model-Update-View reducer and typed effects**. This exact
combination is a Switchboard design decision, not a framework prescribed by Rust, Ratatui, or Elm.
A complex feature owns its state, inputs, state transitions, effects, rendering, and adapters
beneath one narrow interface.

Ratatui documents the Model/Update/View flow for mutable Rust applications but presents it as
pedagogical guidance, not a required framework:
[The Elm Architecture in Ratatui](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/).
Elm's effect model supports the typed-effect analogy: update returns commands to the runtime and
completion returns to the application as a message:
[Elm effects](https://guide.elm-lang.org/effects/).

Rust's module tree supports moving a module's implementation into child files without changing its
logical path. Items are private by default, and `pub(crate)`, `pub(super)`, and re-exports can keep a
feature's implementation behind its interface:
[separating modules into files](https://doc.rust-lang.org/stable/book/ch07-05-separating-modules-into-different-files.html)
and [visibility and privacy](https://doc.rust-lang.org/stable/reference/visibility-and-privacy.html).

Use the supporting patterns selectively:

- **Enum state machines:** use enums for mutually exclusive overlays, views, loading states, and
  confirmations. Exhaustive matching makes an unhandled state a compiler error; see Rust's
  [non-exhaustive match diagnostic](https://doc.rust-lang.org/stable/error_codes/E0004.html).
- **Ports and Adapters:** keep ports at process, network, credential, filesystem, and socket seams
  only when production and test adapters justify the indirection. A port represents the feature's
  conversation with an external actor, not every low-level operation. See Cockburn's original
  [Hexagonal Architecture](https://alistair.cockburn.us/hexagonal-architecture/).
- **Independently stateful modules:** give a nested module its own model and update path only when it
  owns a real lifecycle, as Settings does. Stateless panels and cards remain ordinary view
  functions.

Do not adopt these patterns:

- Global `model/`, `view/`, and `controller` directories: they scatter one feature across the tree
  and weaken locality. Elm's official structure guidance likewise recommends organizing larger
  programs around feature/page modules rather than global layers:
  [Elm application structure](https://guide.elm-lang.org/webapps/structure.html).
- A global Flux dispatcher or action bus: Switchboard has one host and does not need broadcast
  indirection or a second event lifecycle.
- Trait-object State implementations for closed UI states: enums are smaller, exhaustive, and
  easier to inspect. Traits remain useful for open adapter sets.
- A generic reducer or effect trait: each feature's inputs, effects, and outcomes have different
  invariants. Sharing vocabulary is enough; sharing an abstraction would make the interfaces
  shallower.
- A workspace split: all modes ship in one binary and share terminal, configuration, and process
  contracts. Modules are sufficient until a subsystem has an independent consumer or release
  lifecycle.
- A hard file-length maximum: it rewards mechanical splitting rather than depth and locality.

## Target feature shape

Each complex surface should expose one interface and extract only cohesive clusters that have enough
behavior to earn a child module:

```text
feature.rs               interface, central model/update flow, surface adapter
feature/
├── domain.rs            cohesive domain values and their pure behavior
├── effect.rs            substantial external work and completion messages
├── view.rs              substantial rendering and render-derived hit zones
└── provider.rs          an open adapter set and provider-specific children
```

This is an example, not a template. Elm's structure guidance specifically warns against defaulting
to `Model`, `Update`, and `View` modules; feature ownership and cohesive behavior should determine
the module tree. A feature with no substantial external work needs no effect child, and a small
reducer can remain in the feature root beside its model. Pure helpers stay beside the state that
gives them meaning. Tests live with the child module whose interface they exercise; moving a
monolithic test module without moving responsibility is not an architectural improvement.

Interface rules for the target structure:

- The feature interface exposes only mode entry points, CLI entry points, and output types needed
  by callers.
- Child modules are private. Use `pub(super)` for sibling implementation access and `pub(crate)`
  only when the crate has a real caller outside the feature.
- A feature-local update path performs no blocking IO. It returns typed effects or a typed outcome;
  the surface adapter performs or schedules the work and feeds results back as inputs.
- Rendering reads the model and publishes geometry-derived hit zones. It does not invoke external
  work or duplicate hit-test geometry.
- Tests primarily cross the same feature or reducer interface as callers. Keep focused parser and
  geometry tests where those pure interfaces carry meaningful behavior.
- Do not add a port for a single implementation merely to make the tree look architectural.

## Refactoring roadmap

The target design is broad, but implementation stays phase-gated. Each phase must compile, pass the
full verification suite, and leave the next phase optional.

1. **Characterize observable behavior.** Pin CLI/config contracts, surface outcomes, render states,
   terminal restoration, effect argv, credential handling, and the performance budgets below.
2. **Extract Projects from the composition root.** Leave `main.rs` responsible for module
   declarations, argv parsing, and mode dispatch. Move the Projects model, reducer, surface adapter,
   and rendering under a `projects` module. Replace independent overlay flags with one closed
   `Overlay` enum when the overlays are mutually exclusive.
3. **Make Git the reference feature.** Preserve its existing `on_key -> Step -> background effect`
   behavior while separating model/update, external loading and parsing, configuration parsing, and
   rendering. Keep `git::ReviewSpec` re-exported through the feature interface for `action`.
4. **Separate Usage adapters from presentation.** Split report-domain values, provider registry,
   Codex and Claude adapters, refresh runtime, time formatting, and rendering. Keep `Provider`
   feature-private and preserve the rule that credentials never reach argv, logs, traces, caches,
   or retained state.
5. **Separate Zen policy from effects.** Keep geometry and session codecs pure; isolate Herdr,
   socket, chrome, and persistence effects; keep the picker as the module's UI adapter.
6. **Deepen secondary features only where responsibilities are already distinct.** Settings can
   separate its catalogue, document writer, model, and view. Commands can separate its catalogue
   and shell-history ingestion from its picker adapter and terminal actions. Keep the shared picker
   intact unless concrete duplication demonstrates a better, smaller interface.

### Acceptance for every implementation phase

- Reducer tests cover navigation, overlays, loading, confirmation, cancellation, completion, and
  failure without executing real IO.
- Adapter tests use `MockRunner` or a disposable file and assert observable commands, parsed
  results, failure behavior, and secret handling.
- `TestBackend` render tests cover supported widths, transparent panels, live key labels, and hit
  zones for any changed surface.
- Run `cargo test`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings`.
- Run `tests/manifest_spec.sh`, `tests/update_guard_spec.sh`, `tests/bootstrap_spec.sh`, and
  `tests/menu_handoff_spec.sh` with Bash.
- Manually exercise every affected Herdr pane. Attach a current screenshot when layout or
  interaction changes.
- Any user-visible change receives an `[Unreleased]` changelog entry in the same implementation
  commit. Pure structural phases do not.
- CLI flags, configuration keys, terminal restoration, process handoff, credential safety, and
  synchronous Projects startup remain stable. Layout and interaction may change only when the
  phase documents the maintainability benefit and updates its acceptance tests.

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
