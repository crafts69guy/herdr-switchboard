//! The git menu — the whole `--git` mode, which is the whole `Prefix + g` pane.
//!
//! It is **not** an overlay on the picker any more. `Prefix + g` opens a herdr
//! pane of its own (`[[panes]] id = "git"` → `bin/git.sh` → this mode), which
//! loads no sources, spawns no preview worker, and reads no update cache: the
//! menu is on screen as fast as a `git rev-parse` allows. The repo is the pane's
//! own cwd.
//!
//! Selecting a row resolves a concrete command — which repo, which base branch,
//! which commit — and hands it to `bin/review.sh`, which **`exec`s over this very
//! process**, in this very pane: `tuicr` for review, `lazygit` for staging, or a
//! custom `menu.conf` command. Quitting the tool closes the pane and herdr returns
//! to the pane you pressed `Prefix + g` in. That is why the pane is a full-frame
//! overlay rather than a popup — a small popup would hand tuicr a tiny window.
//!
//! Every shell-out goes through the [`CommandRunner`] seam and happens **outside**
//! [`Git::on_key`]: base-branch detection when the menu opens, and a sub-list fetch
//! when `on_key` returns [`Step::Load`]. So the whole key surface is IO-free and
//! unit-testable, and `gh` talking to GitHub leaves the menu on screen rather than
//! freezing a half-drawn list.

mod effect;
mod handoff;
mod menu_config;
mod review_archive;
mod view;

use std::env;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use ratatui::Frame;

use crate::agent_handoff::{discover_targets, AgentTarget, TargetResolution, TargetScope};
use crate::data::{Config, Theme};
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::runner::{CommandRunner, SystemRunner};
use crate::surface::{Surface, Transition};
use effect::{count_tracked_files, detect_base_branch, load_rows, read_menu_conf, repo_cwd};
use handoff::{deliver, HandoffRequest};
use view::{draw, fuzzy_match};

#[cfg(test)]
use effect::review_rows;
#[cfg(test)]
use menu_config::parse_menu_conf;
#[cfg(test)]
use view::thousands;

/// The resolved review command, passed to `bin/review.sh` as environment. `mode`
/// picks the tool + shape; `arg` carries the one variable piece (a base ref for
/// `branch`, a number for `pr`, a session slug for `comments`); `custom` is the
/// shell command for a `menu.conf` entry. Everything is a plain string so the
/// launcher stays a thin `case`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewSpec {
    pub mode: String,
    pub cwd: String,
    pub arg: String,
    pub custom: String,
    pub label: String,
}

/// One row of a sub-list — a pull request, a saved review session. The four
/// fields are what the list can draw, not what any one source happens to have:
/// `id` is the argument the dispatch carries (a PR number, a session slug), and
/// the rest is display.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    /// The main line: a PR title, a session anchor.
    pub label: String,
    /// The dim column beside the id — a date.
    pub meta: String,
    /// A third fact for the pinned header: an author, a comment count.
    pub detail: String,
}

/// Which sub-list a menu row opens. All three are fetched on demand: a `gh` call,
/// a `tuicr` call, and a `git status` are too slow to make while merely drawing the
/// menu — and the conflict set changes under the menu while it is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ListKind {
    PullRequests,
    Reviews,
    ArchivedReviews,
    Conflicts,
    Agents,
}

impl ListKind {
    /// The card title's tail.
    fn title(self) -> &'static str {
        match self {
            ListKind::PullRequests => "pull requests",
            ListKind::Reviews => "saved reviews",
            ListKind::ArchivedReviews => "archived reviews",
            ListKind::Conflicts => "conflicts",
            ListKind::Agents => "review agents",
        }
    }

    /// The `review.sh` mode a picked row dispatches with.
    fn mode(self) -> Option<&'static str> {
        Some(match self {
            ListKind::PullRequests => "pr",
            ListKind::Reviews | ListKind::ArchivedReviews => "comments",
            ListKind::Conflicts => "conflict",
            ListKind::Agents => return None,
        })
    }

    /// What Enter does, for the command bar.
    fn verb(self) -> &'static str {
        match self {
            ListKind::PullRequests => "review PR",
            ListKind::Reviews | ListKind::ArchivedReviews => "read comments",
            ListKind::Conflicts => "open file",
            ListKind::Agents => "send review",
        }
    }

    /// Shown when the fetch came back with nothing.
    fn empty(self) -> &'static str {
        match self {
            ListKind::PullRequests => "  (no open pull requests)",
            ListKind::Reviews => "  (no saved reviews)",
            ListKind::ArchivedReviews => "  (no archived reviews)",
            ListKind::Conflicts => "  (no conflicted files)",
            ListKind::Agents => "  (no promptable agents; blocked agents cannot receive prompts)",
        }
    }
}

/// What a key press left for the caller to do. Keeping the fetch out here is what
/// keeps [`Git::on_key`] IO-free and unit-testable.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// Nothing to do. `show` going false means the user backed out.
    Stay,
    /// A row opened a sub-list: fetch it with [`load_rows`] and hand the result
    /// back through [`Git::show_list`].
    Load(ListKind),
    /// `chosen` holds a resolved [`ReviewSpec`]; dispatch it.
    Chosen,
    /// An all-files review wants its size checked first: run
    /// [`count_tracked_files`] and hand the number back through
    /// [`Git::show_count`]. The IO stays out here for the same reason
    /// [`Step::Load`]'s does.
    CountFiles,
    /// Discover the exact origin agent or the candidates for a target picker.
    LoadTargets(String),
    /// Deliver one saved-review session pointer to one exact Herdr pane.
    Deliver(HandoffRequest),
    /// Persist one saved review's Switchboard-owned archive visibility.
    SetReviewArchived { slug: String, archived: bool },
}

/// A custom row read from `menu.conf` (`key|icon|label|shell command`).
#[derive(Clone, Debug, PartialEq)]
pub struct Custom {
    pub key: char,
    pub icon: String,
    pub label: String,
    pub cmd: String,
}

/// What a menu row does when activated.
#[derive(Clone, Debug, PartialEq)]
enum Act {
    /// A `review.sh` mode with no extra argument resolved here: worktree,
    /// commits, lazygit.
    Review(&'static str),
    /// The whole-tree review — the one row big enough to need a size check
    /// before it opens.
    AllFiles,
    /// Review a branch against `base` (resolved at open); empty base still opens,
    /// `review.sh` falls back to the working tree alone.
    Branch,
    /// Open a sub-list rather than dispatching.
    List(ListKind),
    /// A `menu.conf` shell command, run verbatim.
    Custom(String),
}

/// One row of the top-level menu.
struct Item {
    /// Mnemonic — pressing it activates the row directly, like the old fzf `--expect`.
    key: char,
    icon: String,
    label: String,
    act: Act,
}

/// Which list the overlay is showing.
#[derive(Debug, PartialEq)]
enum View {
    Menu,
    List,
    /// The size warning standing in front of an all-files review.
    Confirm,
}

#[derive(Clone)]
struct ListSnapshot {
    rows: Vec<Row>,
    kind: ListKind,
    query: String,
    lsel: usize,
}

/// The menu itself. `chosen` is the resolved command a successful activation
/// leaves behind; [`main`] reads it after the loop and `exec`s `review.sh` with it.
pub struct Git {
    pub show: bool,
    /// The repo the verbs act on — the pane's cwd.
    cwd: String,
    /// Short label for the card title and the resolved spec.
    label: String,
    /// Detected base branch for the `branch` review, `None` when none resolves.
    base: Option<String>,
    /// The open sub-list and what it is; empty while the menu is showing.
    rows: Vec<Row>,
    kind: Option<ListKind>,
    /// The sub-list fuzzy filter, and the `rows` indices it currently keeps (all
    /// of them when empty). `lsel` indexes into `filtered`, not `rows`.
    query: String,
    filtered: Vec<usize>,
    items: Vec<Item>,
    view: View,
    sel: usize,
    lsel: usize,
    /// Set by a successful activation; taken by the picker to dispatch.
    pub chosen: Option<ReviewSpec>,
    /// The all-files review waiting on a confirmation, and the tracked-file
    /// count that asked for one. Enter promotes `pending` to `chosen`.
    pending: Option<ReviewSpec>,
    /// The saved-review session being handed off while the agent picker is open.
    handoff_session: Option<String>,
    /// Saved list state restored when Esc backs out of the agent picker.
    return_list: Option<ListSnapshot>,
    target_scope: Option<TargetScope>,
    status_message: Option<String>,
    error_message: Option<String>,
    count: usize,
    /// Ask before an all-files review over this many tracked files; 0 never asks.
    warn_at: usize,
    /// Where the last draw put everything a pointer can land on.
    zones: Zones,
}

/// Click targets published by the last draw. Written only by the draw functions
/// and read only by [`Git::on_click`]: the card is recentred every frame, so
/// this is the only thing that knows where it actually landed.
#[derive(Default)]
struct Zones {
    /// The whole card. A click outside it is not this menu's business.
    card: Rect,
    /// The menu body. One item is one line and nothing wraps, so the row under
    /// the pointer is `y - menu.y` with no offset to apply.
    menu: Rect,
    /// The sub-list body and the page it was drawn with — stored rather than
    /// recomputed, so a click and its render cannot disagree about which page
    /// is on screen.
    body: Rect,
    page_start: usize,
    /// The command bar, each pill carrying the key its cap advertises.
    bar_row: u16,
    bar_zones: Vec<(u16, u16, KeyEvent)>,
}

impl Git {
    pub fn new() -> Self {
        Git {
            show: false,
            cwd: String::new(),
            label: String::new(),
            base: None,
            rows: Vec::new(),
            kind: None,
            query: String::new(),
            filtered: Vec::new(),
            items: Vec::new(),
            view: View::Menu,
            sel: 0,
            lsel: 0,
            chosen: None,
            pending: None,
            handoff_session: None,
            return_list: None,
            target_scope: None,
            status_message: None,
            error_message: None,
            count: 0,
            warn_at: 0,
            zones: Zones::default(),
        }
    }

