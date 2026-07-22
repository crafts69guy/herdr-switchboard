//! The git overlay: a floating menu — the review verbs the retired `git-hub`
//! plugin used to serve behind `prefix+g` — folded into the switcher itself.
//!
//! Like the `⌥c` changelog and the `⌥,` settings form, it lives **inside** the
//! picker: `^g` (Insert) or `␣g` (Normal) opens a centred, rounded, ink-filled
//! card over the list rather than a separate herdr pane. Selecting a row resolves
//! a concrete command — which repo, which base branch, which commit — and hands it
//! to `bin/review.sh`, which `exec`s the right tool in the overlay pane the way the
//! clone flow `exec`s `get.sh`: **`hunk`** for read-only review, `lazygit` for
//! staging, `$EDITOR` for conflict resolution.
//!
//! Resolution that needs to shell out (base-branch detection, the commit list) runs
//! **when the overlay opens**, through the [`CommandRunner`] seam, so `on_key` stays
//! pure IO-free navigation and the whole surface is unit-testable.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::data::Theme;
use crate::runner::CommandRunner;

/// The resolved review command, passed to `bin/review.sh` as environment. `mode`
/// picks the tool + shape; `arg` carries the one variable piece (a base ref for
/// `branch`, a sha for `history`); `custom` is the shell command for a `menu.conf`
/// entry. Everything is a plain string so the launcher stays a thin `case`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewSpec {
    pub mode: String,
    pub cwd: String,
    pub arg: String,
    pub custom: String,
    pub label: String,
}

