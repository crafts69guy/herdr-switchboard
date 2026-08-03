//! Preview for the highlighted entry, drawn as a card: a header row carrying
//! the name and its state as a pill, a meta column, then bodies under captioned
//! rules.
//!
//! Agents and workspaces are read from herdr's JSON here rather than in
//! `preview.sh`. Two reasons: every colour has to come from [`Theme`] for the
//! card to match the list and the command bar, and `serde_json` gets herdr's
//! envelope right where hand-written jq filters silently did not — herdr nests
//! the record under `result.agent` / `result.workspace`, and reading
//! `result.agent_status` instead yields no error, just "unknown". `preview.sh`
//! keeps only the file tree, the one part that arrives as ANSI already.
//!
//! The workspace card is a dashboard rather than a copy of `workspace get`,
//! which counts panes but names none: it reads `pane list`, narrows it to the
//! workspace, and renders the running agents (name, status, current task) and
//! the distinct repositories their panes sit in (branch + dirty, the same git
//! read `repo_card` makes).
//!
//! `render` shells out and costs ~100ms on a large repo — mostly `git status`
//! — so it runs on a [`Worker`] thread rather than between a keypress and the
//! next frame.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ansi_to_tui::IntoText;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use serde_json::Value;

use crate::data::{state_color, Config, Entry, Kind, Theme};
use crate::runner::{CommandRunner, SystemRunner};

/// A render request. `seq` lets the UI drop results it has already scrolled
/// past; `width` is the pane's inner width at the last draw, so bodies can be
/// clipped to it rather than wrapped.
struct Job {
    seq: u64,
    entry: Entry,
    width: u16,
}

/// A finished preview, tagged with the `seq` of the job that produced it.
pub struct Done {
    pub seq: u64,
    pub text: Text<'static>,
}

/// Renders previews off the UI thread, newest request first.
pub struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

impl Worker {
    pub fn spawn(script_dir: String, cfg: Config, theme: Theme) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        thread::spawn(move || {
            // The live worker always drives the real programs; the runner seam
            // exists so `render` can be unit-tested with a mock, not so this
            // thread can be reconfigured.
            let runner = SystemRunner;
            while let Ok(mut job) = job_rx.recv() {
                // Skip ahead to the newest request: while the user scrolls, only
                // the entry they land on is worth the subprocess.
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                let started = std::time::Instant::now();
                let text = render(&job.entry, &runner, &script_dir, &cfg, &theme, job.width);
                if crate::trace::enabled() {
                    // Measured around `render` so the number includes the
                    // `preview.sh` shell-out, which is the part under suspicion.
                    crate::trace::span_with("preview.render", started, &job.entry.label);
                }
                if done_tx.send(Done { seq: job.seq, text }).is_err() {
                    break; // the UI is gone
                }
            }
        });
        Self {
            jobs: job_tx,
            done: done_rx,
        }
    }

    /// Queues a render. Returns false if the worker thread is gone.
    pub fn request(&self, seq: u64, entry: Entry, width: u16) -> bool {
        self.jobs.send(Job { seq, entry, width }).is_ok()
    }

    /// Non-blocking: the next finished preview, if one has landed.
    pub fn poll(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }
}

pub fn render(
    entry: &Entry,
    runner: &dyn CommandRunner,
    script_dir: &str,
    cfg: &Config,
    theme: &Theme,
    width: u16,
) -> Text<'static> {
    let p = Ink::new(theme, cfg);
    // A pane this narrow is unusable anyway; the floor just keeps the clip and
    // rule arithmetic below out of saturating-to-zero territory.
    let width = width.max(24);
    let lines = match entry.kind {
        Kind::Agent => agent_card(entry, runner, width, &p, theme),
        Kind::Workspace => workspace_card(entry, runner, width, &p, theme),
        Kind::Repo | Kind::Worktree => repo_card(entry, runner, script_dir, cfg, width, &p, theme),
    };
    Text::from(lines)
}

// --- card primitives -------------------------------------------------------