    /// Build the menu for `cwd` and open it. `base` is resolved by the caller
    /// through the runner so this stays IO-free; `has_lazygit` / `has_gh` hide the
    /// rows whose binary is missing; `customs` are the `menu.conf` rows, appended
    /// after the built-ins.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        cwd: String,
        label: String,
        base: Option<String>,
        has_lazygit: bool,
        has_gh: bool,
        customs: Vec<Custom>,
        all_files_warn: usize,
    ) {
        // Nerd Font (nf-md) glyphs, one per row so every label lines up: worktree
        // is uncommitted edits (pencil), branch a diff against the base
        // (source-branch), commits the log (clock), all-files a whole-tree read
        // (file), conflicts an unfinished merge (alert), pull request an incoming
        // change (source-pull), saved reviews the comments left on one
        // (comment-multiple), archived reviews their Switchboard-hidden subset,
        // lazygit the git surface.
        let mut items = vec![
            Item {
                key: 'd',
                icon: "󰏫".into(),
                label: "review worktree".into(),
                act: Act::Review("worktree"),
            },
            Item {
                key: 'b',
                icon: "󰘬".into(),
                label: match &base {
                    Some(b) => format!("review branch ({b})"),
                    None => "review branch".into(),
                },
                act: Act::Branch,
            },
            Item {
                key: 'h',
                icon: "󰋚".into(),
                label: "review commits".into(),
                act: Act::Review("commits"),
            },
            Item {
                key: 'a',
                icon: "󰈔".into(),
                label: "review all files".into(),
                act: Act::AllFiles,
            },
            // Unconditional, like `saved reviews`: probing for a merge in progress
            // would put a `git status` on the path to the menu's first frame, and an
            // empty sub-list says "no conflicted files" just as clearly as a missing
            // row would — without the row moving around under the mnemonic.
            Item {
                key: 'x',
                icon: "󰞇".into(),
                label: "conflicts".into(),
                act: Act::List(ListKind::Conflicts),
            },
        ];
        // `gh` is a soft dependency: without it the row would only ever fail, so
        // it is not offered at all.
        if has_gh {
            items.push(Item {
                key: 'p',
                icon: "󰓁".into(),
                label: "review pull request".into(),
                act: Act::List(ListKind::PullRequests),
            });
        }
        items.push(Item {
            key: 'r',
            icon: "󰅺".into(),
            label: "saved reviews".into(),
            act: Act::List(ListKind::Reviews),
        });
        items.push(Item {
            key: 'R',
            icon: "󰀼".into(),
            label: "archived reviews".into(),
            act: Act::List(ListKind::ArchivedReviews),
        });
        if has_lazygit {
            items.push(Item {
                key: 'l',
                icon: "󰊢".into(),
                label: "lazygit".into(),
                act: Act::Review("lazygit"),
            });
        }
        // Built-in mnemonics win over a duplicate custom key: skip a custom whose key
        // an earlier row already claimed, matching the old menu's built-ins-first order.
        for c in customs {
            if items.iter().any(|i| i.key == c.key) {
                continue;
            }
            items.push(Item {
                key: c.key,
                icon: c.icon,
                label: c.label,
                act: Act::Custom(c.cmd),
            });
        }

        self.cwd = cwd;
        self.label = label;
        self.base = base;
        self.rows = Vec::new();
        self.kind = None;
        self.items = items;
        self.view = View::Menu;
        self.sel = 0;
        self.lsel = 0;
        self.query.clear();
        self.refilter();
        self.chosen = None;
        self.pending = None;
        self.handoff_session = None;
        self.return_list = None;
        self.target_scope = None;
        self.status_message = None;
        self.error_message = None;
        self.count = 0;
        self.warn_at = all_files_warn;
        self.show = true;
    }

    /// Show a fetched sub-list after `GitSurface`'s background adapter answers.
    /// An empty result still opens, saying so, rather than silently doing nothing
    /// when a mnemonic is pressed.
    fn show_list(&mut self, kind: ListKind, rows: Vec<Row>) {
        self.kind = Some(kind);
        self.rows = rows;
        self.view = View::List;
        self.lsel = 0;
        self.query.clear();
        self.status_message = None;
        self.error_message = None;
        self.refilter();
    }

