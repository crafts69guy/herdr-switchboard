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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
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
    /// The history fuzzy filter, and the `commits` indices it currently keeps
    /// (all of them when empty). `hsel` indexes into `filtered`, not `commits`.
    query: String,
    filtered: Vec<usize>,
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
            query: String::new(),
            filtered: Vec::new(),
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
        self.query.clear();
        self.refilter();
        self.chosen = None;
        self.show = true;
    }

    /// Recompute `filtered` from `query`: the `commits` indices whose `sha`,
    /// subject, or author fuzzily match, in chronological order (no re-ranking, so
    /// the log still reads newest-first). Keeps `hsel` in range.
    fn refilter(&mut self) {
        let q = self.query.clone();
        self.filtered = self
            .commits
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                q.is_empty() || fuzzy_match(&q, &format!("{} {} {}", c.sha, c.subject, c.author))
            })
            .map(|(i, _)| i)
            .collect();
        if self.hsel >= self.filtered.len() {
            self.hsel = 0;
        }
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
                self.query.clear();
                self.refilter();
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
        let Some(commit) = self
            .filtered
            .get(self.hsel)
            .and_then(|&i| self.commits.get(i))
        else {
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
            // The history view is a fuzzy picker: printable keys type into the
            // filter, so navigation moves to the arrows (and readline `^n`/`^p`).
            View::History => {
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
                    KeyCode::Down => self.hstep(1),
                    KeyCode::Up => self.hstep(-1),
                    KeyCode::Char('n') if ctrl => self.hstep(1),
                    KeyCode::Char('p') if ctrl => self.hstep(-1),
                    KeyCode::Home => self.hsel = 0,
                    KeyCode::End => self.hsel = self.filtered.len().saturating_sub(1),
                    KeyCode::Enter => return self.activate_commit(),
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
        false
    }

    fn step(&mut self, d: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        self.sel = ((self.sel as i32 + d).rem_euclid(n as i32)) as usize;
    }

    /// Wheel over the overlay: scroll the history body by moving the selection
    /// (which pages the list). A no-op in the menu, which has nothing to scroll.
    pub fn on_wheel(&mut self, d: i32) {
        if matches!(self.view, View::History) {
            self.hstep(d);
        }
    }

    fn hstep(&mut self, d: i32) {
        let n = self.filtered.len();
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

    // The history browser is a fixed-size box with its own internal layout.
    if matches!(g.view, View::History) {
        draw_history(f, area, theme, title, g);
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(ink))
        .title(Span::styled(
            format!(" 󰊢 Git · {} ", g.label),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
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

/// The history log browser, drawn as a **fixed-size** card so filtering never
/// resizes it: a pinned detail header on top, a rounded search box just above the
/// body, and — the only part that scrolls — the paged commit list, then the bar.
fn draw_history(f: &mut Frame, area: Rect, theme: &Theme, title: Color, g: &Git) {
    use ratatui::layout::Constraint::{Length, Min};

    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    let border = theme.or("accent", Color::Cyan);

    // Size from the terminal, not the content, so the box is stable across searches.
    let w = (HIST_W as u16 + 6).min(area.width.saturating_sub(2));
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

    let tail = if g.commits.is_empty() {
        " · history ".to_string()
    } else if g.filtered.is_empty() {
        " · history · no matches ".to_string()
    } else {
        format!(" · history · {}/{} ", g.hsel + 1, g.filtered.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(ink))
        .title(Span::styled(
            format!(" 󰊢 Git · {} ", g.label),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
        .title(Line::from(Span::styled(tail, Style::default().fg(sub))).right_aligned());
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // A repo with no commits: nothing to browse.
    if g.commits.is_empty() {
        let rows = ratatui::layout::Layout::vertical([Min(1), Length(1)]).split(inner);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  (no commits)",
                Style::default().fg(sub),
            ))),
            rows[0],
        );
        draw_bar(f, g, rows[1], theme);
        return;
    }

    // Header (fixed) · search box (rounded, near the body) · scrolling list · bar.
    let a =
        ratatui::layout::Layout::vertical([Length(HIST_HEADER_ROWS), Length(3), Min(1), Length(1)])
            .split(inner);
    let (header_area, search_area, body_area, bar_area) = (a[0], a[1], a[2], a[3]);

    f.render_widget(
        Paragraph::new(history_header_lines(
            g,
            text,
            sub,
            title,
            header_area.width as usize,
        )),
        header_area,
    );

    // The search input, its own rounded box.
    let sbox = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(sub))
        .style(Style::default().bg(ink));
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

    // Only this scrolls: the page of commits holding the selection.
    let viewport = body_area.height as usize;
    f.render_widget(
        Paragraph::new(history_body_lines(
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

/// Target content width of the fixed history card, driving the card's width and
/// the subject wrap.
const HIST_W: usize = 68;

/// The pinned detail header's height: one meta row plus up to two subject rows.
const HIST_HEADER_ROWS: u16 = 3;

/// The relative-date column in a history list row, padded so subjects line up.
const DATE_COL: usize = 14;

/// The leading run of the history search line (a leading space, the filter icon,
/// a space) — shared so `draw_history` can place the real terminal cursor exactly
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

/// The pinned detail header for the highlighted commit — its sha, relative date,
/// author, then the (wrapped, two-row-capped) subject. Fixed height, so it never
/// shrinks while filtering; a clipped list row is never the only place a subject
/// appears. Falls back to a placeholder when the filter matches nothing.
fn history_header_lines(
    g: &Git,
    text: Color,
    sub: Color,
    title: Color,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(c) = g.filtered.get(g.hsel).and_then(|&i| g.commits.get(i)) else {
        return vec![Line::from(Span::styled(
            "  no matches",
            Style::default().fg(sub),
        ))];
    };
    let mut meta = vec![
        Span::raw(" "),
        Span::styled(
            c.sha.clone(),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ),
    ];
    if !c.date.is_empty() {
        meta.push(Span::styled(
            format!("  {}", c.date),
            Style::default().fg(sub),
        ));
    }
    if !c.author.is_empty() {
        meta.push(Span::styled(
            format!("  ·  {}", c.author),
            Style::default().fg(sub),
        ));
    }
    let mut lines = vec![Line::from(meta)];
    for (prefix, line) in crate::markdown::wrap(&c.subject, width.saturating_sub(1), "", "")
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

/// The scrolling body: the page of the filtered commit list holding the
/// selection, each row a marker, sha, date column, and clipped subject. This is
/// the only part of the card that moves; `viewport` is the body area's height.
fn history_body_lines(
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
        (g.hsel / viewport) * viewport
    };
    let end = (start + viewport).min(len);
    g.filtered
        .iter()
        .enumerate()
        .take(end)
        .skip(start)
        .filter_map(|(rank, &i)| {
            let c = g.commits.get(i)?;
            let selected = rank == g.hsel;
            // marker(2) + sha + space + date column + space, then the subject fills the rest.
            let room = width.saturating_sub(5 + c.sha.chars().count() + DATE_COL);
            let date = clip(&c.date, DATE_COL);
            Some(Line::from(vec![
                Span::styled(
                    if selected { "▌ " } else { "  " },
                    Style::default().fg(accent),
                ),
                Span::styled(
                    format!("{} ", c.sha),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{date:<DATE_COL$} "), Style::default().fg(sub)),
                Span::styled(
                    clip(&c.subject, room),
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
        // ...move to the second commit (↓, since letters now type into the filter) and pick it.
        g.on_key(key(KeyCode::Down));
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

    #[test]
    fn history_browser_scrolls_and_shows_a_position_counter() {
        // More commits than fit; the list must page rather than overflow.
        let commits: Vec<Commit> = (0..60)
            .map(|i| Commit {
                sha: format!("sha{i:04}"),
                date: "1 hour ago".into(),
                author: "Ada".into(),
                subject: format!("commit number {i}"),
            })
            .collect();
        let mut g = Git::new();
        g.open("/repo".into(), "repo".into(), None, commits, false, vec![]);
        g.on_key(key(KeyCode::Char('h'))); // enter the history browser
        g.on_key(key(KeyCode::End)); // jump to the last commit

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, &g))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = (0..24)
            .map(|y| {
                (0..90)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The counter reflects the End jump, the last commit's page is on screen,
        // and an early commit has scrolled off.
        assert!(screen.contains("60/60"), "{screen}");
        assert!(screen.contains("commit number 59"), "{screen}");
        assert!(!screen.contains("commit number 0 "), "{screen}");
    }

    #[test]
    fn wheel_scrolls_the_history_body_but_not_the_menu() {
        let mut g = Git::new();
        open_default(&mut g); // two commits
        g.on_wheel(1);
        assert_eq!(g.hsel, 0, "the menu has nothing to scroll");
        g.on_key(key(KeyCode::Char('h'))); // enter history
        g.on_wheel(1);
        assert_eq!(g.hsel, 1, "wheel-down moves the history selection");
        g.on_wheel(-1);
        assert_eq!(g.hsel, 0, "wheel-up moves it back");
    }

    #[test]
    fn history_box_stays_fixed_size_while_filtering() {
        let commits: Vec<Commit> = (0..40)
            .map(|i| Commit {
                sha: format!("s{i:03}"),
                date: "1h".into(),
                author: "Ada".into(),
                subject: format!("thing {i}"),
            })
            .collect();
        let mut g = Git::new();
        g.open("/repo".into(), "repo".into(), None, commits, false, vec![]);
        g.on_key(key(KeyCode::Char('h')));

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
        assert!(
            g.filtered.len() < 40,
            "the filter should have narrowed the list"
        );
        assert_eq!(full, card_bottom(&g), "the box resized when filtering");
    }

    #[test]
    fn history_places_the_cursor_after_the_query() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('h'))); // history
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
    fn typing_filters_the_history_and_enter_picks_a_match() {
        let mut g = Git::new();
        g.open(
            "/repo".into(),
            "repo".into(),
            None,
            vec![
                Commit {
                    sha: "aaa1111".into(),
                    date: "1h".into(),
                    author: "Ada".into(),
                    subject: "add login".into(),
                },
                Commit {
                    sha: "bbb2222".into(),
                    date: "2h".into(),
                    author: "Bea".into(),
                    subject: "fix logout".into(),
                },
                Commit {
                    sha: "ccc3333".into(),
                    date: "3h".into(),
                    author: "Cy".into(),
                    subject: "refactor cache".into(),
                },
            ],
            false,
            vec![],
        );
        g.on_key(key(KeyCode::Char('h'))); // enter history
        for ch in "fix".chars() {
            g.on_key(key(KeyCode::Char(ch)));
        }
        // Only the "fix logout" commit is a subsequence match.
        assert_eq!(g.filtered.len(), 1);
        assert!(g.on_key(key(KeyCode::Enter)));
        assert_eq!(g.chosen.unwrap().arg, "bbb2222");
    }

    #[test]
    fn esc_clears_the_filter_before_leaving_history() {
        let mut g = Git::new();
        open_default(&mut g);
        g.on_key(key(KeyCode::Char('h')));
        g.on_key(key(KeyCode::Char('f'))); // filter query "f"
        assert_eq!(g.query, "f");
        g.on_key(key(KeyCode::Esc)); // first esc clears the query...
        assert!(g.query.is_empty());
        assert!(g.show, "clearing the filter must not close the overlay");
        g.on_key(key(KeyCode::Esc)); // ...then history → menu...
        g.on_key(key(KeyCode::Esc)); // ...then menu closes.
        assert!(!g.show);
    }
}