/// The preview's slice of the theme, resolved once per render.
struct Ink {
    /// The panel background, used as the *text* colour on a filled pill.
    ink: Color,
    text: Color,
    sub: Color,
    overlay: Color,
    accent: Color,
    /// The same colour the pane titles use, so a README heading in the card
    /// reads as a heading of the same rank.
    title: Color,
}

impl Ink {
    fn new(t: &Theme, cfg: &Config) -> Self {
        Ink {
            ink: t.or("panel_bg", Color::Rgb(16, 18, 20)),
            text: t.or("text", Color::Reset),
            sub: t.or("subtext0", Color::DarkGray),
            overlay: t.or("overlay0", Color::DarkGray),
            accent: t.or("accent", Color::Cyan),
            // Resolved the way `App::new` resolves it, from the same setting.
            title: t
                .resolve(&cfg.get("title_color", "peach"))
                .unwrap_or_else(|| t.or("accent", Color::Cyan)),
        }
    }
}

/// A filled pill — the shape the command bar and the help popup already use, so
/// a state here reads as the same kind of object as a key there.
fn pill(label: &str, bg: Color, p: &Ink) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .bg(bg)
            .fg(p.ink)
            .add_modifier(Modifier::BOLD),
    )
}

/// Icon, name, then any pills, on one row.
fn header(
    icon: &str,
    icon_color: Color,
    name: &str,
    pills: Vec<Span<'static>>,
    p: &Ink,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            name.to_string(),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ];
    for pill in pills {
        spans.push(Span::raw(" "));
        spans.push(pill);
    }
    Line::from(spans)
}

/// Width of the label column, so every value in a card starts at one column.
const META_LABEL: usize = 8;

/// One `label   value` row.
fn meta(label: &str, value: &str, width: u16, p: &Ink) -> Line<'static> {
    let room = (width as usize).saturating_sub(META_LABEL + 3);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{label:<META_LABEL$}"), Style::default().fg(p.sub)),
        Span::styled(clip(value, room), Style::default().fg(p.text)),
    ])
}

/// A captioned rule: `── caption ──────────`. Separates a card's sections
/// without spending a whole row on a heading.
fn rule(caption: &str, width: u16, p: &Ink) -> Line<'static> {
    let used = 2 + caption.chars().count() + 2;
    let tail = (width as usize).saturating_sub(used + 1);
    Line::from(vec![
        Span::styled("──".to_string(), Style::default().fg(p.overlay)),
        Span::styled(format!(" {caption} "), Style::default().fg(p.sub)),
        Span::styled("─".repeat(tail), Style::default().fg(p.overlay)),
    ])
}

/// A dim aside — the "nothing here" line every body falls back to.
fn note(s: &str, p: &Ink) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(s.to_string(), Style::default().fg(p.sub)),
    ])
}

/// Clip an already-styled line to `width`, keeping each span's colour. Content
/// that arrives styled — the agent's ANSI output, eza's tree — cannot go through
/// [`clip`], which would count the escapes as text and cut them mid-sequence.
///
/// Every body clips rather than wraps, which is also what lets the pane scroll:
/// one line of content is one row on screen, so the scroll offset means what it
/// says.
fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    let mut used = 0usize;
    let mut out: Vec<Span<'static>> = Vec::new();
    for span in line.spans {
        let n = span.content.chars().count();
        if used + n <= width {
            used += n;
            out.push(span);
            continue;
        }
        // This span crosses the edge: keep what fits of it and stop.
        let room = width.saturating_sub(used);
        if room > 0 {
            let mut s: String = span.content.chars().take(room.saturating_sub(1)).collect();
            s.push('…');
            out.push(Span::styled(s, span.style));
        }
        break;
    }
    Line::from(out)
}