/// A commit in the `history` sub-list: a short sha, a relative date and author
/// (shown in the detail header), and its subject line.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub date: String,
    pub author: String,
    pub subject: String,
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
    /// A `review.sh` mode with no extra argument resolved here: worktree, staged,
    /// conflicts, lazygit.
    Review(&'static str),
    /// Review a branch against `base` (resolved at open); empty base still opens,
    /// `review.sh` falls back to a plain diff.
    Branch,
    /// Open the commit sub-list rather than dispatching.
    History,
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
#[derive(PartialEq)]
enum View {
    Menu,
    History,
}

/// The git overlay, embedded in the picker as `App::git` and drawn over it when
/// `show`. `chosen` is the resolved command a successful activation leaves behind;
/// the picker reads it after the loop and `exec`s `review.sh` with it.
pub struct Git {
    pub show: bool,
    /// The repo the verbs act on: the selected entry's dir, else the origin cwd.
    cwd: String,
    /// Short label for the card title and the resolved spec.
    label: String,
    /// Detected base branch for the `branch` review, `None` when none resolves.
    base: Option<String>,
    commits: Vec<Commit>,
    items: Vec<Item>,
    view: View,
    sel: usize,
    hsel: usize,
    /// Set by a successful activation; taken by the picker to dispatch.
    pub chosen: Option<ReviewSpec>,
}

impl Git {
    pub fn new() -> Self {
        Git {
            show: false,
            cwd: String::new(),
            label: String::new(),
            base: None,
            commits: Vec::new(),
            items: Vec::new(),
            view: View::Menu,
            sel: 0,
            hsel: 0,
            chosen: None,
        }
    }

    /// Build the menu for `cwd` and open it. `base`/`commits` are resolved by the
    /// caller through the runner so this stays IO-free; `has_lazygit` hides the
    /// staging row when lazygit is not installed (as the fzf menu did); `customs`
    /// are the `menu.conf` rows, appended after the built-ins.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        cwd: String,
        label: String,
        base: Option<String>,
        commits: Vec<Commit>,
        has_lazygit: bool,
        customs: Vec<Custom>,
    ) {
        // Nerd Font (nf-md) glyphs, one per row so every label lines up: worktree
        // is unstaged edits (pencil), staged is the ready index (check), branch is
        // a diff against the base (source-branch), history the commit list (clock),
        // conflicts the merge (alert), lazygit the git surface (git logo).
        let mut items = vec![
            Item {
                key: 'd',
                icon: "󰏫".into(),
                label: "review worktree".into(),
                act: Act::Review("worktree"),
            },
            Item {
                key: 's',
                icon: "󰄬".into(),
                label: "review staged".into(),
                act: Act::Review("staged"),
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
                label: "review history".into(),
                act: Act::History,
            },
            Item {
                key: 'x',
                icon: "󰞇".into(),
                label: "resolve conflicts".into(),
                act: Act::Review("conflicts"),
            },
        ];
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
        self.commits = commits;
        self.items = items;
        self.view = View::Menu;
        self.sel = 0;
        self.hsel = 0;
        self.chosen = None;
        self.show = true;
    }

    /// Resolve the selected menu row into a [`ReviewSpec`], or open the history list.
    /// Returns `true` when it produced a `chosen` command and closed the overlay.
    fn activate(&mut self) -> bool {
        let Some(item) = self.items.get(self.sel) else {
            return false;
        };
        let (mode, arg, custom) = match &item.act {
            Act::Review(m) => (m.to_string(), String::new(), String::new()),
            Act::Branch => (
                "branch".to_string(),
                self.base.clone().unwrap_or_default(),
                String::new(),
            ),
            Act::Custom(cmd) => ("custom".to_string(), String::new(), cmd.clone()),
            Act::History => {
                self.view = View::History;
                self.hsel = 0;
                return false;
            }
        };
        self.chosen = Some(ReviewSpec {
            mode,
            cwd: self.cwd.clone(),
            arg,
            custom,
            label: self.label.clone(),
        });
        self.show = false;
        true
    }

    /// Dispatch the selected commit as a `history` review.
    fn activate_commit(&mut self) -> bool {
        let Some(commit) = self.commits.get(self.hsel) else {
            return false;
        };
        self.chosen = Some(ReviewSpec {
            mode: "history".into(),
            cwd: self.cwd.clone(),
            arg: commit.sha.clone(),
            custom: String::new(),
            label: format!("{} · {}", self.label, commit.sha),
        });
        self.show = false;
        true
    }

    /// Handle a key while the overlay is open. Returns `true` once a selection has
    /// resolved a command (`chosen` is set and the overlay closed), so the picker
    /// breaks its loop and dispatches. `esc`/`q` step back a view, then close; the
    /// caller keeps `^c` as the picker's quit.
    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        match self.view {
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
            View::History => match k.code {
                // esc backs out to the menu rather than closing the whole overlay.
                KeyCode::Esc | KeyCode::Char('q') => self.view = View::Menu,
                KeyCode::Down | KeyCode::Char('j') => self.hstep(1),
                KeyCode::Up | KeyCode::Char('k') => self.hstep(-1),
                KeyCode::Home | KeyCode::Char('g') => self.hsel = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    self.hsel = self.commits.len().saturating_sub(1)
                }
                KeyCode::Enter => return self.activate_commit(),
                _ => {}
            },
        }
        false
    }

    fn step(&mut self, d: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        self.sel = ((self.sel as i32 + d).rem_euclid(n as i32)) as usize;
    }

    fn hstep(&mut self, d: i32) {
        let n = self.commits.len();
        if n == 0 {
            return;
        }
        self.hsel = ((self.hsel as i32 + d).rem_euclid(n as i32)) as usize;
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
    let resolves =
        |r: &str| runner.capture("git", &["-C", cwd, "rev-parse", "--verify", "--quiet", r]).is_some();
    if !configured.is_empty() && resolves(configured) {
        return Some(configured.to_string());
    }
    ["main", "master", "origin/main", "origin/master"]
        .into_iter()
        .find(|r| resolves(r))
        .map(str::to_string)
}