    fn list_title(&self) -> &'static str {
        match (self.kind, self.target_scope) {
            (Some(ListKind::Agents), Some(TargetScope::SameWorktree)) => "agents · same worktree",
            (Some(ListKind::Agents), Some(TargetScope::SameDirectory)) => "agents · same directory",
            (Some(ListKind::Agents), Some(TargetScope::AllAgents)) => "agents · all running",
            (Some(kind), _) => kind.title(),
            _ => "list",
        }
    }

    /// Apply background target discovery. An exact origin starts delivery at
    /// once; otherwise the current saved-review list is pushed underneath the
    /// temporary agent picker so Esc can restore it exactly.
    fn show_targets(&mut self, session: String, targets: TargetResolution) -> Step {
        self.status_message = None;
        self.error_message = None;
        self.handoff_session = Some(session.clone());
        self.target_scope = Some(targets.scope);
        if let Some(target) = targets.origin {
            return self.begin_delivery(session, target);
        }

        self.return_list = self.kind.map(|kind| ListSnapshot {
            rows: self.rows.clone(),
            kind,
            query: self.query.clone(),
            lsel: self.lsel,
        });
        let rows = targets
            .choices
            .into_iter()
            .map(|target| Row {
                id: target.pane_id,
                label: target.agent,
                meta: target.status,
                detail: target.cwd,
            })
            .collect();
        self.show_list(ListKind::Agents, rows);
        Step::Stay
    }

    fn restore_review_list(&mut self) {
        let Some(snapshot) = self.return_list.take() else {
            self.view = View::Menu;
            return;
        };
        self.rows = snapshot.rows;
        self.kind = Some(snapshot.kind);
        self.query = snapshot.query;
        self.lsel = snapshot.lsel;
        self.view = View::List;
        self.handoff_session = None;
        self.target_scope = None;
        self.status_message = None;
        self.error_message = None;
        self.refilter();
    }

    fn begin_delivery(&mut self, session: String, target: AgentTarget) -> Step {
        self.status_message = Some(format!("Sending review to {}…", target.agent));
        self.error_message = None;
        Step::Deliver(HandoffRequest {
            repo: self.cwd.clone(),
            session,
            target,
        })
    }

    fn delivery_failed(&mut self) {
        self.status_message = None;
        self.error_message =
            Some("Could not send review; the agent may be blocked or unavailable.".into());
    }

    fn set_selected_review_archived(&mut self, archived: bool) -> Step {
        let Some(slug) = self
            .filtered
            .get(self.lsel)
            .and_then(|&i| self.rows.get(i))
            .map(|row| row.id.clone())
        else {
            return Step::Stay;
        };
        self.status_message = Some(if archived {
            "Archiving review…".into()
        } else {
            "Restoring review…".into()
        });
        self.error_message = None;
        Step::SetReviewArchived { slug, archived }
    }

    fn review_archive_finished(&mut self, slug: &str, archived: bool, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.rows.retain(|row| row.id != slug);
                self.refilter();
                self.status_message = Some(if archived {
                    "Review archived.".into()
                } else {
                    "Review restored.".into()
                });
                self.error_message = None;
            }
            Err(error) => {
                self.status_message = None;
                self.error_message = Some(format!(
                    "Could not {} review: {error}",
                    if archived { "archive" } else { "restore" }
                ));
            }
        }
    }

    /// Recompute `filtered` from `query`: the `rows` indices whose id, label, or
    /// detail fuzzily match, in source order (no re-ranking, so a list that
    /// arrived newest-first stays that way). Keeps `lsel` in range.
    fn refilter(&mut self) {
        let q = self.query.clone();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                q.is_empty() || fuzzy_match(&q, &format!("{} {} {}", r.id, r.label, r.detail))
            })
            .map(|(i, _)| i)
            .collect();
        if self.lsel >= self.filtered.len() {
            self.lsel = 0;
        }
    }

    /// Resolve the selected menu row into a [`ReviewSpec`], or ask the caller to
    /// fetch a sub-list.
    fn activate(&mut self) -> Step {
        let Some(item) = self.items.get(self.sel) else {
            return Step::Stay;
        };
        let (mode, arg, custom) = match &item.act {
            Act::Review(m) => (m.to_string(), String::new(), String::new()),
            Act::Branch => (
                "branch".to_string(),
                self.base.clone().unwrap_or_default(),
                String::new(),
            ),
            Act::Custom(cmd) => ("custom".to_string(), String::new(), cmd.clone()),
            Act::List(kind) => return Step::Load(*kind),
            Act::AllFiles => {
                self.pending = Some(self.spec("all-files", String::new(), String::new()));
                // A threshold of 0 turns the question off entirely — including
                // the `git ls-files` it would have been asked from.
                return if self.warn_at == 0 {
                    self.take_pending()
                } else {
                    Step::CountFiles
                };
            }
        };
        self.pending = Some(self.spec(&mode, arg, custom));
        self.take_pending()
    }

    /// The review this menu resolves to, in `review.sh`'s vocabulary.
    fn spec(&self, mode: &str, arg: String, custom: String) -> ReviewSpec {
        ReviewSpec {
            mode: mode.to_string(),
            cwd: self.cwd.clone(),
            arg,
            custom,
            label: self.label.clone(),
        }
    }

    /// Promote the pending review to `chosen` and close the menu.
    fn take_pending(&mut self) -> Step {
        let Some(spec) = self.pending.take() else {
            return Step::Stay;
        };
        self.chosen = Some(spec);
        self.show = false;
        Step::Chosen
    }

    /// Hand back what [`count_tracked_files`] found. Over `warn_at` this opens
    /// the confirmation; at or under it — and when the count could not be taken
    /// at all — the review dispatches unchanged, because a number git refuses to
    /// give must not become a locked door in front of a working feature.
    fn show_count(&mut self, count: Option<usize>) -> Step {
        match count {
            Some(n) if n > self.warn_at => {
                self.count = n;
                self.view = View::Confirm;
                Step::Stay
            }
            _ => self.take_pending(),
        }
    }

    /// Dispatch the selected sub-list row in its list's mode.
    fn activate_row(&mut self) -> Step {
        let (Some(kind), Some(row)) = (
            self.kind,
            self.filtered.get(self.lsel).and_then(|&i| self.rows.get(i)),
        ) else {
            return Step::Stay;
        };
        if kind == ListKind::Agents {
            let Some(session) = self.handoff_session.clone() else {
                return Step::Stay;
            };
            return self.begin_delivery(
                session,
                AgentTarget {
                    pane_id: row.id.clone(),
                    agent: row.label.clone(),
                    status: row.meta.clone(),
                    cwd: row.detail.clone(),
                },
            );
        }
        let Some(mode) = kind.mode() else {
            return Step::Stay;
        };
        self.chosen = Some(ReviewSpec {
            label: format!("{} · {}", self.label, row.id),
            ..self.spec(mode, row.id.clone(), String::new())
        });
        self.show = false;
        Step::Chosen
    }

    /// Handle a key. The returned [`Step`] says what the caller must do next;
    /// `esc`/`q` step back a view, then close, and the caller keeps `^c` as quit.
    fn on_key(&mut self, k: KeyEvent) -> Step {
        match self.view {
            // The size warning: two ways out and nothing else, so a stray key
            // can neither open a minutes-long read nor lose the menu.
            View::Confirm => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.pending = None;
                    self.view = View::Menu;
                }
                KeyCode::Enter => return self.take_pending(),
                _ => {}
            },
            View::Menu => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.show = false,
                KeyCode::Down | KeyCode::Char('j') => self.step(1),
                KeyCode::Up | KeyCode::Char('k') => self.step(-1),
                KeyCode::Home | KeyCode::Char('g') => self.sel = 0,
                KeyCode::End | KeyCode::Char('G') => self.sel = self.items.len().saturating_sub(1),
                KeyCode::Enter => return self.activate(),
                // A mnemonic activates its row directly, wherever the cursor is —
                // bare only, so a modified chord (`^a` in a sub-list) can never
                // fall through into the row that happens to share its letter.
                KeyCode::Char(c)
                    if !k.modifiers.contains(KeyModifiers::CONTROL)
                        && !k.modifiers.contains(KeyModifiers::ALT) =>
                {
                    if let Some(i) = self.items.iter().position(|it| it.key == c) {
                        self.sel = i;
                        return self.activate();
                    }
                }
                _ => {}
            },
            // A sub-list is a fuzzy picker: printable keys type into the filter, so
            // navigation moves to the arrows (and readline `^n`/`^p`).
            View::List => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match k.code {
                    // esc clears a query first, then backs out to the menu.
                    KeyCode::Esc => {
                        if self.query.is_empty() {
                            if self.kind == Some(ListKind::Agents) {
                                self.restore_review_list();
                            } else {
                                self.view = View::Menu;
                            }
                        } else {
                            self.query.clear();
                            self.refilter();
                        }
                    }
                    KeyCode::Down => self.lstep(1),
                    KeyCode::Up => self.lstep(-1),
                    KeyCode::Char('n') if ctrl => self.lstep(1),
                    KeyCode::Char('p') if ctrl => self.lstep(-1),
                    KeyCode::Char('a')
                        if ctrl
                            && matches!(
                                self.kind,
                                Some(ListKind::Reviews | ListKind::ArchivedReviews)
                            ) =>
                    {
                        if let Some(session) = self
                            .filtered
                            .get(self.lsel)
                            .and_then(|&i| self.rows.get(i))
                            .map(|row| row.id.clone())
                        {
                            self.status_message = Some("Finding review agents…".into());
                            self.error_message = None;
                            return Step::LoadTargets(session);
                        }
                    }
                    KeyCode::Char('d') if ctrl && self.kind == Some(ListKind::Reviews) => {
                        return self.set_selected_review_archived(true);
                    }
                    KeyCode::Char('d') if ctrl && self.kind == Some(ListKind::ArchivedReviews) => {
                        return self.set_selected_review_archived(false);
                    }
                    KeyCode::Home => self.lsel = 0,
                    KeyCode::End => self.lsel = self.filtered.len().saturating_sub(1),
                    KeyCode::Enter => return self.activate_row(),
                    KeyCode::Backspace => {
                        self.query.pop();
                        self.refilter();
                    }
                    KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                        self.query.push(c);
                        self.refilter();
                    }
                    _ => {}
                }
            }
        }
        Step::Stay
    }

    fn step(&mut self, d: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        self.sel = ((self.sel as i32 + d).rem_euclid(n as i32)) as usize;
    }

    /// Wheel over the card: scroll a sub-list body by moving the selection (which
    /// pages the list). A no-op in the menu, which has nothing to scroll.
    fn on_wheel(&mut self, d: i32) {
        match self.view {
            View::List => self.lstep(d),
            // The menu is a list too, and a wheel that moved nothing here read
            // as a dead pane next to five pickers where it works.
            View::Menu => self.step(d),
            View::Confirm => {}
        }
    }

    /// A left click, resolved against the zones the last draw published.
    ///
    /// Returns a [`Step`] for the same reason [`Git::on_key`] does: a click on a
    /// row can start a fetch or a review, and `GitSurface` must schedule that effect.
    ///
    /// One rule, shared with every other Switchboard surface: a click selects,
    /// and a click on the row *already* selected does what Enter would. There is
    /// no double-click event to lean on, and a single click that ran the row
    /// would let a stray one `exec` tuicr over the pane.
    fn on_click(&mut self, at: Position) -> Step {
        // The bar first: a pill overlaps nothing else, and its payload is the
        // key on its cap, so this is exactly the key path.
        if let Some(event) = crate::tui::zone_at(&self.zones.bar_zones, self.zones.bar_row, at) {
            return self.on_key(event);
        }
        if !self.zones.card.contains(at) {
            return Step::Stay;
        }
        match self.view {
            View::Menu => {
                if self.zones.menu.contains(at) {
                    let row = (at.y - self.zones.menu.y) as usize;
                    if row < self.items.len() {
                        if row == self.sel {
                            return self.activate();
                        }
                        self.sel = row;
                    }
                }
            }
            View::List => {
                if self.zones.body.contains(at) {
                    let row = self.zones.page_start + (at.y - self.zones.body.y) as usize;
                    if row < self.filtered.len() {
                        if row == self.lsel {
                            return self.activate_row();
                        }
                        self.lsel = row;
                    }
                }
            }
            View::Confirm => {}
        }
        Step::Stay
    }

    fn lstep(&mut self, d: i32) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        self.lsel = ((self.lsel as i32 + d).rem_euclid(n as i32)) as usize;
    }
}

impl Default for Git {
    fn default() -> Self {
        Self::new()
    }
}

/// Entry point for `herdr-switchboard --git` — the entire `Prefix + g` pane.
///
/// Deliberately narrow: no `load_all`, no preview worker, no update cache, no
/// recency file. The only IO before the first frame is the git reads the menu
/// needs to label itself.
pub fn main() -> Result<()> {
    let runner = SystemRunner;
    let cfg = Config::try_load()?;
    let theme = Theme::load();
    let title = theme
        .resolve(&cfg.common.title_color)
        .unwrap_or_else(|| theme.or("accent", Color::Cyan));
    let background = crate::tui::SurfaceBackground::resolve(&theme, cfg.common.transparency);

    // Not a repo: say so in one line and let the pane close, rather than opening
    // a menu whose every row would fail.
    let Some(cwd) = repo_cwd(&runner) else {
        println!("Switchboard git menu: this pane is not inside a git repository.");
        return Ok(());
    };
    let label = std::path::Path::new(&cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());

    let base = detect_base_branch(&runner, &cwd, &cfg.git.base_branch);
    let has = |bin: &str| runner.ok("sh", &["-c", &format!("command -v {bin} >/dev/null 2>&1")]);
    let (has_lazygit, has_gh) = (has("lazygit"), has("gh"));
    let mut g = Git::new();
    g.open(
        cwd.clone(),
        label,
        base,
        has_lazygit,
        has_gh,
        read_menu_conf(),
        cfg.git.all_files_warn,
    );

    crate::surface::run(&mut GitSurface {
        git: &mut g,
        cwd: &cwd,
        origin_pane: env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default(),
        theme: &theme,
        background,
        title,
        notifier: Notifier::new(&cfg),
        effect: None,
    })?;
    let Some(spec) = g.chosen.take() else {
        return Ok(());
    };

    // Re-claim only for the pre-roll. The universal host has already restored
    // its lease, so every normal/error exit is safe; this short second lease is
    // deliberately handed to the review process after drawing its first frame.
    crate::surface::preroll(|frame| {
        crate::splash::draw(
            frame,
            frame.area(),
            &theme,
            background,
            title,
            "Opening review",
        )
    });

    let script_dir = env::var("HERDR_PLUGIN_ROOT")
        .map(|r| format!("{r}/bin"))
        .unwrap_or_else(|_| ".".into());
    let result = crate::action::run_review(&spec, &script_dir);
    // Only an exec failure returns. Restore the pre-roll lease before carrying
    // that error back to the shell.
    crate::surface::restore_terminal();
    result
}

struct GitSurface<'a> {
    git: &'a mut Git,
    cwd: &'a str,
    origin_pane: String,
    theme: &'a Theme,
    background: crate::tui::SurfaceBackground,
    title: Color,
    notifier: Notifier,
    effect: Option<Receiver<GitEffect>>,
}

enum GitEffect {
    Rows(ListKind, Vec<Row>),
    FileCount(Option<usize>),
    Targets(String, TargetResolution),
    Delivered(Result<(), String>),
    ReviewArchived {
        slug: String,
        archived: bool,
        result: Result<(), String>,
    },
}

