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

use std::env;
use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::data::{Config, Theme};
use crate::runner::{CommandRunner, SystemRunner};

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
    Conflicts,
}

impl ListKind {
    /// The card title's tail.
    fn title(self) -> &'static str {
        match self {
            ListKind::PullRequests => "pull requests",
            ListKind::Reviews => "saved reviews",
            ListKind::Conflicts => "conflicts",
        }
    }

    /// The `review.sh` mode a picked row dispatches with.
    fn mode(self) -> &'static str {
        match self {
            ListKind::PullRequests => "pr",
            ListKind::Reviews => "comments",
            ListKind::Conflicts => "conflict",
        }
    }

    /// What Enter does, for the command bar.
    fn verb(self) -> &'static str {
        match self {
            ListKind::PullRequests => "review PR",
            ListKind::Reviews => "read comments",
            ListKind::Conflicts => "open file",
        }
    }

    /// Shown when the fetch came back with nothing.
    fn empty(self) -> &'static str {
        match self {
            ListKind::PullRequests => "  (no open pull requests)",
            ListKind::Reviews => "  (no saved reviews)",
            ListKind::Conflicts => "  (no conflicted files)",
        }
    }
}

/// What a key press left for the caller to do. Keeping the fetch out here is what
/// keeps [`Git::on_key`] IO-free and unit-testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
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
    count: usize,
    /// Ask before an all-files review over this many tracked files; 0 never asks.
    warn_at: usize,
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
            count: 0,
            warn_at: 0,
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
        // (comment-multiple), lazygit the git surface.
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
        self.count = 0;
        self.warn_at = all_files_warn;
        self.show = true;
    }

    /// Show a fetched sub-list. The caller runs the command (see [`load_rows`]);
    /// an empty result still opens, saying so, rather than silently doing nothing
    /// when a mnemonic is pressed.
    pub fn show_list(&mut self, kind: ListKind, rows: Vec<Row>) {
        self.kind = Some(kind);
        self.rows = rows;
        self.view = View::List;
        self.lsel = 0;
        self.query.clear();
        self.refilter();
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
    pub fn show_count(&mut self, count: Option<usize>) -> Step {
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
        self.chosen = Some(ReviewSpec {
            label: format!("{} · {}", self.label, row.id),
            ..self.spec(kind.mode(), row.id.clone(), String::new())
        });
        self.show = false;
        Step::Chosen
    }

    /// Handle a key. The returned [`Step`] says what the caller must do next;
    /// `esc`/`q` step back a view, then close, and the caller keeps `^c` as quit.
    pub fn on_key(&mut self, k: KeyEvent) -> Step {
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
                // A mnemonic activates its row directly, wherever the cursor is.
                KeyCode::Char(c) => {
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
                            self.view = View::Menu;
                        } else {
                            self.query.clear();
                            self.refilter();
                        }
                    }
                    KeyCode::Down => self.lstep(1),
                    KeyCode::Up => self.lstep(-1),
                    KeyCode::Char('n') if ctrl => self.lstep(1),
                    KeyCode::Char('p') if ctrl => self.lstep(-1),
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
    pub fn on_wheel(&mut self, d: i32) {
        if matches!(self.view, View::List) {
            self.lstep(d);
        }
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

/// The base branch a `review branch` diffs against: the configured `base_branch`
/// when it resolves, else the first of the conventional names that does. `None`
/// when the repo has none of them (a fresh repo with no `main`/`master`).
pub fn detect_base_branch(
    runner: &dyn CommandRunner,
    cwd: &str,
    configured: &str,
) -> Option<String> {
    // `capture`, not `ok`: `ok` runs through `status`, which inherits the
    // terminal, and `rev-parse --verify` prints the resolved SHA on success —
    // that 40-char line was landing on the screen under the git overlay. `capture`
    // pipes stdout (and stderr), so the check stays silent; we only want the exit.
    let resolves = |r: &str| {
        runner
            .capture("git", &["-C", cwd, "rev-parse", "--verify", "--quiet", r])
            .is_some()
    };
    if !configured.is_empty() && resolves(configured) {
        return Some(configured.to_string());
    }
    ["main", "master", "origin/main", "origin/master"]
        .into_iter()
        .find(|r| resolves(r))
        .map(str::to_string)
}

/// How many files an all-files review would read: `git ls-files`, counted.
/// `None` when git could not answer — the review then opens without asking, the
/// same way an unreadable sub-list opens empty rather than erroring.
///
/// Counted rather than parsed, so a path containing a newline over-counts by
/// one; the only consequence is a confirmation the user would have seen anyway.
pub fn count_tracked_files(runner: &dyn CommandRunner, cwd: &str) -> Option<usize> {
    runner
        .capture("git", &["-C", cwd, "ls-files"])
        .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Fetch a sub-list. The two remote sources speak JSON and are parsed with
/// `serde_json` — **never `jq`**, which this repo does not depend on and must
/// not start depending on for a list that is only ever displayed. The conflict
/// list is `git status` porcelain, a stable line format that needs no parser.
///
/// Every failure (binary missing, not a GitHub remote, unparseable output) is an
/// empty list. The card then says so, which is the same thing the user needs to
/// know as an error would tell them, without a second failure surface.
pub fn load_rows(runner: &dyn CommandRunner, cwd: &str, kind: ListKind) -> Vec<Row> {
    match kind {
        ListKind::PullRequests => load_pull_requests(runner, cwd),
        ListKind::Reviews => load_reviews(runner, cwd),
        ListKind::Conflicts => load_conflicts(runner, cwd),
    }
}

/// The unmerged files, from `git status --porcelain`.
///
/// One command gives both the path and the status code, where
/// `git diff --name-only --diff-filter=U` gives only the path — and "deleted by
/// them" versus "both modified" is exactly what decides whether opening the file
/// is the right move. `core.quotePath=false` keeps a non-ASCII path readable
/// instead of octal-escaped.
///
/// Porcelain paths are printed from the **repo root** even when git runs inside a
/// subdirectory, which is why `review.sh` resolves the pick against
/// `rev-parse --show-toplevel` rather than against the pane's cwd.
fn load_conflicts(runner: &dyn CommandRunner, cwd: &str) -> Vec<Row> {
    let Some(out) = runner.capture(
        "git",
        &[
            "-C",
            cwd,
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain",
        ],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            // `XY<space>path`: the code is the first two columns, so a path is only
            // reachable on a line with something after them.
            if line.len() < 4 || !line.is_char_boundary(2) {
                return None;
            }
            let (code, rest) = line.split_at(2);
            let detail = unmerged_detail(code)?;
            let path = rest.trim_start();
            if path.is_empty() {
                return None;
            }
            Some(Row {
                id: path.to_string(),
                // The body column clips an id at `ID_COL`; the pinned header draws
                // the label in full, so a deep path stays readable somewhere.
                label: path.to_string(),
                meta: String::new(),
                detail: detail.to_string(),
            })
        })
        .collect()
}

/// The seven porcelain codes git calls unmerged, spelled the way `git status`
/// spells them in its long form. Anything else — a staged edit, an untracked
/// file — is not a conflict and is not offered.
fn unmerged_detail(code: &str) -> Option<&'static str> {
    Some(match code {
        "DD" => "both deleted",
        "AU" => "added by us",
        "UD" => "deleted by them",
        "UA" => "added by them",
        "DU" => "deleted by us",
        "AA" => "both added",
        "UU" => "both modified",
        _ => return None,
    })
}

/// Run a command that returns a JSON array and map each element to a [`Row`].
/// Anything that is not an array of objects — a `gh` usage error, a missing
/// binary, an object where a list was expected — is no rows.
fn json_rows(out: Option<String>, row_of: impl Fn(&serde_json::Value) -> Option<Row>) -> Vec<Row> {
    let Some(json) = out
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    else {
        return Vec::new();
    };
    let Some(items) = json.as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(row_of).collect()
}

/// `gh pr list` as rows: number, title, author, and the date it last moved.
///
/// Run through `sh -c` with a `cd` rather than `gh --repo <cwd>`: `--repo` takes
/// an `OWNER/REPO` coordinate or a URL, **not a path**, so passing the checkout
/// there is a usage error — and one that would show up as "no open pull
/// requests", indistinguishable from the truth. `gh` resolves the repo from its
/// working directory's remote on its own.
fn load_pull_requests(runner: &dyn CommandRunner, cwd: &str) -> Vec<Row> {
    let script = format!(
        "cd {} && gh pr list --limit 50 --json number,title,author,updatedAt",
        shell_quote(cwd)
    );
    json_rows(runner.capture("sh", &["-c", &script]), |pr| {
        let number = pr.get("number")?.as_u64()?;
        Some(Row {
            id: number.to_string(),
            label: pr
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            meta: day_of(pr.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("")),
            detail: pr
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    })
}

/// Single-quote a string for `sh -c`. The only input is a repository path we were
/// handed, but a path with a quote in it must become an unreadable path, never a
/// second command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `tuicr review list` as rows: the session slug is the id (it is what
/// `review comments --session` takes), with the comment count as the detail.
/// Here `--repo` **does** take a checkout path — tuicr documents it that way.
fn load_reviews(runner: &dyn CommandRunner, cwd: &str) -> Vec<Row> {
    json_rows(
        runner.capture("tuicr", &["review", "list", "--repo", cwd]),
        |s| {
            let slug = s.get("slug")?.as_str()?.to_string();
            let count = s.get("comment_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let anchor = s
                .get("anchor")
                .and_then(|v| v.as_str())
                .filter(|a| !a.is_empty())
                .unwrap_or("(no anchor)");
            Some(Row {
                id: slug,
                label: anchor.to_string(),
                meta: day_of(s.get("updated_at").and_then(|v| v.as_str()).unwrap_or("")),
                detail: match count {
                    1 => "1 comment".to_string(),
                    n => format!("{n} comments"),
                },
            })
        },
    )
}

/// The date out of an ISO-8601 timestamp — the whole string is too wide for the
/// column and the clock half of it never decides anything.
fn day_of(timestamp: &str) -> String {
    timestamp
        .split('T')
        .next()
        .unwrap_or(timestamp)
        .chars()
        .take(10)
        .collect()
}

/// Whether `dir` sits inside a git working tree.
fn is_repo(runner: &dyn CommandRunner, dir: &str) -> bool {
    runner
        .capture("git", &["-C", dir, "rev-parse", "--git-dir"])
        .is_some()
}

/// The repo the menu acts on: the pane's own cwd, which herdr sets from the pane
/// `Prefix + g` was pressed in. `SWITCHBOARD_ORIGIN_CWD` is the fallback for the case
/// where the pane started somewhere else — it is a hint, not an authority, so it
/// is only taken when it is a repo and the cwd is not.
fn repo_cwd(runner: &dyn CommandRunner) -> Option<String> {
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    if is_repo(runner, &cwd) {
        return Some(cwd);
    }
    env::var("SWITCHBOARD_ORIGIN_CWD")
        .ok()
        .filter(|s| !s.is_empty() && is_repo(runner, s))
}

/// Read the `menu.conf` custom rows from the plugin's config dir, if any.
fn read_menu_conf() -> Vec<Custom> {
    let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").unwrap_or_default();
    if dir.is_empty() {
        return Vec::new();
    }
    std::fs::read_to_string(std::path::Path::new(&dir).join("menu.conf"))
        .map(|t| parse_menu_conf(&t))
        .unwrap_or_default()
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
        .resolve(&cfg.get("title_color", "peach"))
        .unwrap_or_else(|| theme.or("accent", Color::Cyan));

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

    let base = detect_base_branch(&runner, &cwd, &cfg.get("base_branch", ""));
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

    let mut terminal = crate::init_terminal();
    let outcome = loop {
        if let Err(e) = terminal.draw(|f| draw(f, f.area(), &theme, title, &g)) {
            break Err(anyhow::Error::from(e));
        }
        match event::poll(Duration::from_millis(200)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => break Err(e.into()),
        }
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c'))
                {
                    break Ok(());
                }
                match g.on_key(k) {
                    // The fetch lives out here so `on_key` stays IO-free. It runs
                    // on the menu's frame, which is why the card keeps showing the
                    // menu while `gh` talks to GitHub.
                    Step::Load(kind) => {
                        let rows = load_rows(&runner, &cwd, kind);
                        g.show_list(kind, rows);
                    }
                    // Same reason, same place: the count is one `git ls-files`,
                    // taken on activation so the menu's first frame stays free.
                    Step::CountFiles => {
                        if let Step::Chosen = g.show_count(count_tracked_files(&runner, &cwd)) {
                            break Ok(());
                        }
                    }
                    Step::Chosen => break Ok(()),
                    // `show` going false is the user backing all the way out.
                    Step::Stay if !g.show => break Ok(()),
                    Step::Stay => {}
                }
            }
            Ok(Event::Mouse(m)) => match m.kind {
                MouseEventKind::ScrollDown => g.on_wheel(1),
                MouseEventKind::ScrollUp => g.on_wheel(-1),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    // Restore before anything can return: the loop ended holding raw mode, the
    // alternate screen, and `?1000h`, and an `Err` that skipped this would drop
    // mouse escapes into the user's shell. Only the `exec` path below is allowed
    // to leave the screen claimed, and it re-claims it deliberately.
    if outcome.is_err() || g.chosen.is_none() {
        crate::restore_terminal();
    }
    outcome?;
    let Some(spec) = g.chosen.take() else {
        return Ok(());
    };

    // The pre-roll. `review.sh` `exec`s over this process, so the next thing to
    // paint is the review tool — and tuicr can spend a second or more reading a
    // large diff before its first frame. Tearing the screen down here would leave
    // that second black. Instead: turn the wheel off (tuicr sets its own), draw
    // one static frame, and leave the alternate screen up for tuicr to paint over.
    // Raw mode and the cursor are restored for the brief cooked-mode window before
    // it claims them back.
    print!("{}", crate::MOUSE_OFF);
    let _ = io::stdout().flush();
    if let Ok(size) = terminal.size() {
        let area = Rect::new(0, 0, size.width, size.height);
        let _ = terminal.draw(|f| crate::splash::draw(f, area, &theme, title, "Opening review"));
    }
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);

    let script_dir = env::var("HERDR_PLUGIN_ROOT")
        .map(|r| format!("{r}/bin"))
        .unwrap_or_else(|_| ".".into());
    crate::action::run_review(&spec, &script_dir)
}

/// Parse a `menu.conf`: one `key|icon|label|shell command` per line. Blank lines,
/// `#` comments, and rows with an empty key or command are skipped, as the fzf
/// menu did. Only the first character of the key field is the mnemonic.
pub fn parse_menu_conf(text: &str) -> Vec<Custom> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.splitn(4, '|');
            let key = parts.next()?.trim().chars().next()?;
            let icon = parts.next().unwrap_or("").trim().to_string();
            let label = parts.next().unwrap_or("").trim().to_string();
            let cmd = parts.next().unwrap_or("").trim().to_string();
            if cmd.is_empty() {
                return None;
            }
            Some(Custom {
                key,
                icon,
                label,
                cmd,
            })
        })
        .collect()
}

/// Draw the git card centred in the pane, matching the settings/changelog shape.
pub fn draw(f: &mut Frame, area: Rect, theme: &Theme, title: Color, g: &Git) {
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    // The border recedes: herdr frames the pane in the accent already, and a
    // second accent box inside it reads as two competing frames.
    let border = theme.or("overlay0", Color::DarkGray);

    // A sub-list is a fixed-size box with its own internal layout.
    if matches!(g.view, View::List) {
        draw_list(f, area, theme, title, g);
        return;
    }
    if matches!(g.view, View::Confirm) {
        draw_confirm(f, area, theme, title, g);
        return;
    }

    let lines = menu_lines(g, ink, text, sub, accent, title);

    // Width: the wider of the content and the command bar, clamped; height: the
    // rows plus the border and a one-row command bar. The bar must be measured
    // too — a narrow menu would otherwise size the card below the pill row and
    // clip `esc close` to `esc clo`.
    let pills = bar_pills(g, theme);
    let content_w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(24);
    let want_w = content_w.max(bar_width(&pills)).clamp(24, 78) + 4;
    let w = want_w.min(area.width.saturating_sub(2));
    let want_h = lines.len() as u16 + 2 /* border */ + 1 /* bar */;
    let h = want_h.min(area.height.saturating_sub(1)).max(6);
    let popup = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, popup);

    let cap = format!("󰊢 Git · {}", g.label);
    let block = crate::tui::boxed(&cap, title, border)
        .title(Line::from(Span::styled(" · git ", Style::default().fg(sub))).right_aligned());
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(inner);
    f.render_widget(Paragraph::new(lines), rows[0]);
    draw_bar(f, g, rows[1], theme);
}