/// Is this line blank once its styling is set aside?
fn is_blank(line: &Line) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Truncate to `width` with an ellipsis. Counts chars rather than display
/// columns: the cost of being wrong on a CJK path is a column of padding, and
/// the alternative is a unicode-width dependency for that.
fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `$HOME/x` → `~/x`, so a path still fits the value column.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// Run a herdr subcommand and parse its JSON envelope. Every failure — herdr
/// missing, a non-zero exit, unparseable output — becomes `Value::Null`, which
/// the readers below see as "field absent" and fall back on. A preview must
/// never be the thing that fails loudly.
fn herdr_json(runner: &dyn CommandRunner, args: &[&str]) -> Value {
    let Ok(out) = runner.output("herdr", args) else {
        return Value::Null;
    };
    if !out.status.success() {
        return Value::Null;
    }
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

// --- agent -----------------------------------------------------------------

fn agent_card(
    entry: &Entry,
    runner: &dyn CommandRunner,
    width: u16,
    p: &Ink,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let v = herdr_json(runner, &["agent", "get", &entry.id]);
    let a = &v["result"]["agent"];
    let name = a["agent"].as_str().unwrap_or("agent");
    let status = a["agent_status"].as_str().unwrap_or("unknown");
    let cwd = a["foreground_cwd"]
        .as_str()
        .or_else(|| a["cwd"].as_str())
        .unwrap_or_else(|| entry.dir.as_deref().unwrap_or(""));

    let state = state_color(theme, status);
    let mut lines = vec![
        header(&entry.icon, state, name, vec![pill(status, state, p)], p),
        Line::raw(""),
    ];
    // The terminal title is the agent's own summary of what it is doing — the
    // one field here worth more than the ids around it, so it leads.
    if let Some(title) = a["terminal_title_stripped"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        lines.push(meta("doing", title, width, p));
    }
    if !cwd.is_empty() {
        lines.push(meta("cwd", &tilde(cwd), width, p));
    }
    if let Some(pane) = a["pane_id"].as_str() {
        lines.push(meta("pane", pane, width, p));
    }
    lines.push(Line::raw(""));
    lines.push(rule("recent output", width, p));
    lines.push(Line::raw(""));
    lines.extend(agent_output(runner, &entry.id, width, p));
    lines
}

/// The agent's recent pane text, in the agent's own colours.
///
/// `--format ansi` hands back the escape sequences from the agent's screen, so
/// the body reads the way the agent actually looks rather than as flat text.
/// The rows arrive at the *agent's* pane width, far wider than this preview, so
/// each is clipped rather than wrapped — wrapping is what turned this body into
/// a wall of fragments.
fn agent_output(runner: &dyn CommandRunner, id: &str, width: u16, p: &Ink) -> Vec<Line<'static>> {
    let v = herdr_json(
        runner,
        &[
            "agent", "read", id, "--source", "recent", "--format", "ansi", "--lines", "60",
        ],
    );
    let Some(text) = v["result"]["read"]["text"].as_str() else {
        return vec![note("(no output available)", p)];
    };

    // A pane's rows end in a carriage return; left in, it renders as a stray
    // glyph and defeats the blank-row test below.
    let cleaned = text.replace('\r', "");
    let mut rows: Vec<Line<'static>> = match cleaned.into_text() {
        Ok(t) => t.lines,
        // Unparseable escapes: show the text rather than nothing.
        Err(_) => cleaned.lines().map(|l| Line::raw(l.to_string())).collect(),
    };

    // A terminal pane is mostly padding. Drop the blank rows at both ends and
    // collapse the runs between, so what survives is the output worth reading
    // rather than the empty half of somebody's screen.
    let first = rows.iter().position(|l| !is_blank(l));
    let last = rows.iter().rposition(|l| !is_blank(l));
    let (Some(first), Some(last)) = (first, last) else {
        return vec![note("(no output yet)", p)];
    };
    rows.truncate(last + 1);
    let rows = rows.split_off(first);

    let mut out = Vec::new();
    let mut prev_blank = false;
    for row in rows {
        let blank = is_blank(&row);
        if blank && prev_blank {
            continue;
        }
        prev_blank = blank;
        out.push(clip_line(row, width as usize));
    }
    out
}

// --- workspace -------------------------------------------------------------

