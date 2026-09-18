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
use ratatui::layout::Rect;
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
    background: crate::tui::SurfaceBackground,
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
    app.background.paint(f, f.area());
    let (area, bar) = tui::reserve_bar(f.area(), 1);

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
    draw_bar(f, app, bar);
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
        background: crate::tui::SurfaceBackground::resolve(&theme, cfg.common.transparency),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A pager with `height` rendered rows in a `rows`-tall pane.
    fn pager(height: u16, rows: u16) -> App {
        let theme = Theme::default();
        App {
            background: crate::tui::SurfaceBackground::resolve(
                &theme,
                crate::config::Transparency::Transparent,
            ),
            title_color: Color::Yellow,
            theme,
            blocks: Vec::new(),
            scroll: 0,
            height,
            rows,
            bar_row: 0,
            bar_zones: Vec::new(),
        }
    }

    /// Every movement key reaches the pager, and the scroll stops at both ends —
    /// past the last screenful there is nothing but blank rows.
    #[test]
    fn every_movement_key_scrolls_and_stops_at_both_ends() {
        let mut app = pager(100, 20);
        let max = 80;

        for (code, expected) in [
            (KeyCode::Down, 1),
            (KeyCode::Char('j'), 2),
            (KeyCode::Up, 1),
            (KeyCode::Char('k'), 0),
        ] {
            app.on_key(KeyEvent::from(code));
            assert_eq!(app.scroll, expected, "{code:?}");
        }

        app.on_key(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(app.scroll, 18, "a page is the visible rows less two");
        app.on_key(KeyEvent::from(KeyCode::PageUp));
        assert_eq!(app.scroll, 0);
        app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.scroll, 18, "space pages the way a pager does");

        app.on_key(KeyEvent::from(KeyCode::End));
        assert_eq!(app.scroll, max);
        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.scroll, max, "it cannot scroll past the end");
        app.on_key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(app.scroll, max);

        app.on_key(KeyEvent::from(KeyCode::Home));
        assert_eq!(app.scroll, 0);
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.scroll, 0, "and cannot scroll above the top");
        app.on_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.scroll, 0);
    }

    /// Content shorter than the pane cannot scroll at all.
    #[test]
    fn content_that_fits_never_scrolls() {
        let mut app = pager(5, 20);
        for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::End] {
            app.on_key(KeyEvent::from(code));
            assert_eq!(app.scroll, 0, "{code:?} scrolled content that fits");
        }
    }

    /// Both ways out, from the keyboard.
    #[test]
    fn the_popup_closes_on_esc_q_and_ctrl_c() {
        for code in [KeyCode::Esc, KeyCode::Char('q')] {
            let mut app = pager(100, 20);
            assert!(matches!(
                app.on_key(KeyEvent::from(code)),
                Transition::Exit(())
            ));
        }
        let mut app = pager(100, 20);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(app.on_key(ctrl_c), Transition::Exit(())));
        // A plain `c` is not a close.
        let mut app = pager(100, 20);
        assert!(matches!(
            app.on_key(KeyEvent::from(KeyCode::Char('c'))),
            Transition::Redraw
        ));
    }

    /// A wheel notch is three rows — the conventional feel for reading text.
    #[test]
    fn a_wheel_notch_moves_three_rows() {
        let mut app = pager(100, 20);
        let wheel = |kind| crossterm::event::MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };

        app.on_mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll, 3);
        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, 0);
    }

    /// A command-bar pill carries the key printed on its cap, so clicking it
    /// and pressing that key cannot diverge.
    #[test]
    fn a_bar_pill_click_runs_the_key_on_its_cap() {
        let mut app = pager(100, 20);
        app.bar_row = 23;
        app.bar_zones = vec![(0, 6, KeyCode::Esc), (8, 14, KeyCode::End)];
        let click = |column, row| crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };

        assert!(matches!(app.on_mouse(click(10, 23)), Transition::Redraw));
        assert_eq!(app.scroll, 80, "the End pill jumped to the end");
        assert!(matches!(app.on_mouse(click(2, 23)), Transition::Exit(())));
        // A click on no pill changes nothing.
        let mut app = pager(100, 20);
        app.bar_row = 23;
        app.bar_zones = vec![(0, 6, KeyCode::Esc)];
        assert!(matches!(app.on_mouse(click(40, 23)), Transition::Redraw));
        assert_eq!(app.scroll, 0);
    }

    /// An event the popup does not handle costs nothing.
    #[test]
    fn an_unhandled_event_is_a_wait() {
        let mut app = pager(100, 20);
        assert!(matches!(
            app.on_event(Event::Resize(80, 24)).unwrap(),
            Transition::Wait
        ));
    }
    use crate::config::Transparency;

    fn render(transparency: Transparency) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[("panel_bg", "#101214")]);
        let mut app = App {
            background: crate::tui::SurfaceBackground::resolve(&theme, transparency),
            theme,
            title_color: Color::Yellow,
            blocks: Vec::new(),
            scroll: 0,
            height: 0,
            rows: 1,
            bar_row: 0,
            bar_zones: Vec::new(),
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(88, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn standalone_changelog_obeys_both_background_modes() {
        let fill = Color::Rgb(0x10, 0x12, 0x14);
        let transparent = render(Transparency::Transparent);
        assert!(transparent.content.iter().all(|cell| cell.bg != fill));

        let opaque = render(Transparency::Opaque);
        assert!(opaque.content.iter().all(|cell| cell.bg != Color::Reset));
    }
}