/// The size warning for an all-files review, drawn to the menu card's shape:
/// what it is about to do, how much of it there is, and the two ways out.
///
/// It exists because the pane deliberately keeps the screen, draws one static
/// splash and `exec`s the review over itself — so a tree big enough to keep
/// tuicr reading for minutes is indistinguishable from a hang. This is the only
/// frame that can still say otherwise.
fn draw_confirm(f: &mut Frame, area: Rect, theme: &Theme, title: Color, g: &Git) {
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let border = theme.or("overlay0", Color::DarkGray);

    let lines = vec![
        Line::from(vec![
            Span::styled(" 󰈔 ", Style::default().fg(title)),
            Span::styled(
                " review all files",
                Style::default().fg(text).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!(" {} files", thousands(g.count)),
                Style::default().fg(title).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" tracked in this repo.", Style::default().fg(text)),
        ]),
        Line::from(Span::styled(
            " tuicr reads every one of them before its first",
            Style::default().fg(sub),
        )),
        Line::from(Span::styled(
            " frame; on a tree this size that is minutes.",
            Style::default().fg(sub),
        )),
    ];

    let pills = bar_pills(g, theme);
    let content_w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(24);
    let want_w = content_w.max(bar_width(&pills)).clamp(24, 78) + 4;
    let w = want_w.min(area.width.saturating_sub(2));
    let want_h = lines.len() as u16 + 2 /* border */ + 1 /* bar */;
    let h = want_h.min(area.height.saturating_sub(1)).max(6);
    let popup = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, popup);

    let cap = format!("󰊢 Git · {}", g.label);
    let block = crate::tui::boxed(&cap, title, border).title(
        Line::from(Span::styled(" · large repo ", Style::default().fg(sub))).right_aligned(),
    );
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(inner);
    f.render_widget(Paragraph::new(lines), rows[0]);
    draw_bar(f, g, rows[1], theme);
}

