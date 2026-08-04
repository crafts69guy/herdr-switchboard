//! Shared TUI plumbing: the rounded panel block, the coloured command-bar pill
//! row, and the plain draw/poll/read event loop the two popup modes run.
//!
//! The picker keeps its own loop in `main.rs` on purpose — it also drives a
//! background preview worker, consumes mouse events, and returns a chosen
//! action, none of which the settings/changelog popups do. What the two popups
//! share verbatim is [`run_simple`]; what all three share is the pill row.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

/// The one panel a Switchboard surface is allowed to draw: rounded, bordered in
/// `border`, captioned in `title_color`. Every framed thing goes through here so
/// the projects picker, the mode pickers, and the popups cannot drift into three
/// different looks — the bug that shipped as accent boxes with unstyled captions.
pub fn boxed(label: &str, title_color: Color, border: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
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
}

/// The draw/poll/read loop the settings and changelog popups both run: claim the
/// terminal, redraw on every wake, act on key presses, and restore on quit or
/// error. No mouse, no background work — the picker's loop handles those itself.
pub fn run_simple<M: SimpleMode>(mode: &mut M) -> Result<()> {
    let mut terminal = ratatui::init();
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
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    ratatui::restore();
    outcome
}
