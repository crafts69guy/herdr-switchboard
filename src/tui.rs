//! Shared rendering vocabulary: the rounded panel frame and its captioned
//! form, coloured command-bar pills, and measured hit zones. Terminal lifetime
//! and event scheduling live in the deep [`crate::surface`] module.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui::Frame;

use crate::config::Transparency;
use crate::data::Theme;

/// The background policy for every Switchboard-owned rectangle. Callers only
/// decide *where* they draw; this module owns how transparent and opaque modes
/// clear that rectangle so roots and nested cards cannot drift apart.
#[derive(Clone, Copy)]
pub struct SurfaceBackground {
    fill: Option<Color>,
}

impl SurfaceBackground {
    pub fn resolve(theme: &Theme, transparency: Transparency) -> Self {
        let fill = match transparency {
            Transparency::Transparent => None,
            Transparency::Opaque => Some(theme.or("panel_bg", Color::Rgb(16, 18, 20))),
        };
        Self { fill }
    }

    /// Erase the previous contents, then optionally fill the rectangle. Clearing
    /// first prevents a nested card from leaving the parent's text beneath it;
    /// a transparent card still shows the terminal, never the obscured widget.
    pub fn paint(self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        if let Some(fill) = self.fill {
            frame.render_widget(Block::default().style(Style::default().bg(fill)), area);
        }
    }
}

/// The frame every Switchboard surface wears: rounded, bordered in `border`,
/// captionless, and carrying no background decision of its own — that belongs
/// to [`SurfaceBackground`]. [`boxed`] is this plus a caption; a panel whose title
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;

    fn painted(mode: Transparency) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[("panel_bg", "#101214")]);
        let background = SurfaceBackground::resolve(&theme, mode);
        let mut terminal = Terminal::new(TestBackend::new(6, 3)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("parent")
                        .style(Style::default().bg(Color::Red).fg(Color::White)),
                    frame.area(),
                );
                background.paint(frame, Rect::new(1, 1, 3, 1));
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn transparent_background_clears_to_the_terminal_default() {
        let buffer = painted(Transparency::Transparent);
        for x in 1..4 {
            assert_eq!(buffer[(x, 1)].symbol(), " ");
            assert_eq!(buffer[(x, 1)].bg, Color::Reset);
        }
    }

    #[test]
    fn opaque_background_clears_and_fills_with_panel_bg() {
        let buffer = painted(Transparency::Opaque);
        for x in 1..4 {
            assert_eq!(buffer[(x, 1)].symbol(), " ");
            assert_eq!(buffer[(x, 1)].bg, Color::Rgb(0x10, 0x12, 0x14));
        }
    }
}