/// `6699` → `6,699`. One number on one card; a formatting crate would be a
/// dependency for a comma.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A sub-list, drawn as a **fixed-size** card so filtering never resizes it: a
/// pinned detail header on top, a rounded search box just above the body, and —
/// the only part that scrolls — the paged row list, then the bar.
fn draw_list(f: &mut Frame, area: Rect, theme: &Theme, title: Color, g: &Git) {
    use ratatui::layout::Constraint::{Length, Min};

    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    let border = theme.or("overlay0", Color::DarkGray);

    // Size from the terminal, not the content, so the box is stable across searches.
    let w = (LIST_W as u16 + 6).min(area.width.saturating_sub(2));
    let h = area
        .height
        .saturating_sub(4)
        .clamp(16, 34)
        .min(area.height.saturating_sub(2));
    let popup = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, popup);

    let what = g.kind.map(ListKind::title).unwrap_or("list");
    let tail = if g.rows.is_empty() {
        format!(" · {what} ")
    } else if g.filtered.is_empty() {
        format!(" · {what} · no matches ")
    } else {
        format!(" · {what} · {}/{} ", g.lsel + 1, g.filtered.len())
    };
    let cap = format!("󰊢 Git · {}", g.label);
    let block = crate::tui::boxed(&cap, title, border)
        .title(Line::from(Span::styled(tail, Style::default().fg(sub))).right_aligned());
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Nothing came back: say which nothing, rather than an empty box.
    if g.rows.is_empty() {
        let rows = ratatui::layout::Layout::vertical([Min(1), Length(1)]).split(inner);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                g.kind.map(ListKind::empty).unwrap_or("  (nothing here)"),
                Style::default().fg(sub),
            ))),
            rows[0],
        );
        draw_bar(f, g, rows[1], theme);
        return;
    }

    // Header (fixed) · search box (rounded, near the body) · scrolling list · bar.
    let a =
        ratatui::layout::Layout::vertical([Length(LIST_HEADER_ROWS), Length(3), Min(1), Length(1)])
            .split(inner);
    let (header_area, search_area, body_area, bar_area) = (a[0], a[1], a[2], a[3]);

    f.render_widget(
        Paragraph::new(list_header_lines(
            g,
            text,
            sub,
            title,
            header_area.width as usize,
        )),
        header_area,
    );

    // The search input, its own rounded box.
    let sbox = crate::tui::framed(border);
    let sinner = sbox.inner(search_area);
    f.render_widget(sbox, search_area);
    let mut qline = vec![Span::styled(SEARCH_PREFIX, Style::default().fg(accent))];
    if g.query.is_empty() {
        qline.push(Span::styled(
            "type to filter",
            Style::default().fg(sub).add_modifier(Modifier::ITALIC),
        ));
    } else {
        qline.push(Span::styled(g.query.clone(), Style::default().fg(text)));
    }
    f.render_widget(Paragraph::new(Line::from(qline)), sinner);

    // Only this scrolls: the page of rows holding the selection.
    let viewport = body_area.height as usize;
    f.render_widget(
        Paragraph::new(list_body_lines(
            g,
            text,
            sub,
            accent,
            viewport,
            body_area.width as usize,
        )),
        body_area,
    );

    draw_bar(f, g, bar_area, theme);

    // The real (blinking) cursor, inside the search box after the query.
    let x = sinner.x
        + Span::raw(SEARCH_PREFIX).width() as u16
        + Span::raw(g.query.as_str()).width() as u16;
    let max_x = sinner.x + sinner.width.saturating_sub(1);
    f.set_cursor_position(Position::new(x.min(max_x), sinner.y));
}