fn workspace_card(
    entry: &Entry,
    runner: &dyn CommandRunner,
    width: u16,
    p: &Ink,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let v = herdr_json(runner, &["workspace", "get", &entry.id]);
    let w = &v["result"]["workspace"];
    let label = w["label"].as_str().unwrap_or(&entry.label);
    let status = w["agent_status"].as_str().unwrap_or("unknown");

    let mut pills = vec![pill(status, state_color(theme, status), p)];
    if w["focused"].as_bool().unwrap_or(false) {
        pills.push(pill("current", p.accent, p));
    }
    let mut lines = vec![
        header(&entry.icon, p.accent, label, pills, p),
        Line::raw(""),
    ];

    // `workspace get` counts panes but names none, so the card's body comes from
    // `pane list` — every pane in the session — narrowed to this workspace once.
    let list = herdr_json(runner, &["pane", "list"]);
    let panes: Vec<&Value> = list["result"]["panes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|pane| pane["workspace_id"].as_str() == Some(entry.id.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let pane_count = w["pane_count"].as_i64().unwrap_or(panes.len() as i64);
    lines.extend(workspace_stats(&panes, pane_count, p, theme));

    lines.push(Line::raw(""));
    lines.push(rule("agents", width, p));
    lines.push(Line::raw(""));
    lines.extend(workspace_agents(&panes, width, p, theme));

    let repos = workspace_repos(runner, &panes, width, p, theme);
    if !repos.is_empty() {
        lines.push(Line::raw(""));
        lines.push(rule("repos", width, p));
        lines.push(Line::raw(""));
        lines.extend(repos);
    }
    lines
}

/// The agent a pane is running, if any — the field's absence (a plain shell) or
/// emptiness both read as "no agent".
fn pane_agent(pane: &Value) -> Option<&str> {
    pane["agent"].as_str().filter(|s| !s.is_empty())
}

/// A pane's working directory, foreground first like [`agent_card`].
fn pane_cwd(pane: &Value) -> Option<&str> {
    pane["foreground_cwd"]
        .as_str()
        .or_else(|| pane["cwd"].as_str())
        .filter(|s| !s.is_empty())
}

/// The last path component — a repo's short name.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The summary rows: pane and agent counts, then a colour-coded breakdown of the
/// agents by status. The breakdown is skipped when nothing is running.
fn workspace_stats(
    panes: &[&Value],
    pane_count: i64,
    p: &Ink,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let agents: Vec<&Value> = panes
        .iter()
        .copied()
        .filter(|pane| pane_agent(pane).is_some())
        .collect();

    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled("panes  ", Style::default().fg(p.sub)),
        Span::styled(pane_count.to_string(), Style::default().fg(p.text)),
        Span::styled("   ·   ", Style::default().fg(p.overlay)),
        Span::styled("agents  ", Style::default().fg(p.sub)),
        Span::styled(agents.len().to_string(), Style::default().fg(p.text)),
    ])];

    if !agents.is_empty() {
        // Count by status, keeping first-seen order so the row stays stable.
        let mut order: Vec<String> = Vec::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for a in &agents {
            let s = a["agent_status"].as_str().unwrap_or("unknown").to_string();
            if counts
                .insert(s.clone(), *counts.get(&s).unwrap_or(&0) + 1)
                .is_none()
            {
                order.push(s);
            }
        }
        let mut spans = vec![
            Span::raw("  "),
            Span::styled("status ", Style::default().fg(p.sub)),
        ];
        for status in order {
            let n = counts[&status];
            spans.push(Span::styled(
                " ● ".to_string(),
                Style::default().fg(state_color(theme, &status)),
            ));
            spans.push(Span::styled(
                format!("{n} {status}"),
                Style::default().fg(p.sub),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The running agents: a coloured bullet, the agent's name, a status pill, and
/// the task it reports as its terminal title. The focused pane keeps the same
/// `▌` marker the list uses for its selection.
fn workspace_agents(panes: &[&Value], width: u16, p: &Ink, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for pane in panes.iter().copied() {
        let Some(agent) = pane_agent(pane) else {
            continue;
        };
        let status = pane["agent_status"].as_str().unwrap_or("unknown");
        let state = state_color(theme, status);
        let marker = if pane["focused"].as_bool().unwrap_or(false) {
            "▌"
        } else {
            " "
        };
        out.push(clip_line(
            Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(p.accent)),
                Span::styled("● ".to_string(), Style::default().fg(state)),
                Span::styled(
                    agent.to_string(),
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                pill(status, state, p),
            ]),
            width as usize,
        ));
        if let Some(title) = pane["terminal_title_stripped"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            out.push(clip_line(
                Line::from(vec![
                    Span::styled("   ⤷ ".to_string(), Style::default().fg(p.accent)),
                    Span::styled(title.to_string(), Style::default().fg(p.sub)),
                ]),
                width as usize,
            ));
        }
    }
    if out.is_empty() {
        out.push(note("(no agents running)", p));
    }
    out
}

/// The distinct repositories open across the workspace's panes, each with its
/// branch and a dirty marker — the same git read [`repo_card`] shows, gathered
/// over every pane's cwd and deduplicated in first-seen order.
fn workspace_repos(
    runner: &dyn CommandRunner,
    panes: &[&Value],
    width: u16,
    p: &Ink,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut seen = HashSet::new();
    let mut dirs: Vec<&str> = Vec::new();
    for pane in panes.iter().copied() {
        if let Some(dir) = pane_cwd(pane) {
            if seen.insert(dir) {
                dirs.push(dir);
            }
        }
    }
    if dirs.is_empty() {
        return Vec::new();
    }

    // A name column so the branches line up, capped near half the pane width.
    let cap = (width as usize / 2).max(12);
    let name_col = dirs
        .iter()
        .map(|d| basename(d).chars().count())
        .max()
        .unwrap_or(0)
        .min(cap);

    let mut out = Vec::new();
    for dir in dirs {
        let name = clip(basename(dir), name_col);
        // Detached HEAD has no symbolic ref; fall back to the short sha.
        let branch = branch_from_head(dir)
            .or_else(|| {
                git(runner, dir, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty())
            })
            .or_else(|| {
                git(runner, dir, &["rev-parse", "--short", "HEAD"]).filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "—".into());
        let dirty = git(
            runner,
            dir,
            &["status", "--porcelain", "--untracked-files=no"],
        )
        .is_some_and(|s| !s.is_empty());
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{name:<name_col$}"), Style::default().fg(p.text)),
            Span::raw("  "),
            Span::styled(branch, Style::default().fg(p.sub)),
        ];
        if dirty {
            spans.push(Span::styled(
                " ✎".to_string(),
                Style::default().fg(theme.or("yellow", Color::Yellow)),
            ));
        }
        out.push(clip_line(Line::from(spans), width as usize));
    }
    out
}

// --- repo ------------------------------------------------------------------

fn repo_card(
    entry: &Entry,
    runner: &dyn CommandRunner,
    script_dir: &str,
    cfg: &Config,
    width: u16,
    p: &Ink,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(dir) = entry.dir.as_deref() else {
        return vec![note("(no directory)", p)];
    };
    // ghq listed it, so it existed a moment ago; say so plainly rather than
    // rendering a card full of blanks.
    if !Path::new(dir).is_dir() {
        return vec![
            header(
                &entry.icon,
                entry.icon_color,
                &entry.label,
                vec![pill("missing", theme.or("red", Color::Red), p)],
                p,
            ),
            Line::raw(""),
            meta("path", &tilde(dir), width, p),
        ];
    }

    // Detached HEAD has no symbolic ref; fall back to the short sha.
    let branch = branch_from_head(dir)
        .or_else(|| {
            git(runner, dir, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty())
        })
        .or_else(|| git(runner, dir, &["rev-parse", "--short", "HEAD"]).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "—".into());
    let dirty = git(
        runner,
        dir,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .is_some_and(|s| !s.is_empty());
    let (state, state_c) = if dirty {
        ("dirty", theme.or("yellow", Color::Yellow))
    } else {
        ("clean", theme.or("green", Color::Green))
    };

    let mut lines = vec![
        header(
            &entry.icon,
            entry.icon_color,
            &entry.label,
            vec![pill(state, state_c, p)],
            p,
        ),
        Line::raw(""),
        meta("branch", &branch, width, p),
    ];
    // A repo with no commits yet has no last commit; the row simply goes unsaid.
    if let Some(last) =
        git(runner, dir, &["log", "-1", "--format=%cr · %s"]).filter(|s| !s.is_empty())
    {
        lines.push(meta("last", &last, width, p));
    }
    lines.push(meta("path", &tilde(dir), width, p));
    lines.push(Line::raw(""));
    lines.push(rule("files", width, p));
    lines.extend(tree(runner, dir, script_dir, width));

    if cfg.bool("preview_readme", true) {
        if let Some((name, body)) = readme(dir) {
            lines.push(Line::raw(""));
            lines.push(rule(&name, width, p));
            lines.push(Line::raw(""));
            lines.extend(readme_lines(&body, width, p));
        }
    }
    lines
}

/// How much of a README the card carries.
///
/// The old cut was 30 lines, from when the pane could not scroll and anything
/// past the first screen was unreachable anyway. Now that `⌥j`/`⌥k` move it,
/// that cut only hid the text the reader had scrolled down to find. What is
/// left is a bound on pathological files, not an editorial choice — and it is
/// announced rather than silent.
const README_LINES: usize = 400;

/// The README with the little markdown worth styling at this size: headings in
/// the title colour, bullets marked in the accent, and inline `code` /
/// `**bold**` through the changelog's own renderer — so a README here and the
/// `⌥c` popup treat markdown the same way.
fn readme_lines(body: &str, width: u16, p: &Ink) -> Vec<Line<'static>> {
    let base = Style::default().fg(p.sub);
    let code = Style::default().fg(p.accent);
    let mut out = Vec::new();
    for raw in body.lines().take(README_LINES) {
        // Links flatten to their text: a preview this narrow has no room for a
        // URL, and the badge markup at the top of a README is mostly URL. An
        // image is demoted to a link first, so it flattens to its alt text
        // instead of leaving the `!` behind.
        let row = crate::markdown::flatten_links(&raw.trim_end().replace("![", "["));
        let trimmed = row.trim_start();
        let line = if let Some(head) = trimmed.strip_prefix('#') {
            Line::from(Span::styled(
                head.trim_start_matches('#').trim().to_string(),
                Style::default().fg(p.title).add_modifier(Modifier::BOLD),
            ))
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![Span::styled("• ", Style::default().fg(p.accent))];
            spans.extend(crate::markdown::spans(item, base, code));
            Line::from(spans)
        } else {
            Line::from(crate::markdown::spans(&row, base, code))
        };
        out.push(clip_line(line, width as usize));
    }
    // Say what was left out, so a card that ends early never reads as a README
    // that ends there.
    let total = body.lines().count();
    if total > README_LINES {
        out.push(Line::raw(""));
        out.push(note(&format!("… {} more lines", total - README_LINES), p));
    }
    out
}

/// Trimmed stdout of a `git -C dir` call, or None when git fails. Success with
/// empty output stays `Some("")` — for `status --porcelain` the emptiness *is*
/// the answer — so callers that want a value filter for it themselves.
/// The current branch read straight from `HEAD`, with no subprocess.
///
/// `git symbolic-ref` costs a process spawn (~14ms measured) to report something
/// that is one line of a file. `.git` is a directory in a normal checkout and a
/// `gitdir:` pointer file in a linked worktree, so both shapes are followed here.
/// `None` means "not a plain branch" — a detached HEAD, or anything unreadable —
/// and the caller falls back to the git commands it already had.
fn branch_from_head(dir: &str) -> Option<String> {
    let dot_git = Path::new(dir).join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let pointer = fs::read_to_string(&dot_git).ok()?;
        let rel = pointer.trim().strip_prefix("gitdir:")?.trim();
        let path = Path::new(rel);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(dir).join(path)
        }
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let name = head.trim().strip_prefix("ref: refs/heads/")?;
    (!name.is_empty()).then(|| name.to_string())
}

fn git(runner: &dyn CommandRunner, dir: &str, args: &[&str]) -> Option<String> {
    let mut full = vec!["-C", dir];
    full.extend_from_slice(args);
    let started = std::time::Instant::now();
    let out = runner.capture("git", &full);
    if crate::trace::enabled() {
        crate::trace::span_with("preview.git", started, args.first().copied().unwrap_or("-"));
    }
    out
}

/// The file tree, still from `preview.sh`: it is the one part of the card that
/// is already ANSI (eza's colours and icons), so it passes through rather than
/// being re-styled here.
fn tree(runner: &dyn CommandRunner, dir: &str, script_dir: &str, width: u16) -> Vec<Line<'static>> {
    let script = format!("{script_dir}/preview.sh");
    let started = std::time::Instant::now();
    let out = runner.output("bash", &[&script, dir]);
    if crate::trace::enabled() {
        crate::trace::span("preview.tree_sh", started);
    }
    let Ok(out) = out else {
        return Vec::new();
    };
    let lines = out.stdout.into_text().map(|t| t.lines).unwrap_or_else(|_| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| Line::raw(l.to_string()))
            .collect()
    });
    lines
        .into_iter()
        .map(|l| clip_line(l, width as usize))
        .collect()
}