impl Surface for GitSurface<'_> {
    type Output = ();

    fn draw(&mut self, frame: &mut Frame) {
        draw(
            frame,
            frame.area(),
            self.theme,
            self.background,
            self.title,
            self.git,
        );
    }

    fn tick_rate(&self) -> Duration {
        if self.effect.is_some() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(250)
        }
    }

    fn on_tick(&mut self) -> Result<Transition<Self::Output>> {
        let Some(receiver) = &self.effect else {
            return Ok(Transition::Wait);
        };
        let effect = match receiver.try_recv() {
            Ok(effect) => effect,
            Err(TryRecvError::Empty) => return Ok(Transition::Wait),
            Err(TryRecvError::Disconnected) => {
                self.effect = None;
                self.git.delivery_failed();
                return Ok(Transition::Redraw);
            }
        };
        self.effect = None;
        Ok(match effect {
            GitEffect::Rows(kind, rows) => {
                self.git.show_list(kind, rows);
                Transition::Redraw
            }
            GitEffect::FileCount(count) => {
                if let Step::Chosen = self.git.show_count(count) {
                    Transition::Exit(())
                } else {
                    Transition::Redraw
                }
            }
            GitEffect::Targets(session, targets) => {
                let step = self.git.show_targets(session, targets);
                self.apply_step(step)
            }
            GitEffect::Delivered(result) => match result {
                Ok(()) => {
                    self.git.status_message = None;
                    self.git.show = false;
                    self.notifier
                        .send(NotifyEvent::ReviewHandoffSucceeded, None);
                    Transition::Exit(())
                }
                Err(_) => {
                    self.git.delivery_failed();
                    Transition::Redraw
                }
            },
            GitEffect::ReviewArchived {
                slug,
                archived,
                result,
            } => {
                self.git.review_archive_finished(&slug, archived, result);
                Transition::Redraw
            }
        })
    }

    fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'))
                {
                    return Ok(Transition::Exit(()));
                }
                if self.effect.is_some() {
                    return Ok(Transition::Wait);
                }
                let step = self.git.on_key(key);
                Ok(self.apply_step(step))
            }
            Event::Mouse(mouse) => {
                if self.effect.is_some() {
                    return Ok(Transition::Wait);
                }
                let step = match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        self.git.on_wheel(1);
                        Step::Stay
                    }
                    MouseEventKind::ScrollUp => {
                        self.git.on_wheel(-1);
                        Step::Stay
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.git.on_click(Position::new(mouse.column, mouse.row))
                    }
                    _ => return Ok(Transition::Wait),
                };
                Ok(self.apply_step(step))
            }
            _ => Ok(Transition::Wait),
        }
    }
}