fn menu_lines(
    g: &Git,
    ink: Color,
    text: Color,
    sub: Color,
    accent: Color,
    title: Color,
) -> Vec<Line<'static>> {
    g.items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let selected = i == g.sel;
            Line::from(vec![
                Span::styled(
                    if selected { "▌" } else { " " },
                    Style::default().fg(accent),
                ),
                Span::styled(
                    format!(" {} ", it.key),
                    Style::default()
                        .bg(if selected { title } else { accent })
                        .fg(ink)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    it.icon.clone(),
                    Style::default().fg(if selected { title } else { sub }),
                ),
                Span::raw(" "),
                Span::styled(
                    it.label.clone(),
                    Style::default().fg(if selected { text } else { sub }),
                ),
            ])
        })
        .collect()
}

/// Target content width of the fixed sub-list card, driving the card's width and
/// the label wrap.
const LIST_W: usize = 68;

/// The pinned detail header's height: one meta row plus up to two label rows.
const LIST_HEADER_ROWS: u16 = 3;

/// The date column in a list row, padded so labels line up.
const DATE_COL: usize = 14;

/// Widest an id may draw in a list row. A PR number is two characters, but a
/// tuicr session slug is a whole revset —
/// `herdr-switchboard@main/staged-and-unstaged/4e27385` — and unclipped it eats the row
/// until the label has nothing left. The pinned header still shows the id at the
/// card's full width, so nothing is only visible here.
const ID_COL: usize = 26;