/// The first README-ish file at the repo root, as (display name, contents).
fn readme(dir: &str) -> Option<(String, String)> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().starts_with("readme"))
        .collect();
    names.sort();
    let name = names.into_iter().next()?;
    let body = fs::read_to_string(Path::new(dir).join(&name)).ok()?;
    Some((name, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    /// The visible text of a card, spans joined, for `contains` assertions.
    fn flat(lines: &[Line]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<&str>>()
            .join(" ")
    }

    fn ink() -> Ink {
        Ink::new(&Theme::default(), &Config::default())
    }

    #[test]
    fn agent_card_reads_herdr_json_into_name_status_doing_and_output() {
        let get = r#"{"result":{"agent":{"agent":"claude","agent_status":"working","foreground_cwd":"/tmp/x","terminal_title_stripped":"building the thing","pane_id":"p1"}}}"#;
        let read = r#"{"result":{"read":{"text":"compiling module\n"}}}"#;
        let runner = MockRunner::new()
            .on("agent get", get)
            .on("agent read", read);
        let entry = Entry {
            kind: Kind::Agent,
            id: "term-1".into(),
            dir: Some("/tmp/x".into()),
            label: "x".into(),
            icon: "●".into(),
            icon_color: Color::Reset,
            primary: String::new(),
            secondary: String::new(),
            search: String::new(),
        };
        let out = flat(&agent_card(&entry, &runner, 60, &ink(), &Theme::default()));
        assert!(out.contains("claude"), "{out}");
        assert!(out.contains("working"), "{out}");
        assert!(out.contains("building the thing"), "{out}");
        assert!(out.contains("compiling module"), "{out}");
    }

    #[test]
    fn workspace_card_shows_agents_and_repos_for_its_own_panes() {
        let get = r#"{"result":{"workspace":{"label":"work","agent_status":"blocked","number":2,"pane_count":2,"tab_count":1,"focused":true}}}"#;
        let panes = r#"{"result":{"panes":[
            {"workspace_id":"ws-1","agent":"claude","agent_status":"blocked","terminal_title_stripped":"Improve preview UI","cwd":"/tmp/herdr-ghq","focused":true},
            {"workspace_id":"ws-OTHER","agent":"codex","agent_status":"idle","terminal_title_stripped":"elsewhere entirely","cwd":"/tmp/other-repo"}
        ]}}"#;
        let runner = MockRunner::new()
            .on("workspace get", get)
            .on("pane list", panes)
            .on("symbolic-ref", "main")
            .on("status --porcelain", "");
        let entry = Entry {
            kind: Kind::Workspace,
            id: "ws-1".into(),
            dir: None,
            label: "work".into(),
            icon: "".into(),
            icon_color: Color::Reset,
            primary: String::new(),
            secondary: String::new(),
            search: String::new(),
        };
        let out = flat(&workspace_card(
            &entry,
            &runner,
            60,
            &ink(),
            &Theme::default(),
        ));
        // Its own pane's agent, task, repo, and branch all surface.
        assert!(out.contains("claude"), "{out}");
        assert!(out.contains("Improve preview UI"), "{out}");
        assert!(out.contains("herdr-ghq"), "{out}");
        assert!(out.contains("main"), "{out}");
        // A pane from another workspace never leaks in.
        assert!(
            !out.contains("codex"),
            "an agent from another workspace leaked in: {out}"
        );
        assert!(!out.contains("elsewhere entirely"), "{out}");
    }

    #[test]
    fn repo_card_reads_git_state_into_the_header_and_meta() {
        let dir = std::env::temp_dir().join(format!("ghq-prev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new()
            .on("symbolic-ref", "feature/x")
            .on("status --porcelain", "") // clean
            .on("log -1", "2 days ago · initial commit");
        let entry = Entry {
            kind: Kind::Repo,
            id: "o/r".into(),
            dir: Some(path),
            label: "r".into(),
            icon: "".into(),
            icon_color: Color::Reset,
            primary: String::new(),
            secondary: String::new(),
            search: String::new(),
        };
        let out = flat(&repo_card(
            &entry,
            &runner,
            ".",
            &Config::default(),
            60,
            &ink(),
            &Theme::default(),
        ));
        assert!(out.contains("feature/x"), "{out}");
        assert!(out.contains("clean"), "{out}");
        assert!(out.contains("initial commit"), "{out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clip_leaves_short_text_alone() {
        assert_eq!(clip("main", 10), "main");
        assert_eq!(clip("exactly-10", 10), "exactly-10");
    }

    #[test]
    fn clip_ellipsises_at_the_limit() {
        assert_eq!(clip("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn rule_fills_the_pane_width() {
        let p = Ink::new(&Theme::default(), &Config::default());
        let line = rule("files", 30, &p);
        assert_eq!(line.width(), 29);
    }

    #[test]
    fn meta_pads_the_label_into_a_column() {
        let p = Ink::new(&Theme::default(), &Config::default());
        let a = meta("cwd", "x", 40, &p);
        let b = meta("branch", "y", 40, &p);
        // Both values start at the same column, whatever the label's length.
        assert_eq!(a.spans[1].content.len(), b.spans[1].content.len());
    }

    /// Three spans, red/green/blue, four chars each.
    fn striped() -> Line<'static> {
        Line::from(vec![
            Span::styled("aaaa", Style::default().fg(Color::Red)),
            Span::styled("bbbb", Style::default().fg(Color::Green)),
            Span::styled("cccc", Style::default().fg(Color::Blue)),
        ])
    }

    #[test]
    fn clip_line_keeps_a_fitting_line_whole() {
        let line = clip_line(striped(), 12);
        assert_eq!(line.width(), 12);
        assert_eq!(line.spans.len(), 3);
    }

    #[test]
    fn clip_line_never_exceeds_the_width() {
        // The guarantee the scroll math leans on: one card line, one screen row.
        for w in 1..20usize {
            assert!(clip_line(striped(), w).width() <= w, "overflowed at {w}");
        }
    }

    #[test]
    fn clip_line_cuts_mid_span_and_keeps_its_colour() {
        let line = clip_line(striped(), 6);
        // "aaaa" survives whole; "bbbb" is cut to "b…" and stays green.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[1].content, "b…");
        assert_eq!(line.spans[1].style.fg, Some(Color::Green));
    }

    #[test]
    fn clip_line_drops_spans_past_the_edge() {
        let line = clip_line(striped(), 4);
        // Nothing of the green or blue span survives a width the red one fills.
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "aaaa");
    }

    #[test]
    fn readme_carries_a_normal_file_whole() {
        let p = Ink::new(&Theme::default(), &Config::default());
        let body: String = (1..=120).map(|i| format!("line {i}\n")).collect();
        // Well past the 30 lines the pre-scrolling card used to stop at.
        assert_eq!(readme_lines(&body, 76, &p).len(), 120);
    }

    #[test]
    fn readme_says_how_much_it_left_out() {
        let p = Ink::new(&Theme::default(), &Config::default());
        let body: String = (1..=README_LINES + 100)
            .map(|i| format!("line {i}\n"))
            .collect();
        let out = readme_lines(&body, 76, &p);
        let last: String = out
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // A card that stops early must never read as a README that stops there.
        assert!(last.contains("100 more lines"), "got {last:?}");
    }

    #[test]
    fn blankness_ignores_styling() {
        assert!(is_blank(&Line::from(vec![
            Span::styled("   ", Style::default().fg(Color::Red)),
            Span::raw("\t"),
        ])));
        assert!(!is_blank(&striped()));
    }
}