/// The recent commits for the history sub-list. A `--pretty` format with a US
/// (`\x1f`) field separator carries the short sha, relative date, author, and
/// subject in one line each — the detail header shows the first three, the list
/// the sha and subject. Empty on any failure (not a repo, no commits) — the
/// sub-list just shows nothing.
pub fn load_commits(runner: &dyn CommandRunner, cwd: &str, n: usize) -> Vec<Commit> {
    let n = n.to_string();
    let out = runner.capture(
        "git",
        &[
            "-C",
            cwd,
            "log",
            "--no-decorate",
            "-n",
            &n,
            "--pretty=format:%h%x1f%cr%x1f%an%x1f%s",
        ],
    );
    out.into_iter()
        .flat_map(|s| s.lines().map(str::to_string).collect::<Vec<_>>())
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            let sha = fields.next()?.to_string();
            if sha.is_empty() {
                return None;
            }
            Some(Commit {
                sha,
                date: fields.next().unwrap_or_default().to_string(),
                author: fields.next().unwrap_or_default().to_string(),
                subject: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
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

/// Draw the git card centred over the picker, matching the settings/changelog shape.
pub fn draw(f: &mut Frame, area: Rect, theme: &Theme, title: Color, g: &Git) {
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    let border = theme.or("accent", Color::Cyan);

    let (lines, title_tail) = match g.view {
        View::Menu => (menu_lines(g, ink, text, sub, accent, title), " · git "),
        View::History => (history_lines(g, text, sub, accent, title), " · history "),
    };

    // Width: the wider of the content and the command bar, clamped; height: the
    // rows plus the border and a one-row command bar. The bar must be measured
    // too — a narrow menu would otherwise size the card below the pill row and
    // clip `esc close` to `esc clo`.
    let pills = bar_pills(g, theme);
    let content_w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(24);
    let want_w = content_w.max(bar_width(&pills)).clamp(24, 60) + 4;
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(ink))
        .title(Span::styled(
            format!(" 󰊢 Git · {} ", g.label),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
        .title(Line::from(Span::styled(title_tail, Style::default().fg(sub))).right_aligned());
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

/// Width the history detail wraps and the list clips to. Fixed so the wrap and
/// the separator rule can be built before the card's final width is known — the
/// menu sizes to its widest row, but history reads better at a steady width.
const HIST_W: usize = 52;

/// Truncate to `w` chars with a trailing ellipsis; short strings pass through.
fn clip(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The history view: a detail header for the highlighted commit — its sha,
/// relative date, author, and full (wrapped) subject — then a rule, then the
/// commit list. The header means a clipped list row is never the only place a
/// subject appears, so the user always sees what `↵ show diff` will open.
fn history_lines(g: &Git, text: Color, sub: Color, accent: Color, title: Color) -> Vec<Line<'static>> {
    if g.commits.is_empty() {
        return vec![Line::from(Span::styled(
            "  no commits",
            Style::default().fg(sub),
        ))];
    }

    let mut lines = Vec::new();
    if let Some(c) = g.commits.get(g.hsel) {
        let mut meta = vec![
            Span::raw(" "),
            Span::styled(
                c.sha.clone(),
                Style::default().fg(title).add_modifier(Modifier::BOLD),
            ),
        ];
        if !c.date.is_empty() {
            meta.push(Span::styled(format!("  {}", c.date), Style::default().fg(sub)));
        }
        if !c.author.is_empty() {
            meta.push(Span::styled(
                format!("  ·  {}", c.author),
                Style::default().fg(sub),
            ));
        }
        lines.push(Line::from(meta));
        for (prefix, line) in crate::markdown::wrap(&c.subject, HIST_W, "", "") {
            lines.push(Line::from(Span::styled(
                format!(" {prefix}{line}"),
                Style::default().fg(text).add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(Span::styled(
            "─".repeat(HIST_W),
            Style::default().fg(sub),
        )));
    }

    for (i, c) in g.commits.iter().enumerate() {
        let selected = i == g.hsel;
        let room = HIST_W.saturating_sub(c.sha.chars().count() + 3);
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▌ " } else { "  " },
                Style::default().fg(accent),
            ),
            Span::styled(
                format!("{} ", c.sha),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                clip(&c.subject, room),
                Style::default().fg(if selected { text } else { sub }),
            ),
        ]));
    }
    lines
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
        View::History => vec![
            crate::tui::Pill::new("↵", "show diff", theme.or("accent", Color::Cyan)),
            crate::tui::Pill::new("↑ ↓", "move", theme.or("blue", Color::Blue)),
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
    use crate::data::Config;
    use crate::runner::MockRunner;
    use crossterm::event::KeyModifiers;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn open_default(g: &mut Git) {
        g.open(
            "/repo".into(),
            "repo".into(),
            Some("main".into()),
            vec![
                Commit {
                    sha: "aaa1111".into(),
                    date: "2 hours ago".into(),
                    author: "Ada".into(),
                    subject: "first".into(),
                },
                Commit {
                    sha: "bbb2222".into(),
                    date: "3 hours ago".into(),
                    author: "Ada".into(),
                    subject: "second".into(),
                },
            ],
            true,
            vec![],
        );
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
    fn commits_parse_into_sha_date_author_and_subject() {
        // The `\x1f`-delimited pretty format, one commit per line.
        let runner = MockRunner::new().on(
            "log",
            "abc1234\u{1f}2 hours ago\u{1f}Ada\u{1f}fix the thing\n\
             def5678\u{1f}3 days ago\u{1f}Bea\u{1f}add a test\n",
        );
        let commits = load_commits(&runner, "/r", 50);
        assert_eq!(
            commits,
            vec![
                Commit {
                    sha: "abc1234".into(),
                    date: "2 hours ago".into(),
                    author: "Ada".into(),
                    subject: "fix the thing".into()
                },
                Commit {
                    sha: "def5678".into(),
                    date: "3 days ago".into(),
                    author: "Bea".into(),
                    subject: "add a test".into()
                },
            ]
        );
    }

    #[test]
    fn menu_conf_skips_comments_blanks_and_incomplete_rows() {
        let conf = "\
# a comment

p|󰊢|push|git push
bad line with no pipes
k||no command|
r|󰑓|pull|git pull
";
        let rows = parse_menu_conf(conf);
        assert_eq!(
            rows,
            vec![
                Custom {
                    key: 'p',
                    icon: "󰊢".into(),
                    label: "push".into(),
                    cmd: "git push".into()
                },
                Custom {
                    key: 'r',
                    icon: "󰑓".into(),
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
        assert!(g.on_key(key(KeyCode::Enter)));
        assert!(!g.show);
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "worktree");
        assert_eq!(spec.cwd, "/repo");
    }

    #[test]
    fn branch_row_carries_the_detected_base() {
        let mut g = Git::new();
        open_default(&mut g);
        // d, s, then b (branch).
        g.on_key(key(KeyCode::Char('b')));
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "branch");
        assert_eq!(spec.arg, "main");
    }

    #[test]
    fn history_opens_a_sublist_then_shows_the_chosen_commit() {
        let mut g = Git::new();
        open_default(&mut g);
        // h opens history (no dispatch yet)...
        assert!(!g.on_key(key(KeyCode::Char('h'))));
        assert!(g.show, "history must stay in the overlay");
        // ...move to the second commit and pick it.
        g.on_key(key(KeyCode::Char('j')));
        assert!(g.on_key(key(KeyCode::Enter)));
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "history");
        assert_eq!(spec.arg, "bbb2222");
    }

    #[test]
    fn esc_in_history_steps_back_to_the_menu_not_out() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('h')));
        g.on_key(key(KeyCode::Esc));
        assert!(
            g.show,
            "the first esc backs out of history, not the overlay"
        );
        g.on_key(key(KeyCode::Esc));
        assert!(!g.show, "a second esc closes the overlay");
    }

    #[test]
    fn a_custom_mnemonic_dispatches_its_command() {
        let mut g = Git::new();
        g.open(
            "/repo".into(),
            "repo".into(),
            None,
            vec![],
            false,
            vec![Custom {
                key: 'z',
                icon: "".into(),
                label: "prune".into(),
                cmd: "git gc".into(),
            }],
        );
        assert!(g.on_key(key(KeyCode::Char('z'))));
        let spec = g.chosen.unwrap();
        assert_eq!(spec.mode, "custom");
        assert_eq!(spec.custom, "git gc");
    }

    #[test]
    fn lazygit_row_is_hidden_without_lazygit() {
        let mut g = Git::new();
        g.open("/repo".into(), "repo".into(), None, vec![], false, vec![]);
        assert!(g.items.iter().all(|i| i.key != 'l'));
    }

    #[test]
    fn draw_renders_the_card_over_the_picker() {
        let mut g = Git::new();
        open_default(&mut g);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, &g))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Git"), "{screen}");
        assert!(screen.contains("review worktree"), "{screen}");
        assert!(screen.contains('╭'), "{screen}");
        // The card must be wide enough for the whole command bar — a narrow menu
        // once clipped `esc close` to `esc clo`.
        assert!(screen.contains("close"), "{screen}");
        let _ = Config::default();
    }

    #[test]
    fn history_view_shows_a_detail_header_and_show_diff_action() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('h'))); // open the history sub-list
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, &g))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The detail header carries the selected commit's date and author, and the
        // footer spells out what Enter does.
        assert!(screen.contains("2 hours ago"), "{screen}");
        assert!(screen.contains("Ada"), "{screen}");
        assert!(screen.contains("show diff"), "{screen}");
    }
}