/// The leading run of the sub-list search line (a leading space, the filter icon,
/// a space) — shared so `draw_list` can place the real terminal cursor exactly
/// where the query text starts.
const SEARCH_PREFIX: &str = " 󰍉 ";

/// Case-insensitive subsequence fuzzy match: every char of `needle` appears in
/// `haystack` in order. An empty needle matches everything.
fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    'needle: for nc in needle.chars().flat_map(char::to_lowercase) {
        for hc in hay.by_ref() {
            if hc == nc {
                continue 'needle;
            }
        }
        return false;
    }
    true
}

/// Truncate to `w` chars with a trailing ellipsis; short strings pass through.
fn clip(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The pinned detail header for the highlighted row — its id, date, and detail,
/// then the (wrapped, two-row-capped) label. Fixed height, so it never shrinks
/// while filtering; a clipped list row is never the only place a label appears.
/// Falls back to a placeholder when the filter matches nothing.
fn list_header_lines(
    g: &Git,
    text: Color,
    sub: Color,
    title: Color,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(r) = g.filtered.get(g.lsel).and_then(|&i| g.rows.get(i)) else {
        return vec![Line::from(Span::styled(
            "  no matches",
            Style::default().fg(sub),
        ))];
    };
    let mut meta = vec![
        Span::raw(" "),
        Span::styled(
            r.id.clone(),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ),
    ];
    if !r.meta.is_empty() {
        meta.push(Span::styled(
            format!("  {}", r.meta),
            Style::default().fg(sub),
        ));
    }
    if !r.detail.is_empty() {
        meta.push(Span::styled(
            format!("  ·  {}", r.detail),
            Style::default().fg(sub),
        ));
    }
    let mut lines = vec![Line::from(meta)];
    for (prefix, line) in crate::markdown::wrap(&r.label, width.saturating_sub(1), "", "")
        .into_iter()
        .take(2)
    {
        lines.push(Line::from(Span::styled(
            format!(" {prefix}{line}"),
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        )));
    }
    lines
}

/// The scrolling body: the page of the filtered list holding the selection, each
/// row a marker, id, date column, and clipped label. This is the only part of the
/// card that moves; `viewport` is the body area's height.
fn list_body_lines(
    g: &Git,
    text: Color,
    sub: Color,
    accent: Color,
    viewport: usize,
    width: usize,
) -> Vec<Line<'static>> {
    if g.filtered.is_empty() {
        return vec![Line::from(Span::styled(
            "  no matches",
            Style::default().fg(sub),
        ))];
    }
    let viewport = viewport.max(1);
    let len = g.filtered.len();
    let start = if len <= viewport {
        0
    } else {
        (g.lsel / viewport) * viewport
    };
    let end = (start + viewport).min(len);
    g.filtered
        .iter()
        .enumerate()
        .take(end)
        .skip(start)
        .filter_map(|(rank, &i)| {
            let r = g.rows.get(i)?;
            let selected = rank == g.lsel;
            // marker(2) + id + space + date column + space, then the label fills the rest.
            let id = clip(&r.id, ID_COL);
            let room = width.saturating_sub(5 + id.chars().count() + DATE_COL);
            let date = clip(&r.meta, DATE_COL);
            Some(Line::from(vec![
                Span::styled(
                    if selected { "▌ " } else { "  " },
                    Style::default().fg(accent),
                ),
                Span::styled(
                    format!("{id} "),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{date:<DATE_COL$} "), Style::default().fg(sub)),
                Span::styled(
                    clip(&r.label, room),
                    Style::default().fg(if selected { text } else { sub }),
                ),
            ]))
        })
        .collect()
}

/// The command-bar pills for the current view. Shared by the width calculation in
/// [`draw`] and [`draw_bar`], so the card is always sized to fit what it draws.
fn bar_pills(g: &Git, theme: &Theme) -> Vec<crate::tui::Pill<'static>> {
    match g.view {
        View::Menu => vec![
            crate::tui::Pill::new("↵", "run", theme.or("accent", Color::Cyan)),
            crate::tui::Pill::new("↑ ↓", "move", theme.or("blue", Color::Blue)),
            crate::tui::Pill::new("esc", "close", theme.or("red", Color::Red)),
        ],
        View::List => vec![
            crate::tui::Pill::new(
                "↵",
                g.kind.map(ListKind::verb).unwrap_or("open"),
                theme.or("accent", Color::Cyan),
            ),
            crate::tui::Pill::new("↑ ↓", "move", theme.or("blue", Color::Blue)),
            crate::tui::Pill::new("esc", "back", theme.or("red", Color::Red)),
        ],
        View::Confirm => vec![
            crate::tui::Pill::new("↵", "open anyway", theme.or("accent", Color::Cyan)),
            crate::tui::Pill::new("esc", "back", theme.or("red", Color::Red)),
        ],
    }
}

