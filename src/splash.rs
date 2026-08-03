//! One static cat frame, drawn once and then replaced.
//!
//! What is left of the old startup animation. The picker no longer needs it —
//! it loads its sources synchronously and draws the list first — but the `--git`
//! mode does: selecting a review row `exec`s `tuicr` right there in the pane, and
//! tuicr can spend a second or more reading a large diff before it paints its own
//! first frame. This covers that gap with something branded instead of a black
//! pane.
//!
//! Deliberately frameless: there is no loop to advance it, no floor to hold it
//! visible, and nothing to cancel. Draw it, then `exec` over it.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::data::Theme;

/// Draw the cat centred in `area`, with `status` under it. A pane too small for
/// the full frame gets the three-line one instead.
pub fn draw(f: &mut Frame, area: Rect, theme: &Theme, title: Color, status: &str) {
    let glow = theme.or("green", Color::Green);
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::DarkGray);

    let compact = area.width < 36 || area.height < 12;
    let mut lines = Vec::new();

    if compact {
        lines.push(Line::styled(
            r" /\_/\",
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(vec![
            Span::styled("( ", Style::default().fg(text)),
            Span::styled(
                "o.o",
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" )", Style::default().fg(text)),
        ]));
        lines.push(Line::styled(" > ^ <", Style::default().fg(text)));
    } else {
        lines.push(Line::from(vec![
            Span::styled("*   ", Style::default().fg(glow)),
            Span::styled(
                r"/\_____/\",
                Style::default().fg(text).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::styled(
            r"   /         \",
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(vec![
            Span::styled("  |   ", Style::default().fg(text)),
            Span::styled(
                "o   o",
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("   |", Style::default().fg(text)),
        ]));
        lines.push(Line::styled(r"  |     ^     |", Style::default().fg(text)));
        lines.push(Line::styled(r"   \   ---   /", Style::default().fg(text)));
        lines.push(Line::styled(r"    |_______|", Style::default().fg(text)));
        lines.push(Line::from(vec![
            Span::styled("   / _|", Style::default().fg(text)),
            Span::styled("  tap tap  ", Style::default().fg(sub)),
            Span::styled(r"|_ \", Style::default().fg(text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" .--", Style::default().fg(text)),
            Span::styled(
                "[=]",
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("--[ ][ ][ ]--", Style::default().fg(text)),
            Span::styled(
                "[=]",
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("--.", Style::default().fg(text)),
        ]));
        lines.push(Line::styled(
            " '-----------------------'",
            Style::default().fg(title),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        status.to_string(),
        Style::default().fg(text).add_modifier(Modifier::BOLD),
    ));

    let content_height = lines.len() as u16;
    let top = area
        .y
        .saturating_add(area.height.saturating_sub(content_height) / 2);
    let draw_area = Rect::new(area.x, top, area.width, content_height.min(area.height));
    f.render_widget(
        Paragraph::new(Text::from(lines)).alignment(Alignment::Center),
        draw_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole screen as text, row by row.
    fn rendered(w: u16, h: u16) -> (String, ratatui::buffer::Buffer) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    f.area(),
                    &Theme::default(),
                    Color::Yellow,
                    "Opening review",
                )
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let screen = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (screen, buf)
    }

    #[test]
    fn the_full_cat_carries_the_status() {
        let (screen, _) = rendered(80, 24);
        assert!(screen.contains(r"/\_____/\"), "{screen}");
        assert!(screen.contains("Opening review"), "{screen}");
    }

    #[test]
    fn a_small_pane_gets_the_compact_cat() {
        let (screen, _) = rendered(34, 10);
        assert!(screen.contains("( o.o )"), "{screen}");
        assert!(screen.contains("Opening review"), "{screen}");
    }

    /// tuicr paints over this frame, so anything it tints stays tinted until it
    /// does. Leave the terminal's own background alone.
    #[test]
    fn the_terminals_default_background_is_preserved() {
        let (_, buf) = rendered(80, 24);
        assert!(buf.content.iter().all(|cell| cell.bg == Color::Reset));
    }
}
