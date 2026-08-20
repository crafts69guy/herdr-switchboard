//! Shared TUI plumbing: the rounded panel frame and its captioned form, the
//! coloured command-bar pill row, and the plain draw/poll/read event loop the
//! two popup modes run.
//!
//! The picker keeps its own loop in `main.rs` on purpose — it also drives a
//! background preview worker and returns a chosen action, neither of which the
//! popup modes do. What the popups share verbatim is [`run_simple`]; what they
//! all share is the pill row and [`zone_at`], the one-row hit test every command
//! bar performs.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind, MouseEvent};
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

/// The frame every Switchboard surface wears: rounded, bordered in `border`,
/// captionless, and — the part that keeps the panes transparent — carrying no
/// background of its own. [`boxed`] is this plus a caption; a panel whose title
/// slot holds something richer than a word (the switcher's tab strip) builds
/// from here rather than hand-rolling a `Block` and drifting away from it.
pub fn framed(border: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
}

/// The one panel a Switchboard surface is allowed to draw: [`framed`], captioned
/// in `title_color`. Every framed thing goes through one of the two so the
/// projects picker, the mode pickers, and the popups cannot drift into three
/// different looks — the bug that shipped as accent boxes with unstyled captions.
pub fn boxed(label: &str, title_color: Color, border: Color) -> Block<'_> {
    framed(border).title(Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ))
}

/// One coloured command-bar pill: a bold key cap and its label, drawn in
/// `ink`-on-`color`.
pub struct Pill<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub color: Color,
}

impl<'a> Pill<'a> {
    pub fn new(key: &'a str, label: &'a str, color: Color) -> Self {
        Pill { key, label, color }
    }
}

/// Lay out a row of pills starting one column in from `start_x`, matching the
/// leading space the row opens with. Returns the spans to draw and, for each
/// pill, its `[x_start, x_end)` click zone — built in the same loop that lays
/// out the spans, so a zone can never drift from the pill a user aims at.
/// Callers that don't hit-test simply ignore the zones.
pub fn pill_row(pills: &[Pill], ink: Color, start_x: u16) -> (Vec<Span<'static>>, Vec<(u16, u16)>) {
    let mut spans = vec![Span::raw(" ")];
    let mut x = start_x + 1;
    let mut zones = Vec::with_capacity(pills.len());
    for p in pills {
        let cap = format!(" {} ", p.key);
        let label = format!("{} ", p.label);
        let w = (cap.chars().count() + label.chars().count()) as u16;
        zones.push((x, x + w));
        x += w + 1; // the trailing gap span below
        spans.push(Span::styled(
            cap,
            Style::default()
                .bg(p.color)
                .fg(ink)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(label, Style::default().bg(p.color).fg(ink)));
        spans.push(Span::raw(" "));
    }
    (spans, zones)
}

/// The payload whose zone contains `at`, for a bar that occupies a single row.
/// Every command bar's hit test is this line; only the payload differs, which is
/// why the *lookup* is shared and the *measurement* is not — a zone is still
/// built by the loop that lays the pills out, so the two cannot drift.
pub fn zone_at<T: Copy>(zones: &[(u16, u16, T)], row: u16, at: Position) -> Option<T> {
    (at.y == row)
        .then(|| zones.iter().find(|(a, b, _)| at.x >= *a && at.x < *b))
        .flatten()
        .map(|(_, _, payload)| *payload)
}

/// What a popup mode's key handler decides.
pub enum Flow {
    Continue,
    Quit,
}

/// A modal popup driven by [`run_simple`]: it draws itself and reacts to key
/// presses, nothing more.
pub trait SimpleMode {
    fn draw(&mut self, f: &mut Frame);
    fn on_key(&mut self, key: KeyEvent) -> Flow;
    /// A pointer event. The default ignores it, so a popup with nothing to
    /// scroll and nothing to click needs no arm.
    fn on_mouse(&mut self, m: MouseEvent) -> Flow {
        let _ = m;
        Flow::Continue
    }
}

/// The draw/poll/read loop the popup modes run: claim the terminal, redraw on
/// every wake, act on key and pointer events, and restore on quit or error. No
/// background work — the picker's loop handles that itself.
///
/// It claims the mouse through [`crate::init_terminal`] rather than
/// `ratatui::init`, which is the only thing that chains the mouse teardown ahead
/// of the panic hook. Turning it on by hand means turning it off on every exit
/// path, and this loop is two of them.
pub fn run_simple<M: SimpleMode>(mode: &mut M) -> Result<()> {
    let mut terminal = crate::init_terminal();
    let outcome = loop {
        if let Err(e) = terminal.draw(|f| mode.draw(f)) {
            break Err(e.into());
        }
        match event::poll(Duration::from_millis(200)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => break Err(e.into()),
        }
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                if let Flow::Quit = mode.on_key(k) {
                    break Ok(());
                }
            }
            Ok(Event::Mouse(m)) => {
                if let Flow::Quit = mode.on_mouse(m) {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    crate::restore_terminal();
    outcome
}