impl GitSurface<'_> {
    fn apply_step(&mut self, step: Step) -> Transition<()> {
        match step {
            Step::Load(kind) => {
                let (sender, receiver) = mpsc::channel();
                let cwd = self.cwd.to_string();
                std::thread::spawn(move || {
                    let rows = load_rows(&SystemRunner, &cwd, kind);
                    let _ = sender.send(GitEffect::Rows(kind, rows));
                });
                self.effect = Some(receiver);
                Transition::Redraw
            }
            Step::CountFiles => {
                let (sender, receiver) = mpsc::channel();
                let cwd = self.cwd.to_string();
                std::thread::spawn(move || {
                    let count = count_tracked_files(&SystemRunner, &cwd);
                    let _ = sender.send(GitEffect::FileCount(count));
                });
                self.effect = Some(receiver);
                Transition::Redraw
            }
            Step::LoadTargets(session) => {
                let (sender, receiver) = mpsc::channel();
                let cwd = self.cwd.to_string();
                let origin_pane = self.origin_pane.clone();
                let effect_session = session.clone();
                std::thread::spawn(move || {
                    let targets = discover_targets(&SystemRunner, &cwd, &origin_pane);
                    let _ = sender.send(GitEffect::Targets(effect_session, targets));
                });
                self.effect = Some(receiver);
                Transition::Redraw
            }
            Step::Deliver(request) => {
                let (sender, receiver) = mpsc::channel();
                std::thread::spawn(move || {
                    let result =
                        deliver(&SystemRunner, &request).map_err(|error| error.to_string());
                    let _ = sender.send(GitEffect::Delivered(result));
                });
                self.effect = Some(receiver);
                Transition::Redraw
            }
            Step::SetReviewArchived { slug, archived } => {
                let (sender, receiver) = mpsc::channel();
                std::thread::spawn(move || {
                    let result =
                        review_archive::set(&slug, archived).map_err(|error| error.to_string());
                    let _ = sender.send(GitEffect::ReviewArchived {
                        slug,
                        archived,
                        result,
                    });
                });
                self.effect = Some(receiver);
                Transition::Redraw
            }
            Step::Chosen => Transition::Exit(()),
            Step::Stay if !self.git.show => Transition::Exit(()),
            Step::Stay => Transition::Redraw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;
    use crossterm::event::KeyModifiers;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// The menu as `Prefix + g` builds it in a repo with both optional binaries,
    /// and with the all-files confirmation at its shipped threshold.
    fn open_default(g: &mut Git) {
        g.open(
            "/repo".into(),
            "repo".into(),
            Some("main".into()),
            true,
            true,
            vec![],
            1500,
        );
    }

    fn rows() -> Vec<Row> {
        vec![
            Row {
                id: "12".into(),
                label: "add login".into(),
                meta: "2026-08-01".into(),
                detail: "ada".into(),
            },
            Row {
                id: "13".into(),
                label: "fix logout".into(),
                meta: "2026-08-02".into(),
                detail: "bea".into(),
            },
        ]
    }

    /// A surface over `git`, with no effect outstanding and a cwd that is not a
    /// repository — so any background `git` a test does start exits at once.
    fn surface<'a>(git: &'a mut Git, theme: &'a Theme) -> GitSurface<'a> {
        GitSurface {
            git,
            cwd: "/definitely/not/a/repository",
            origin_pane: "w1:p1".into(),
            theme,
            background: crate::tui::SurfaceBackground::resolve(
                theme,
                crate::config::Transparency::Transparent,
            ),
            title: Color::Yellow,
            // Never the real notifier: a delivery test would otherwise shell out
            // to `herdr notification show` on the machine running the suite.
            notifier: Notifier::silent(),
            effect: None,
        }
    }

    /// Typing filters a sub-list, and backspace takes it back — the filter is
    /// the only way through a long pull-request list.
    #[test]
    fn typing_filters_a_sub_list_and_backspace_restores_it() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        let all = g.filtered.len();
        assert!(all > 1, "the fixture has several rows");

        for character in rows()[0].label.chars().take(4) {
            g.on_key(key(KeyCode::Char(character)));
        }
        assert!(!g.query.is_empty(), "the query took the keys");
        assert!(g.filtered.len() <= all, "the list narrowed");

        while !g.query.is_empty() {
            g.on_key(key(KeyCode::Backspace));
        }
        assert_eq!(g.filtered.len(), all, "the whole list came back");
    }

    /// The wheel moves the sub-list selection the way the arrow keys do.
    #[test]
    fn the_wheel_moves_the_sub_list_selection() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        let first = g.lsel;

        g.on_wheel(1);
        assert_ne!(g.lsel, first, "the wheel moved the sub-list");
        g.on_wheel(-1);
        assert_eq!(g.lsel, first);
    }

    /// Every step that needs external work becomes an outstanding effect rather
    /// than a blocking call, so the menu stays responsive while it runs.
    #[test]
    fn every_background_step_becomes_an_outstanding_effect() {
        let theme = Theme::default();
        let steps = || {
            vec![
                Step::Load(ListKind::PullRequests),
                Step::CountFiles,
                Step::LoadTargets("session-1".into()),
                Step::SetReviewArchived {
                    slug: "session-1".into(),
                    archived: true,
                },
            ]
        };

        for step in steps() {
            let mut g = Git::new();
            open_default(&mut g);
            let mut s = surface(&mut g, &theme);
            assert!(
                matches!(s.apply_step(step), Transition::Redraw),
                "a background step must redraw and keep going"
            );
            assert!(s.effect.is_some(), "no background work was started");
            assert_eq!(s.tick_rate(), Duration::from_millis(50));
        }
    }

    /// The menu navigates with both the arrow keys and their Vim equivalents.
    /// `sel` is the menu's own cursor, so a rebind that lands `j` on the wrong
    /// handler would move nothing while still looking like it worked.
    #[test]
    fn the_menu_navigates_with_arrows_and_their_vim_equivalents() {
        let mut g = Git::new();
        open_default(&mut g);
        let first = g.sel;

        g.on_key(key(KeyCode::Down));
        let after_down = g.sel;
        assert_ne!(after_down, first, "Down moved the selection");
        g.on_key(key(KeyCode::Up));
        assert_eq!(g.sel, first);

        g.on_key(key(KeyCode::Char('j')));
        assert_eq!(g.sel, after_down, "j is Down");
        g.on_key(key(KeyCode::Char('k')));
        assert_eq!(g.sel, first, "k is Up");

        // And it wraps rather than sticking at the ends.
        g.on_key(key(KeyCode::Up));
        assert_ne!(g.sel, first, "up from the first entry wraps");
    }

    /// The host may only spin fast while an effect is outstanding.
    #[test]
    fn the_git_tick_rate_is_fast_only_while_an_effect_is_in_flight() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut surface = surface(&mut g, &theme);

        assert_eq!(surface.tick_rate(), Duration::from_millis(250));
        let (_sender, receiver) = mpsc::channel();
        surface.effect = Some(receiver);
        assert_eq!(surface.tick_rate(), Duration::from_millis(50));
    }

    /// Loading a list leaves the menu responsive rather than freezing on a
    /// half-drawn list — that is the whole reason `on_key` returns a `Step`.
    #[test]
    fn a_list_step_becomes_an_outstanding_effect_rather_than_a_blocking_call() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut surface = surface(&mut g, &theme);

        assert!(matches!(
            surface.apply_step(Step::Load(ListKind::PullRequests)),
            Transition::Redraw
        ));
        assert!(surface.effect.is_some(), "the load runs in the background");

        // While it is outstanding, input is ignored rather than queued against a
        // list that is about to be replaced.
        let ignored = surface
            .on_event(Event::Key(key(KeyCode::Char('j'))))
            .unwrap();
        assert!(matches!(ignored, Transition::Wait));
    }

    /// `Stay` means "keep the menu up" — unless the menu has already closed
    /// itself, in which case it is the way out.
    #[test]
    fn staying_exits_only_once_the_menu_has_closed_itself() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut surface = surface(&mut g, &theme);
        assert!(matches!(surface.apply_step(Step::Stay), Transition::Redraw));

        surface.git.show = false;
        assert!(matches!(
            surface.apply_step(Step::Stay),
            Transition::Exit(())
        ));

        surface.git.show = true;
        assert!(matches!(
            surface.apply_step(Step::Chosen),
            Transition::Exit(())
        ));
    }

    /// Every effect has one landing, and each clears the receiver so the menu
    /// does not stay locked against input.
    #[test]
    fn every_git_effect_lands_somewhere_and_releases_the_menu() {
        let theme = Theme::default();

        // Rows install and redraw.
        let mut g = Git::new();
        open_default(&mut g);
        let mut s = surface(&mut g, &theme);
        let (sender, receiver) = mpsc::channel();
        sender
            .send(GitEffect::Rows(ListKind::PullRequests, rows()))
            .unwrap();
        s.effect = Some(receiver);
        assert!(matches!(s.on_tick().unwrap(), Transition::Redraw));
        assert!(s.effect.is_none(), "the effect is consumed");

        // A successful handoff closes the pane.
        let mut g = Git::new();
        open_default(&mut g);
        let mut s = surface(&mut g, &theme);
        let (sender, receiver) = mpsc::channel();
        sender.send(GitEffect::Delivered(Ok(()))).unwrap();
        s.effect = Some(receiver);
        assert!(matches!(s.on_tick().unwrap(), Transition::Exit(())));
        assert!(!s.git.show);

        // A failed one stays open so it can be retried.
        let mut g = Git::new();
        open_default(&mut g);
        let mut s = surface(&mut g, &theme);
        let (sender, receiver) = mpsc::channel();
        sender
            .send(GitEffect::Delivered(Err("agent is busy".into())))
            .unwrap();
        s.effect = Some(receiver);
        assert!(matches!(s.on_tick().unwrap(), Transition::Redraw));
        assert!(s.git.show, "a failed delivery keeps the menu up");
    }

    /// An archive write reports back through the same seam, so the row's marker
    /// only changes once the state file has actually been written.
    #[test]
    fn an_archive_result_comes_back_before_the_row_changes() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut s = surface(&mut g, &theme);
        let (sender, receiver) = mpsc::channel();
        sender
            .send(GitEffect::ReviewArchived {
                slug: "session-1".into(),
                archived: true,
                result: Ok(()),
            })
            .unwrap();
        s.effect = Some(receiver);
        assert!(matches!(s.on_tick().unwrap(), Transition::Redraw));
        assert!(s.effect.is_none());
    }

    /// A background thread that dies without answering must release the menu
    /// rather than leaving it locked against every key forever.
    #[test]
    fn a_dead_git_effect_thread_releases_the_menu() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut s = surface(&mut g, &theme);
        let (sender, receiver) = mpsc::channel::<GitEffect>();
        s.effect = Some(receiver);
        drop(sender);

        assert!(matches!(s.on_tick().unwrap(), Transition::Redraw));
        assert!(s.effect.is_none(), "the menu accepts input again");
    }

    /// Nothing outstanding, and an effect that has not answered yet, are both
    /// plain waits.
    #[test]
    fn an_idle_or_pending_git_tick_changes_nothing() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut s = surface(&mut g, &theme);
        assert!(matches!(s.on_tick().unwrap(), Transition::Wait));

        let (_sender, receiver) = mpsc::channel::<GitEffect>();
        s.effect = Some(receiver);
        assert!(matches!(s.on_tick().unwrap(), Transition::Wait));
        assert!(s.effect.is_some(), "still outstanding");
    }

    /// `^c` leaves from anywhere, including while an effect is still running —
    /// it is the one key that must never be swallowed by the in-flight guard.
    #[test]
    fn ctrl_c_leaves_even_while_an_effect_is_outstanding() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut s = surface(&mut g, &theme);
        let (_sender, receiver) = mpsc::channel();
        s.effect = Some(receiver);

        let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(s.on_event(event).unwrap(), Transition::Exit(())));
    }

    /// The wheel moves the selection and a click routes through the same hit
    /// test the keyboard uses.
    #[test]
    fn the_wheel_and_a_click_reach_the_menu() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut s = surface(&mut g, &theme);

        let wheel = |kind| {
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 4,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert!(matches!(
            s.on_event(wheel(MouseEventKind::ScrollDown)).unwrap(),
            Transition::Redraw
        ));
        assert!(matches!(
            s.on_event(wheel(MouseEventKind::ScrollUp)).unwrap(),
            Transition::Redraw
        ));
        // A pointer event the menu does not handle costs no repaint.
        assert!(matches!(
            s.on_event(wheel(MouseEventKind::Moved)).unwrap(),
            Transition::Wait
        ));
    }

    /// The surface draws its own chrome through the shared frame.
    #[test]
    fn the_git_surface_draws_its_menu() {
        let mut g = Git::new();
        open_default(&mut g);
        let theme = Theme::default();
        let mut s = surface(&mut g, &theme);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| s.draw(frame)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("repo"), "the menu names its repository");
    }

    /// The whole screen as text, for the draw assertions.
    fn screen(g: &mut Git, w: u16, h: u16) -> String {
        let theme = Theme::default();
        let background = crate::tui::SurfaceBackground::resolve(
            &theme,
            crate::config::Transparency::Transparent,
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), &theme, background, Color::Yellow, g))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The same render `screen` does, handing back the cells rather than their
    /// symbols — what a background or a border colour has to be asserted against.
    fn buffer(g: &mut Git, theme: &Theme, w: u16, h: u16) -> ratatui::buffer::Buffer {
        buffer_with(g, theme, crate::config::Transparency::Transparent, w, h)
    }

    fn buffer_with(
        g: &mut Git,
        theme: &Theme,
        transparency: crate::config::Transparency,
        w: u16,
        h: u16,
    ) -> ratatui::buffer::Buffer {
        let background = crate::tui::SurfaceBackground::resolve(theme, transparency);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), theme, background, Color::Yellow, g))
            .unwrap();
        term.backend().buffer().clone()
    }

    /// The pane is transparent so the terminal shows through it, exactly like
    /// every other Switchboard surface. `panel_bg` is allowed as the *ink* on a
    /// key cap and on the command bar's pills — never as a fill, which is what
    /// the three hand-rolled `Block`s here used to do to the menu card, the
    /// sub-list card, and that list's search box.
    #[test]
    fn no_git_card_paints_an_opaque_background() {
        let fill = Color::Rgb(0x10, 0x12, 0x14);
        let theme = Theme::from_slots(&[("panel_bg", "#101214"), ("accent", "#6fd0a8")]);
        let mut g = Git::new();
        open_default(&mut g);

        for (view, buf) in [
            ("menu", buffer(&mut g, &theme, 60, 20)),
            ("list", {
                g.show_list(ListKind::PullRequests, rows());
                buffer(&mut g, &theme, 60, 20)
            }),
        ] {
            for y in 0..20 {
                for x in 0..60 {
                    assert_ne!(buf[(x, y)].bg, fill, "{view} view fills ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn opaque_git_views_leave_no_transparent_holes() {
        let theme = Theme::from_slots(&[("panel_bg", "#101214")]);
        let mut g = Git::new();
        open_default(&mut g);

        for (view, buf) in [
            (
                "menu",
                buffer_with(&mut g, &theme, crate::config::Transparency::Opaque, 60, 20),
            ),
            ("list", {
                g.show_list(ListKind::PullRequests, rows());
                buffer_with(&mut g, &theme, crate::config::Transparency::Opaque, 60, 20)
            }),
        ] {
            assert!(
                buf.content.iter().all(|cell| cell.bg != Color::Reset),
                "{view} left a transparent cell"
            );
        }
    }

    /// The card body itself — everything inside the frame that is not a chip —
    /// leaves the terminal showing through. Measured from the frame's own
    /// corner so it cannot drift when the card is resized or recentred.
    #[test]
    fn the_git_menu_paints_nothing_but_its_key_caps() {
        let theme = Theme::default();
        let mut g = Git::new();
        open_default(&mut g);
        let buf = buffer(&mut g, &theme, 60, 20);

        let corner = (0..20u16)
            .flat_map(|y| (0..60u16).map(move |x| (x, y)))
            .find(|&(x, y)| buf[(x, y)].symbol() == "╭")
            .expect("the card draws a rounded corner");
        let bottom = (corner.1..20)
            .find(|&y| buf[(corner.0, y)].symbol() == "╰")
            .expect("the card closes");
        // Inside the border: one cell of selection gutter, then the three-cell
        // key cap. The bar is the last inner row and is all pills.
        let caps = corner.0 + 2..corner.0 + 5;
        for y in corner.1 + 1..bottom - 1 {
            for x in corner.0 + 1..60 {
                if caps.contains(&x) {
                    continue;
                }
                assert_eq!(buf[(x, y)].bg, Color::Reset, "menu paints ({x},{y})");
            }
        }
    }

    /// herdr frames the pane in the accent itself; the plugin's own border has
    /// to recede behind it, or the card reads as a second competing frame.
    #[test]
    fn the_git_cards_border_recedes_in_overlay0() {
        let theme = Theme::from_slots(&[("accent", "#6fd0a8"), ("overlay0", "#6c7e76")]);
        let mut g = Git::new();
        open_default(&mut g);
        let buf = buffer(&mut g, &theme, 60, 20);

        let corner = (0..20)
            .flat_map(|y| (0..60).map(move |x| (x, y)))
            .find(|&(x, y)| buf[(x, y)].symbol() == "╭")
            .expect("the card draws a rounded corner");
        assert_eq!(buf[corner].fg, Color::Rgb(0x6c, 0x7e, 0x76));
    }

    #[test]
    fn counting_tracked_files_counts_what_git_lists() {
        let runner = MockRunner::new().on("ls-files", "a\nb\nc\n");
        assert_eq!(count_tracked_files(&runner, "/repo"), Some(3));
        assert_eq!(
            runner.calls(),
            vec![vec![
                "git".to_string(),
                "-C".into(),
                "/repo".into(),
                "ls-files".into()
            ]]
        );

        let broken = MockRunner::new().failing("ls-files");
        assert_eq!(count_tracked_files(&broken, "/repo"), None);
    }

    /// The bug this whole path exists for: `tuicr -A` on a large checkout reads
    /// every tracked file behind a splash that cannot say it is working, and
    /// reads as a hang. The menu says the number first.
    #[test]
    fn a_big_repo_asks_before_it_opens_an_all_files_review() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(g.on_key(key(KeyCode::Char('a'))), Step::CountFiles);
        assert!(g.chosen.is_none(), "nothing dispatched before the count");

        assert_eq!(g.show_count(Some(6699)), Step::Stay);
        assert!(g.show, "the card stays up");
        assert!(g.chosen.is_none());
        let screen = screen(&mut g, 70, 20);
        assert!(screen.contains("6,699 files"), "{screen}");
        assert!(screen.contains("open anyway"), "{screen}");

        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
        assert_eq!(g.chosen.unwrap().mode, "all-files");
    }

    /// A count git refuses to give must not become a locked door in front of a
    /// feature that worked yesterday.
    #[test]
    fn an_unreadable_count_opens_the_review_rather_than_blocking_it() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(g.on_key(key(KeyCode::Char('a'))), Step::CountFiles);
        assert_eq!(g.show_count(None), Step::Chosen);
        assert_eq!(g.chosen.unwrap().mode, "all-files");
    }

    #[test]
    fn esc_on_the_confirmation_goes_back_to_the_menu() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('a')));
        g.show_count(Some(6699));

        assert_eq!(g.on_key(key(KeyCode::Esc)), Step::Stay);
        assert!(g.show, "esc backs out of the warning, not out of the menu");
        assert_eq!(g.view, View::Menu);
        assert!(g.chosen.is_none());
        assert!(g.pending.is_none(), "a discarded review must not linger");

        // And the row still works on the way back in.
        assert_eq!(g.on_key(key(KeyCode::Char('a'))), Step::CountFiles);
        assert_eq!(g.show_count(Some(6699)), Step::Stay);
        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
    }

    /// Zero turns the question off — including the `git ls-files` it would have
    /// been asked from.
    #[test]
    fn a_zero_threshold_never_counts_at_all() {
        let mut g = Git::new();
        g.open(
            "/repo".into(),
            "repo".into(),
            Some("main".into()),
            true,
            true,
            vec![],
            0,
        );
        assert_eq!(g.on_key(key(KeyCode::Char('a'))), Step::Chosen);
        assert_eq!(g.chosen.unwrap().mode, "all-files");
    }

    #[test]
    fn thousands_groups_digits() {
        for (n, want) in [
            (0usize, "0"),
            (999, "999"),
            (1000, "1,000"),
            (6699, "6,699"),
            (1234567, "1,234,567"),
        ] {
            assert_eq!(thousands(n), want);
        }
    }

    /// One rule everywhere: a click selects, and a click on the row already
    /// selected does what Enter would. A single click that ran the row would let
    /// a stray one `exec` tuicr over the pane.
    #[test]
    fn clicking_the_selected_menu_row_runs_it_and_another_row_only_moves_the_cursor() {
        let mut g = Git::new();
        open_default(&mut g);
        // Zones exist only after a draw — the card is recentred every frame.
        let _ = screen(&mut g, 70, 24);
        let menu = g.zones.menu;

        let third = Position::new(menu.x + 3, menu.y + 2);
        assert_eq!(g.on_click(third), Step::Stay);
        assert_eq!(g.sel, 2, "the first click only moves the cursor");
        assert!(g.chosen.is_none());

        let _ = screen(&mut g, 70, 24);
        assert_eq!(g.on_click(third), Step::Chosen);
        assert_eq!(g.chosen.unwrap().mode, "commits");
    }

    /// The click has to read the *same* page the render used, or a list scrolled
    /// past its first screenful selects the wrong row — silently.
    #[test]
    fn clicking_a_list_row_reads_through_the_page_the_draw_used() {
        let many: Vec<Row> = (0..40)
            .map(|i| Row {
                id: format!("{i}"),
                label: format!("row {i}"),
                meta: "2026-08-01".into(),
                detail: "x".into(),
            })
            .collect();
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, many);
        let _ = screen(&mut g, 90, 30);
        // Walk onto the second page, then redraw so the zones follow.
        let viewport = g.zones.body.height as usize;
        g.lsel = viewport + 1;
        let _ = screen(&mut g, 90, 30);
        assert_eq!(g.zones.page_start, viewport, "the draw paged over");

        let body = g.zones.body;
        assert_eq!(g.on_click(Position::new(body.x + 3, body.y)), Step::Stay);
        assert_eq!(g.lsel, viewport, "the click lands on the page on screen");
    }

    #[test]
    fn the_git_bar_pills_are_where_their_zones_say() {
        let mut g = Git::new();
        open_default(&mut g);
        let screen = screen(&mut g, 70, 24);
        let bar = screen.lines().nth(g.zones.bar_row as usize).unwrap();
        let cells: Vec<char> = bar.chars().collect();
        for &(a, b, code) in &g.zones.bar_zones {
            let text: String = cells[a as usize..(b as usize).min(cells.len())]
                .iter()
                .collect();
            assert!(!text.trim().is_empty(), "zone for {code:?} covers blanks");
        }
        // The first pill is the one that runs the row, and it says so.
        let (a, b, code) = g.zones.bar_zones[0];
        let text: String = cells[a as usize..b as usize].iter().collect();
        assert_eq!(code, KeyEvent::from(KeyCode::Enter));
        assert!(text.contains("run"), "{text}");
    }

    /// esc on the size warning is reachable with the pointer too — it is the
    /// only way back that does not open a minutes-long read.
    #[test]
    fn clicking_esc_on_the_confirmation_goes_back() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('a')));
        g.show_count(Some(6699));
        let _ = screen(&mut g, 70, 24);

        let (a, _, code) = *g.zones.bar_zones.last().unwrap();
        assert_eq!(code, KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            g.on_click(Position::new(a + 1, g.zones.bar_row)),
            Step::Stay
        );
        assert_eq!(g.view, View::Menu);
        assert!(g.pending.is_none());
    }

    #[test]
    fn base_branch_prefers_the_configured_one_when_it_resolves() {
        let runner = MockRunner::new(); // every rev-parse succeeds
        assert_eq!(
            detect_base_branch(&runner, "/r", "develop").as_deref(),
            Some("develop")
        );
    }

    #[test]
    fn base_branch_falls_back_through_the_conventional_names() {
        // develop and main do not resolve; master does.
        let runner = MockRunner::new()
            .failing("--verify --quiet develop")
            .failing("--verify --quiet main");
        assert_eq!(
            detect_base_branch(&runner, "/r", "develop").as_deref(),
            Some("master")
        );
    }

    #[test]
    fn base_branch_is_none_when_nothing_resolves() {
        let runner = MockRunner::new().failing("rev-parse");
        assert_eq!(detect_base_branch(&runner, "/r", ""), None);
    }

    #[test]
    fn menu_conf_skips_comments_blanks_and_incomplete_rows() {
        let conf = "\
# a comment

p|X|push|git push
bad line with no pipes
k||no command|
z|Y|pull|git pull
";
        let rows = parse_menu_conf(conf);
        assert_eq!(
            rows,
            vec![
                Custom {
                    key: 'p',
                    icon: "X".into(),
                    label: "push".into(),
                    cmd: "git push".into()
                },
                Custom {
                    key: 'z',
                    icon: "Y".into(),
                    label: "pull".into(),
                    cmd: "git pull".into()
                },
            ]
        );
    }

    #[test]
    fn enter_on_worktree_resolves_a_worktree_spec_and_closes() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
        assert!(!g.show);
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "worktree");
        assert_eq!(spec.cwd, "/repo");
    }

    #[test]
    fn branch_row_carries_the_detected_base() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('b')));
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "branch");
        assert_eq!(spec.arg, "main");
    }

    /// `h` no longer opens a browser of our own — tuicr has a commit panel, so it
    /// dispatches straight into it.
    #[test]
    fn commits_and_all_files_dispatch_without_an_argument() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(g.on_key(key(KeyCode::Char('h'))), Step::Chosen);
        let spec = g.chosen.take().unwrap();
        assert_eq!(spec.mode, "commits");
        assert_eq!(spec.arg, "");

        // All-files takes the same shape, one beat later: the size check runs
        // first and a small tree comes straight back out of it.
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(g.on_key(key(KeyCode::Char('a'))), Step::CountFiles);
        assert_eq!(g.show_count(Some(12)), Step::Chosen);
        let spec = g.chosen.take().unwrap();
        assert_eq!(spec.mode, "all-files");
        assert_eq!(spec.arg, "");
    }

    #[test]
    fn the_retired_staged_mnemonic_does_nothing() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(g.on_key(key(KeyCode::Char('s'))), Step::Stay);
        assert!(g.show);
        assert!(g.chosen.is_none());
    }

    /// `^a` sends a saved review to an agent in a sub-list, and the menu's
    /// mnemonic for `a` opens an all-files review — a minutes-long read. The
    /// mnemonic arm must therefore refuse a modified chord, or a stray `^a`
    /// pressed one view too early would start that read instead of nothing.
    #[test]
    fn a_modified_chord_never_falls_through_to_a_menu_mnemonic() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(
            g.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Step::Stay
        );
        assert!(g.show);
        assert!(g.chosen.is_none());
    }

    #[test]
    fn a_list_row_asks_the_caller_to_fetch_then_dispatches_a_pick() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(
            g.on_key(key(KeyCode::Char('p'))),
            Step::Load(ListKind::PullRequests)
        );
        assert!(g.show, "the menu stays open while the fetch runs");

        g.show_list(ListKind::PullRequests, rows());
        g.on_key(key(KeyCode::Down));
        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "pr");
        assert_eq!(spec.arg, "13");
    }

    #[test]
    fn saved_reviews_dispatch_the_comments_mode_with_the_slug() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(
            g.on_key(key(KeyCode::Char('r'))),
            Step::Load(ListKind::Reviews)
        );
        g.show_list(
            ListKind::Reviews,
            vec![Row {
                id: "9f6c1b3e09a54e2a".into(),
                label: "main..HEAD".into(),
                meta: "2026-08-03".into(),
                detail: "3 comments".into(),
            }],
        );
        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "comments");
        assert_eq!(spec.arg, "9f6c1b3e09a54e2a");
    }

    fn saved_review(g: &mut Git) {
        g.show_list(
            ListKind::Reviews,
            vec![Row {
                id: "session-1".into(),
                label: "main..HEAD".into(),
                meta: "2026-08-20".into(),
                detail: "2 comments".into(),
            }],
        );
    }

    fn target(pane_id: &str, agent: &str, cwd: &str) -> AgentTarget {
        AgentTarget {
            pane_id: pane_id.into(),
            agent: agent.into(),
            status: "idle".into(),
            cwd: cwd.into(),
        }
    }

    #[test]
    fn ctrl_a_requests_targets_without_becoming_filter_text() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);

        assert_eq!(
            g.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Step::LoadTargets("session-1".into())
        );
        assert!(g.query.is_empty());
        assert_eq!(g.status_message.as_deref(), Some("Finding review agents…"));
    }

    #[test]
    fn uppercase_r_opens_archived_reviews() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(
            g.on_key(key(KeyCode::Char('R'))),
            Step::Load(ListKind::ArchivedReviews)
        );
    }

    #[test]
    fn ctrl_d_archives_a_saved_review_and_preserves_the_filter() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);
        g.query = "main".into();
        g.refilter();

        assert_eq!(
            g.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Step::SetReviewArchived {
                slug: "session-1".into(),
                archived: true,
            }
        );
        assert_eq!(g.query, "main", "ctrl-d must not become filter text");

        g.review_archive_finished("session-1", true, Ok(()));
        assert!(g.rows.is_empty());
        assert_eq!(g.query, "main");
        assert_eq!(g.status_message.as_deref(), Some("Review archived."));
        assert!(screen(&mut g, 100, 28).contains("Review archived."));
    }

    #[test]
    fn ctrl_d_restores_an_archived_review_and_a_failure_keeps_the_row() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(
            ListKind::ArchivedReviews,
            vec![Row {
                id: "session-1".into(),
                label: "main..HEAD".into(),
                meta: "2026-08-20".into(),
                detail: "2 comments".into(),
            }],
        );

        assert_eq!(
            g.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Step::SetReviewArchived {
                slug: "session-1".into(),
                archived: false,
            }
        );
        g.review_archive_finished("session-1", false, Err("disk is read-only".into()));
        assert_eq!(g.rows.len(), 1, "a failed restore must keep its row");
        assert!(g
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("disk is read-only")));

        g.review_archive_finished("session-1", false, Ok(()));
        assert!(g.rows.is_empty());
        assert_eq!(g.status_message.as_deref(), Some("Review restored."));
    }

    #[test]
    fn an_exact_origin_starts_delivery_without_opening_the_agent_picker() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);
        let step = g.show_targets(
            "session-1".into(),
            TargetResolution {
                origin: Some(target("pane-1", "codex", "/repo")),
                choices: Vec::new(),
                scope: TargetScope::SameWorktree,
            },
        );

        let Step::Deliver(request) = step else {
            panic!("origin did not start delivery");
        };
        assert_eq!(request.session, "session-1");
        assert_eq!(request.target.pane_id, "pane-1");
        assert_eq!(g.kind, Some(ListKind::Reviews));
        assert_eq!(
            g.status_message.as_deref(),
            Some("Sending review to codex…")
        );
    }

    #[test]
    fn target_picker_sends_the_choice_and_esc_restores_the_saved_review() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);
        g.query = "main".into();
        g.refilter();
        assert_eq!(
            g.show_targets(
                "session-1".into(),
                TargetResolution {
                    origin: None,
                    choices: vec![target("pane-2", "claude", "/repo")],
                    scope: TargetScope::SameWorktree,
                },
            ),
            Step::Stay
        );
        assert_eq!(g.kind, Some(ListKind::Agents));
        assert_eq!(g.list_title(), "agents · same worktree");

        let Step::Deliver(request) = g.on_key(key(KeyCode::Enter)) else {
            panic!("agent selection did not start delivery");
        };
        assert_eq!(request.target.pane_id, "pane-2");
        assert_eq!(request.session, "session-1");

        g.on_key(key(KeyCode::Esc));
        assert_eq!(g.kind, Some(ListKind::Reviews));
        assert_eq!(g.query, "main");
        assert_eq!(g.rows[0].id, "session-1");
        assert!(g.handoff_session.is_none());
    }

    #[test]
    fn all_agent_fallback_and_delivery_failure_are_visible() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);
        g.show_targets(
            "session-1".into(),
            TargetResolution {
                origin: None,
                choices: vec![target("pane-3", "gemini", "/other")],
                scope: TargetScope::AllAgents,
            },
        );
        assert_eq!(g.list_title(), "agents · all running");
        g.delivery_failed();
        let rendered = screen(&mut g, 100, 28);
        assert!(rendered.contains("Could not send review"), "{rendered}");
    }

    #[test]
    fn saved_review_send_pill_keeps_its_control_modifier_when_clicked() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);
        let rendered = screen(&mut g, 100, 28);
        assert!(rendered.contains("send to agent"), "{rendered}");
        let &(start, _, _) = g
            .zones
            .bar_zones
            .iter()
            .find(|(_, _, event)| {
                event.code == KeyCode::Char('a') && event.modifiers.contains(KeyModifiers::CONTROL)
            })
            .expect("the send pill publishes Ctrl-A");
        assert_eq!(
            g.on_click(Position::new(start + 1, g.zones.bar_row)),
            Step::LoadTargets("session-1".into())
        );
    }

    #[test]
    fn saved_review_archive_pill_keeps_its_control_modifier_when_clicked() {
        let mut g = Git::new();
        open_default(&mut g);
        saved_review(&mut g);
        let rendered = screen(&mut g, 100, 28);
        assert!(rendered.contains("archive"), "{rendered}");
        let &(start, _, _) = g
            .zones
            .bar_zones
            .iter()
            .find(|(_, _, event)| {
                event.code == KeyCode::Char('d') && event.modifiers.contains(KeyModifiers::CONTROL)
            })
            .expect("the archive pill publishes Ctrl-D");
        assert_eq!(
            g.on_click(Position::new(start + 1, g.zones.bar_row)),
            Step::SetReviewArchived {
                slug: "session-1".into(),
                archived: true,
            }
        );
    }

    #[test]
    fn esc_in_a_list_steps_back_to_the_menu_not_out() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('p')));
        g.show_list(ListKind::PullRequests, rows());
        g.on_key(key(KeyCode::Esc));
        assert!(g.show, "the first esc backs out of the list, not the menu");
        g.on_key(key(KeyCode::Esc));
        assert!(!g.show, "a second esc closes the menu");
    }

    #[test]
    fn esc_clears_the_filter_before_leaving_a_list() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        g.on_key(key(KeyCode::Char('f'))); // filter query "f"
        assert_eq!(g.query, "f");
        g.on_key(key(KeyCode::Esc)); // first esc clears the query...
        assert!(g.query.is_empty());
        assert!(g.show, "clearing the filter must not close the menu");
        g.on_key(key(KeyCode::Esc)); // ...then list → menu...
        g.on_key(key(KeyCode::Esc)); // ...then the menu closes.
        assert!(!g.show);
    }

    #[test]
    fn a_custom_mnemonic_dispatches_its_command() {
        let mut g = Git::new();
        g.open(
            "/repo".into(),
            "repo".into(),
            None,
            false,
            false,
            vec![Custom {
                key: 'z',
                icon: "".into(),
                label: "prune".into(),
                cmd: "git gc".into(),
            }],
            1500,
        );
        assert_eq!(g.on_key(key(KeyCode::Char('z'))), Step::Chosen);
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "custom");
        assert_eq!(spec.custom, "git gc");
    }

    #[test]
    fn rows_whose_binary_is_missing_are_not_offered() {
        let mut g = Git::new();
        g.open(
            "/repo".into(),
            "repo".into(),
            None,
            false,
            false,
            vec![],
            1500,
        );
        assert!(g.items.iter().all(|i| i.key != 'l'), "lazygit row survived");
        assert!(
            g.items.iter().all(|i| i.key != 'p'),
            "pull request row survived"
        );
        // The rest are unconditional.
        for k in ['d', 'b', 'h', 'a', 'x', 'r', 'R'] {
            assert!(g.items.iter().any(|i| i.key == k), "row {k} is missing");
        }

        let mut g = Git::new();
        open_default(&mut g);
        assert!(g.items.iter().any(|i| i.key == 'l'));
        assert!(g.items.iter().any(|i| i.key == 'p'));
    }

    #[test]
    fn pull_requests_parse_out_of_ghs_json() {
        let runner = MockRunner::new().on(
            "pr list",
            r#"[{"number":12,"title":"add login","author":{"login":"ada"},"updatedAt":"2026-08-01T10:11:12Z"}]"#,
        );
        assert_eq!(
            load_rows(&runner, "/r", ListKind::PullRequests),
            vec![Row {
                id: "12".into(),
                label: "add login".into(),
                meta: "2026-08-01".into(),
                detail: "ada".into(),
            }]
        );
    }

    #[test]
    fn saved_reviews_parse_out_of_tuicrs_json() {
        let output = r#"[{"slug":"9f6c1b3e09a54e2a","kind":"local","anchor":"main..HEAD",
                 "updated_at":"2026-08-03T09:00:00Z","comment_count":3,"active":false},
                {"slug":"aa11","kind":"pr","anchor":"pr/7",
                 "updated_at":"2026-08-02T09:00:00Z","comment_count":1,"active":true}]"#;
        let rows = review_rows(
            Some(output.into()),
            &std::collections::BTreeSet::new(),
            false,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "9f6c1b3e09a54e2a");
        assert_eq!(rows[0].label, "main..HEAD");
        assert_eq!(rows[0].meta, "2026-08-03");
        assert_eq!(rows[0].detail, "3 comments");
        // A count of one must not read "1 comments".
        assert_eq!(rows[1].detail, "1 comment");
    }

    #[test]
    fn saved_reviews_are_partitioned_by_switchboards_archive() {
        let output = Some(
            r#"[{"slug":"active","anchor":"main","comment_count":1},
                 {"slug":"archived","anchor":"pr/7","comment_count":2}]"#
                .into(),
        );
        let archived = std::collections::BTreeSet::from(["archived".to_string(), "stale".into()]);

        let active_rows = review_rows(output.clone(), &archived, false);
        assert_eq!(
            active_rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["active"]
        );

        let archived_rows = review_rows(output, &archived, true);
        assert_eq!(
            archived_rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["archived"]
        );
    }

    /// Only the seven unmerged codes become rows: a staged edit, an unstaged edit,
    /// and an untracked file share the porcelain output and none of them is a
    /// conflict. The path is the id, because it is what `--file` is handed.
    #[test]
    fn conflicts_parse_out_of_git_status_porcelain() {
        let runner = MockRunner::new().on(
            "status --porcelain",
            "UU src/git.rs\n M src/ui.rs\nM  src/data.rs\nDU bin/old.sh\n?? junk\n",
        );
        let rows = load_rows(&runner, "/r", ListKind::Conflicts);
        assert_eq!(rows.len(), 2, "non-conflict lines became rows: {rows:?}");
        assert_eq!(rows[0].id, "src/git.rs");
        assert_eq!(rows[0].label, "src/git.rs");
        assert_eq!(rows[0].detail, "both modified");
        // No timestamp to show — the date column stays blank rather than inventing one.
        assert_eq!(rows[0].meta, "");
        assert_eq!(rows[1].id, "bin/old.sh");
        assert_eq!(rows[1].detail, "deleted by us");
    }

    /// The mnemonic opens the sub-list rather than dispatching, and the pick carries
    /// the path — `review.sh` resolves it against the repo top level from there.
    #[test]
    fn a_conflicted_file_dispatches_the_conflict_mode_with_its_path() {
        let mut g = Git::new();
        open_default(&mut g);
        assert_eq!(
            g.on_key(key(KeyCode::Char('x'))),
            Step::Load(ListKind::Conflicts)
        );
        let runner = MockRunner::new().on("status --porcelain", "UU src/git.rs\n");
        let rows = load_rows(&runner, "/repo", ListKind::Conflicts);
        g.show_list(ListKind::Conflicts, rows);
        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "conflict");
        assert_eq!(spec.arg, "src/git.rs");
        assert_eq!(spec.cwd, "/repo");
    }

    /// The two failure shapes a list fetch actually has. Neither may panic, and
    /// neither may be told apart from "you have no sessions" by the caller — the
    /// card says as much either way.
    #[test]
    fn an_empty_or_broken_fetch_is_an_empty_list() {
        for (tag, out) in [
            ("empty", "[]"),
            ("broken", "not json at all"),
            ("object", "{}"),
        ] {
            let runner = MockRunner::new().on("review list", out);
            assert!(
                load_rows(&runner, "/r", ListKind::Reviews).is_empty(),
                "{tag} output produced rows"
            );
        }
        // The binary is missing entirely.
        let runner = MockRunner::new().failing("pr list");
        assert!(load_rows(&runner, "/r", ListKind::PullRequests).is_empty());
        // Not a repo, or git refusing for any other reason.
        let runner = MockRunner::new().failing("status --porcelain");
        assert!(load_rows(&runner, "/r", ListKind::Conflicts).is_empty());
    }

    #[test]
    fn draw_renders_the_menu_card() {
        let mut g = Git::new();
        open_default(&mut g);
        let s = screen(&mut g, 80, 24);
        assert!(s.contains("Git"), "{s}");
        assert!(s.contains("review worktree"), "{s}");
        assert!(s.contains("review all files"), "{s}");
        assert!(s.contains("saved reviews"), "{s}");
        assert!(s.contains('╭'), "{s}");
        // The card must be wide enough for the whole command bar — a narrow menu
        // once clipped `esc close` to `esc clo`.
        assert!(s.contains("close"), "{s}");
    }

    #[test]
    fn a_list_shows_a_detail_header_and_its_own_verb() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        let s = screen(&mut g, 80, 24);
        assert!(s.contains("2026-08-01"), "{s}");
        assert!(s.contains("ada"), "{s}");
        assert!(s.contains("review PR"), "{s}");
        assert!(s.contains("pull requests"), "{s}");
    }

    #[test]
    fn an_empty_list_says_which_nothing_it_found() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::Reviews, vec![]);
        let s = screen(&mut g, 80, 24);
        assert!(s.contains("(no saved reviews)"), "{s}");
    }

    /// A real tuicr session slug is a whole revset. Unclipped it swallowed the
    /// row, leaving the anchor with no columns to draw in.
    #[test]
    fn a_long_id_still_leaves_room_for_the_label() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(
            ListKind::Reviews,
            vec![Row {
                id: "herdr-switchboard@main/staged-and-unstaged-and-commits/4e27385..06e9955"
                    .into(),
                label: "the anchor".into(),
                meta: "2026-08-03".into(),
                detail: "2 comments".into(),
            }],
        );
        let s = screen(&mut g, 80, 24);
        let row = s
            .lines()
            .find(|l| l.contains('▌'))
            .expect("a selected row")
            .to_string();
        assert!(
            row.contains("the anchor"),
            "the label was crowded out: {row}"
        );
        // The row clips the id; the pinned header above it still carries the whole
        // revset, so nothing is only visible in the clipped column.
        assert!(!row.contains("4e27385..06e9955"), "{row}");
        assert!(
            s.contains("4e27385..06e9955"),
            "the header lost the id: {s}"
        );
    }

    #[test]
    fn a_list_scrolls_and_shows_a_position_counter() {
        // More rows than fit; the list must page rather than overflow.
        let many: Vec<Row> = (0..60)
            .map(|i| Row {
                id: format!("{i}"),
                label: format!("pull request number {i}"),
                meta: "2026-08-01".into(),
                detail: "ada".into(),
            })
            .collect();
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, many);
        g.on_key(key(KeyCode::End)); // jump to the last row

        let s = screen(&mut g, 90, 24);
        assert!(s.contains("60/60"), "{s}");
        assert!(s.contains("pull request number 59"), "{s}");
        assert!(!s.contains("pull request number 0 "), "{s}");
    }

    #[test]
    fn wheel_moves_the_menu_selection_and_the_list_body() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_wheel(1);
        assert_eq!(g.sel, 1, "wheel-down moves the menu selection");
        g.on_wheel(-1);
        assert_eq!(g.sel, 0, "wheel-up moves it back");
        g.show_list(ListKind::PullRequests, rows());
        g.on_wheel(1);
        assert_eq!(g.lsel, 1, "wheel-down moves the list selection");
        g.on_wheel(-1);
        assert_eq!(g.lsel, 0, "wheel-up moves it back");
    }

    #[test]
    fn a_list_box_stays_fixed_size_while_filtering() {
        let many: Vec<Row> = (0..40)
            .map(|i| Row {
                id: format!("{i}"),
                label: format!("thing {i}"),
                meta: "2026-08-01".into(),
                detail: "ada".into(),
            })
            .collect();
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, many);

        // The row of the card's bottom border (the lowest `╰`).
        let card_bottom = |g: &mut Git| -> usize {
            let theme = Theme::default();
            let background = crate::tui::SurfaceBackground::resolve(
                &theme,
                crate::config::Transparency::Transparent,
            );
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 30)).unwrap();
            term.draw(|f| draw(f, f.area(), &theme, background, Color::Yellow, g))
                .unwrap();
            let buf = term.backend().buffer().clone();
            (0..30u16)
                .rev()
                .find(|&y| (0..90u16).any(|x| buf[(x, y)].symbol() == "╰"))
                .unwrap() as usize
        };

        let full = card_bottom(&mut g);
        for ch in "thing 3".chars() {
            g.on_key(key(KeyCode::Char(ch))); // narrow to a handful of matches
        }
        assert!(g.filtered.len() < 40, "the filter should have narrowed");
        assert_eq!(full, card_bottom(&mut g), "the box resized when filtering");
    }

    #[test]
    fn a_list_places_the_cursor_after_the_query() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        g.on_key(key(KeyCode::Char('x'))); // query "x"
        let theme = Theme::default();
        let background = crate::tui::SurfaceBackground::resolve(
            &theme,
            crate::config::Transparency::Transparent,
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &theme, background, Color::Yellow, &mut g))
            .unwrap();
        let pos = term.get_cursor_position().unwrap();
        // The cursor sits after the icon prefix and the one-char query, on the
        // first body row — not stranded at the origin.
        assert!(pos.x > 4, "cursor x={}", pos.x);
        assert!(pos.y > 0, "cursor y={}", pos.y);
    }

    #[test]
    fn fuzzy_match_is_a_case_insensitive_subsequence() {
        assert!(fuzzy_match("", "anything"));
        assert!(fuzzy_match("abc", "aXbYc"));
        assert!(fuzzy_match("FIX", "fix the thing"));
        assert!(!fuzzy_match("abc", "acb")); // order matters
        assert!(!fuzzy_match("z", "abc"));
    }

    #[test]
    fn typing_filters_a_list_and_enter_picks_a_match() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        for ch in "fix".chars() {
            g.on_key(key(KeyCode::Char(ch)));
        }
        // Only "fix logout" is a subsequence match.
        assert_eq!(g.filtered.len(), 1);
        assert_eq!(g.on_key(key(KeyCode::Enter)), Step::Chosen);
        assert_eq!(g.chosen.unwrap().arg, "13");
    }
}
