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
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::data::{Config, Theme};
use crate::markdown::{self, Block, VERSION};
use crate::surface::{Surface, Transition};
use crate::tui::{self, Pill};

pub struct App {
    theme: Theme,
    title_color: Color,
    blocks: Vec<Block>,
    scroll: u16,
    /// Total rendered rows at the last draw, so scrolling can stop at the end.
    height: u16,
    rows: u16,
    /// The command bar's row and its pills, each carrying the key its cap
    /// advertises — so a click does exactly what the label promises, and the two
    /// cannot drift apart. Written by [`draw_bar`], the loop that lays them out.
    bar_row: u16,
    bar_zones: Vec<(u16, u16, KeyCode)>,
}

impl Surface for App {
    type Output = ();

    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => Ok(self.on_key(key)),
            Event::Mouse(mouse) => Ok(self.on_mouse(mouse)),
            _ => Ok(Transition::Wait),
        }
    }
}

impl App {
    fn on_key(&mut self, k: KeyEvent) -> Transition<()> {
        let page = self.rows.saturating_sub(2).max(1);
        let max = self.height.saturating_sub(self.rows);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Transition::Exit(()),
            KeyCode::Char('c') if ctrl => return Transition::Exit(()),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(max),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + page).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = max,
            _ => {}
        }
        Transition::Redraw
    }

    fn on_mouse(&mut self, m: MouseEvent) -> Transition<()> {
        match m.kind {
            // Three rows a notch: the conventional feel for text.
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    self.on_key(KeyEvent::from(KeyCode::Down));
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    self.on_key(KeyEvent::from(KeyCode::Up));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(code) =
                    tui::zone_at(&self.bar_zones, self.bar_row, (m.column, m.row).into())
                {
                    return self.on_key(KeyEvent::from(code));
                }
            }
            _ => {}
        }
        Transition::Redraw
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

fn draw_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));
    let sub = t.or("subtext0", Color::Gray);

    // Each pill beside the key it stands for: the cap *is* the behaviour, so a
    // relabelled pill cannot start doing something else.
    let caps = [
        // A pill naming a *pair* of keys is not clickable: one click cannot mean
        // both, and picking one would make the cap a half-truth. The wheel is
        // the pointer's way to scroll.
        (
            Pill::new("↑ ↓", "scroll", t.or("accent", Color::Cyan)),
            None,
        ),
        (
            Pill::new("g G", "top / end", t.or("blue", Color::Blue)),
            None,
        ),
        (
            Pill::new("esc", "close", t.or("red", Color::Red)),
            Some(KeyCode::Esc),
        ),
    ];
    let pills: Vec<Pill> = caps
        .iter()
        .map(|(p, _)| Pill::new(p.key, p.label, p.color))
        .collect();
    let (mut spans, zones) = tui::pill_row(&pills, ink, area.x);
    app.bar_row = area.y;
    app.bar_zones = zones
        .into_iter()
        .zip(caps.iter())
        .filter_map(|((a, b), (_, code))| code.map(|c| (a, b, c)))
        .collect();
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
        .resolve(&cfg.common.title_color)
        .unwrap_or(Color::Yellow);

    let blocks = markdown::parse(&changelog_text()?);
    let mut app = App {
        theme,
        title_color,
        blocks,
        scroll: 0,
        height: 0,
        rows: 1,
        bar_row: 0,
        bar_zones: Vec::new(),
    };

    crate::surface::run(&mut app)
}