/// The rendered width of a pill row: the leading space plus, per pill, its
/// bracketed key cap, its labelled space, and the trailing gap — mirroring the
/// layout in [`crate::tui::pill_row`].
fn bar_width(pills: &[crate::tui::Pill]) -> u16 {
    1 + pills
        .iter()
        .map(|p| (p.key.chars().count() + p.label.chars().count() + 4) as u16)
        .sum::<u16>()
}

fn draw_bar(f: &mut Frame, g: &Git, area: Rect, theme: &Theme) {
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let pills = bar_pills(g, theme);
    let (spans, _) = crate::tui::pill_row(&pills, ink, area.x);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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

    /// The whole screen as text, for the draw assertions.
    fn screen(g: &Git, w: u16, h: u16) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, g))
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
    fn buffer(g: &Git, theme: &Theme, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), theme, Color::Yellow, g))
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
            ("menu", buffer(&g, &theme, 60, 20)),
            ("list", {
                g.show_list(ListKind::PullRequests, rows());
                buffer(&g, &theme, 60, 20)
            }),
        ] {
            for y in 0..20 {
                for x in 0..60 {
                    assert_ne!(buf[(x, y)].bg, fill, "{view} view fills ({x},{y})");
                }
            }
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
        let buf = buffer(&g, &theme, 60, 20);

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
        let buf = buffer(&g, &theme, 60, 20);

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
        let screen = screen(&g, 70, 20);
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
        for k in ['d', 'b', 'h', 'a', 'x', 'r'] {
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
        let runner = MockRunner::new().on(
            "review list",
            r#"[{"slug":"9f6c1b3e09a54e2a","kind":"local","anchor":"main..HEAD",
                 "updated_at":"2026-08-03T09:00:00Z","comment_count":3,"active":false},
                {"slug":"aa11","kind":"pr","anchor":"pr/7",
                 "updated_at":"2026-08-02T09:00:00Z","comment_count":1,"active":true}]"#,
        );
        let rows = load_rows(&runner, "/r", ListKind::Reviews);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "9f6c1b3e09a54e2a");
        assert_eq!(rows[0].label, "main..HEAD");
        assert_eq!(rows[0].meta, "2026-08-03");
        assert_eq!(rows[0].detail, "3 comments");
        // A count of one must not read "1 comments".
        assert_eq!(rows[1].detail, "1 comment");
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
        let s = screen(&g, 80, 24);
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
        let s = screen(&g, 80, 24);
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
        let s = screen(&g, 80, 24);
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
        let s = screen(&g, 80, 24);
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

        let s = screen(&g, 90, 24);
        assert!(s.contains("60/60"), "{s}");
        assert!(s.contains("pull request number 59"), "{s}");
        assert!(!s.contains("pull request number 0 "), "{s}");
    }

    #[test]
    fn wheel_scrolls_a_list_body_but_not_the_menu() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_wheel(1);
        assert_eq!(g.lsel, 0, "the menu has nothing to scroll");
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
        let card_bottom = |g: &Git| -> usize {
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 30)).unwrap();
            term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, g))
                .unwrap();
            let buf = term.backend().buffer().clone();
            (0..30u16)
                .rev()
                .find(|&y| (0..90u16).any(|x| buf[(x, y)].symbol() == "╰"))
                .unwrap() as usize
        };

        let full = card_bottom(&g);
        for ch in "thing 3".chars() {
            g.on_key(key(KeyCode::Char(ch))); // narrow to a handful of matches
        }
        assert!(g.filtered.len() < 40, "the filter should have narrowed");
        assert_eq!(full, card_bottom(&g), "the box resized when filtering");
    }

    #[test]
    fn a_list_places_the_cursor_after_the_query() {
        let mut g = Git::new();
        open_default(&mut g);
        g.show_list(ListKind::PullRequests, rows());
        g.on_key(key(KeyCode::Char('x'))); // query "x"
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, &g))
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
