//! Changelog viewer: `CHANGELOG.md`, rendered in the picker's colours.
//!
//! No network. An installed plugin is a git checkout of this repo, so the changelog
//! ships next to the code it describes, and `bin/release.sh` feeds the same section
//! verbatim to `gh release create` — the local file and the GitHub release notes are
//! the same text by construction.
//!
//! This draws no border of its own: herdr frames and titles the popup pane already.
//!
//! The markdown parse/render live in [`crate::markdown`], shared with the picker's
//! `⌥c` popup so the two surfaces cannot drift apart.

use std::fs;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::data::{Config, Theme};
use crate::markdown::{self, Block, VERSION};
use crate::tui::{self, Flow, Pill, SimpleMode};

pub struct App {
    theme: Theme,
    title_color: Color,
    blocks: Vec<Block>,
    scroll: u16,
    /// Total rendered rows at the last draw, so scrolling can stop at the end.
    height: u16,
    rows: u16,
}

impl SimpleMode for App {
    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn on_key(&mut self, k: KeyEvent) -> Flow {
        let page = self.rows.saturating_sub(2).max(1);
        let max = self.height.saturating_sub(self.rows);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Quit,
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(max),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + page).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = max,
            _ => {}
        }
        Flow::Continue
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
    let area = rows[0];

    let lines = markdown::render(
        &app.blocks,
        area.width.saturating_sub(2) as usize,
        &app.theme,
        app.title_color,
    );
    app.height = lines.len() as u16;
    app.rows = area.height;
    app.scroll = app.scroll.min(app.height.saturating_sub(app.rows));

    f.render_widget(Paragraph::new(lines).scroll((app.scroll, 0)), area);
    draw_bar(f, app, rows[1]);
}

fn draw_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));
    let sub = t.or("subtext0", Color::Gray);

    let pills = [
        Pill::new("↑ ↓", "scroll", t.or("accent", Color::Cyan)),
        Pill::new("g G", "top / end", t.or("blue", Color::Blue)),
        Pill::new("esc", "close", t.or("red", Color::Red)),
    ];
    let (mut spans, _) = tui::pill_row(&pills, ink, area.x);
    spans.push(Span::styled(
        format!("v{VERSION}"),
        Style::default().fg(sub),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `$HERDR_PLUGIN_ROOT/CHANGELOG.md` — the installed plugin is a checkout of this repo.
pub fn changelog_text() -> Result<String> {
    let root = std::env::var("HERDR_PLUGIN_ROOT").unwrap_or_else(|_| ".".into());
    let path = std::path::Path::new(&root).join("CHANGELOG.md");
    fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))
}

/// Entry point for `herdr-switchboard --changelog`.
pub fn main() -> Result<()> {
    let cfg = Config::try_load()?;
    let theme = Theme::load();
    let title_color = theme
        .resolve(&cfg.get("title_color", "peach"))
        .unwrap_or(Color::Yellow);

    let blocks = markdown::parse(&changelog_text()?);
    let mut app = App {
        theme,
        title_color,
        blocks,
        scroll: 0,
        height: 0,
        rows: 1,
    };

    tui::run_simple(&mut app)
}
