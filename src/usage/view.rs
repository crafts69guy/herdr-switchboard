//! Rendering and render-derived hit zones for the Usage Popup.

use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{
    donut_points, format_clock_dated, format_renewal, format_reset_dated, freshness, level_color,
    App, Fact, Severity, Slot, Window, ARC_STEPS, DONUT_MAX_H, RING_INNER, RING_OUTER,
};
use crate::config::Config;
use crate::data::Theme;
use crate::tui::{self, Pill};

/// The blank columns between two provider cards.
///
/// Nothing draws a divider — the pane is transparent and a rule between the
/// cards would compete with the one each card already draws above its facts —
/// so the whitespace is the only thing separating them, and adjacent cards read
/// as one wide table of rows rather than two independent answers.
pub(super) const CARD_GAP: u16 = 3;

/// The width a card needs before it can spare the full gutter: the longest row
/// it draws, `label + bar + percentage + a dated countdown`, plus the column
/// `draw_card` keeps clear on the right.
const CARD_COMFORTABLE: u16 = 44;

/// The gutter, but never at the expense of a card that is already cutting its
/// own rows. Two providers on the 96-column popup get the full gap; a pane
/// squeezed narrower spends its columns on content instead.
pub(super) fn card_gap(width: u16, cards: usize) -> u16 {
    let cards = cards.max(1) as u16;
    let each = width.saturating_sub(CARD_GAP * (cards - 1)) / cards;
    if each >= CARD_COMFORTABLE {
        CARD_GAP
    } else {
        1
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    app.background.paint(f, f.area());
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
    // Herdr frames and titles the popup pane already, so this draws no outer
    // border of its own — the same reason the changelog pane doesn't. The
    // shared background painter above still owns transparent versus opaque.
    if app.slots.is_empty() {
        let muted = app.theme.or("subtext0", Color::Gray);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " No usage providers are enabled.",
                Style::default().fg(muted),
            ))),
            rows[0],
        );
    } else {
        // `Fill` rather than `Percentage(100 / n)`, which throws away the
        // remainder — three providers took 99% of the pane and left a column
        // dark on the right.
        let columns = Layout::horizontal(
            app.slots
                .iter()
                .map(|_| Constraint::Fill(1))
                .collect::<Vec<_>>(),
        )
        .spacing(card_gap(rows[0].width, app.slots.len()))
        .split(rows[0]);
        // Built once, here, because the same list has to size the cards and
        // fill them: a count taken from one list and a render taken from
        // another drift the moment a row is added to only one of them.
        let facts = app
            .slots
            .iter()
            .map(|slot| card_facts(slot, app.now, app.offset))
            .collect::<Vec<_>>();
        // Every card gets the same row heights, taken from the busiest one.
        // Sized per card, a provider with one window and a provider with four
        // put their donuts — and every line under them — at different heights,
        // and two cards that will not line up read as two unrelated widgets.
        let rows = CardRows {
            bars: app
                .slots
                .iter()
                .map(|slot| slot.windows().len() as u16)
                .max()
                .unwrap_or(0),
            facts: facts.iter().map(|f| f.len() as u16).max().unwrap_or(0),
        };
        for ((slot, area), facts) in app.slots.iter().zip(columns.iter()).zip(facts.iter()) {
            draw_card(f, app, slot, facts, *area, rows);
        }
    }
    draw_bar(f, app, rows[1]);
}

/// One provider: a heading, a donut for the window closest to running out, a bar
/// per window, and the facts that give the percentage its context.
///
/// The vertical order is deliberate: the donut answers "am I fine?" from across
/// the room, the bars answer "which window?", and the facts answer "why?" — so
/// the card degrades gracefully when the pane is short, losing the explanation
/// before it loses the answer.
#[derive(Clone, Copy)]
struct CardRows {
    bars: u16,
    facts: u16,
}

/// A card's facts as they reach the screen: what the provider recorded, plus
/// the `renews` row, which only the drawing layer can build — it needs the
/// clock and the local offset the whole frame shares.
fn card_facts(slot: &Slot, now: u64, offset: i64) -> Vec<Fact> {
    let mut facts = slot.facts().to_vec();
    if let Slot::Ready(report) = slot {
        facts.push(Fact::new(
            "renews",
            format_renewal(now, report.renews_at, offset),
        ));
    }
    facts
}

fn draw_card(f: &mut Frame, app: &App, slot: &Slot, facts: &[Fact], area: Rect, card: CardRows) {
    let theme = &app.theme;
    let text = theme.or("text", Color::Reset);
    let muted = theme.or("subtext0", Color::Gray);
    let track = theme.or("overlay0", Color::DarkGray);

    let windows = slot.windows();
    // Bars, then a rule, then the facts — all reserved before the donut is
    // sized, so the donut takes what is left rather than crowding them out.
    let facts_h = if card.facts == 0 { 0 } else { card.facts + 1 };
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(card.bars),
        Constraint::Length(facts_h),
    ])
    .split(area);

    let mut head = vec![Span::styled(
        format!(" {}", slot.name()),
        Style::default()
            .fg(app.title_color)
            .add_modifier(Modifier::BOLD),
    )];
    if let Slot::Ready(report) = slot {
        if let Some(plan) = &report.plan {
            head.push(Span::styled(
                format!("  {plan}"),
                Style::default().fg(muted),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(head)), rows[0]);

    let hottest = slot.hottest();
    let pct = hottest.map(|window| window.used_percent);
    let (used_color, available_color) = donut_colors(theme, pct);
    draw_donut(f, pct, used_color, available_color, rows[1]);
    // The number sits in the hole rather than beside the ring, so the eye lands
    // on the figure and the colour reads as context around it.
    let label = match slot {
        Slot::Loading { .. } => "…".to_string(),
        Slot::Ready(_) => pct
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "—".into()),
        Slot::Unavailable { .. } => "—".into(),
    };
    let hole = centered(rows[1], label.chars().count() as u16 + 2, 1);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().fg(used_color).add_modifier(Modifier::BOLD),
        )))
        .centered(),
        hole,
    );

    // The one line that says how much of this card to trust.
    let status = match slot {
        Slot::Loading { .. } => Line::from(Span::styled(" reading…", Style::default().fg(muted))),
        Slot::Unavailable { reason, .. } => Line::from(Span::styled(
            format!(" {reason}"),
            Style::default().fg(muted),
        )),
        Slot::Ready(report) => {
            let mut note = freshness(app.now, report.measured_at);
            if let Some(at) = hottest.and_then(|window| window.resets_at) {
                let clock = format_clock_dated(app.now, at, app.offset);
                if !clock.is_empty() {
                    note = format!("{note} · resets {clock}");
                }
            }
            Line::from(Span::styled(format!(" {note}"), Style::default().fg(muted)))
        }
    };
    f.render_widget(Paragraph::new(status), rows[2]);

    let width = rows[3].width.saturating_sub(1) as usize;
    let bars = windows
        .iter()
        .map(|window| window_line(window, theme, app, width, text, muted))
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(bars), rows[3]);

    if facts_h > 0 && !facts.is_empty() {
        let mut lines = vec![Line::from(Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(1))),
            Style::default().fg(track),
        ))];
        // A long value — an email on a narrow pane — is cut rather than wrapped:
        // one fact is one row, so a wrap would push every row below it out of
        // line with the card beside it.
        let value_w = width.saturating_sub(FACT_KEY + 1);
        lines.extend(facts.iter().map(|fact| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<FACT_KEY$}", clip(&fact.key, FACT_KEY)),
                    Style::default().fg(muted),
                ),
                Span::styled(clip(&fact.value, value_w), Style::default().fg(text)),
            ])
        }));
        f.render_widget(Paragraph::new(lines), rows[4]);
    }
}

/// The width of a fact's label column, wide enough for the longest key either
/// provider produces plus a gap.
const FACT_KEY: usize = 10;
/// How many cells a window's bar gets. Fixed so the bars of both cards line up
/// even when their labels differ in length.
const BAR_W: usize = 12;

/// `weekly  ████████░░░░  42%   1d 12h` — one window, on one line.
fn window_line(
    window: &Window,
    theme: &Theme,
    app: &App,
    width: usize,
    text: Color,
    muted: Color,
) -> Line<'static> {
    let color = window_color(theme, window, &app.cfg);
    let filled = ((window.used_percent / 100.0) * BAR_W as f64).round() as usize;
    let filled = filled.min(BAR_W);
    // The reservation is the bar plus " NNN%" plus the reset text, whose worst
    // case is the dated form (`1d 12h · 19 Aug`) and its two leading spaces. On
    // a card too narrow for all of it the label is what yields.
    let label_w = width.saturating_sub(BAR_W + 23).clamp(6, 8);
    let mut spans = vec![
        Span::styled(
            format!(" {:<label_w$}", clip(&window.label, label_w)),
            Style::default().fg(muted),
        ),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(BAR_W - filled),
            Style::default().fg(theme.or("overlay0", Color::DarkGray)),
        ),
        Span::styled(
            format!(" {:>3.0}%", window.used_percent),
            Style::default().fg(text),
        ),
    ];
    if let Some(at) = window.resets_at {
        spans.push(Span::styled(
            format!("  {}", format_reset_dated(app.now, at, app.offset)),
            Style::default().fg(muted),
        ));
    }
    Line::from(spans)
}

/// A window's colour: the provider's own grading when it offers one, and the
/// configured thresholds when it does not.
pub(super) fn window_color(theme: &Theme, window: &Window, cfg: &Config) -> Color {
    match window.severity {
        Some(Severity::Normal) => theme.or("green", Color::Green),
        Some(Severity::Warning) => theme.or("yellow", Color::Yellow),
        Some(Severity::Critical) => theme.or("red", Color::Red),
        None => level_color(theme, window.used_percent, cfg),
    }
}

/// A ready donut is a composition rather than a severity gauge: red is quota
/// already spent and green is quota still available. Without a reading both
/// arcs stay muted so an unavailable provider cannot look fully available.
pub(super) fn donut_colors(theme: &Theme, pct: Option<f64>) -> (Color, Color) {
    if pct.is_some() {
        (theme.or("red", Color::Red), theme.or("green", Color::Green))
    } else {
        let muted = theme.or("overlay0", Color::DarkGray);
        (muted, muted)
    }
}

fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

fn draw_donut(
    f: &mut Frame,
    pct: Option<f64>,
    used_color: Color,
    available_color: Color,
    area: Rect,
) {
    // A terminal cell is about twice as tall as it is wide, so a ring drawn on
    // equal x/y bounds comes out as an ellipse unless the canvas is given twice
    // the columns it has rows. Squaring it here is what makes it look round.
    let height = area.height.min(DONUT_MAX_H).min(area.width / 2);
    if height == 0 {
        return;
    }
    let width = (height * 2).min(area.width);
    let square = centered(area, width, height);
    let (used, rest) = donut_points(pct.unwrap_or(0.0), RING_INNER, RING_OUTER, ARC_STEPS);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([-1.0, 1.0])
        .y_bounds([-1.0, 1.0])
        .paint(move |ctx| {
            ctx.draw(&Points {
                coords: &rest,
                color: available_color,
            });
            if pct.is_some() {
                ctx.draw(&Points {
                    coords: &used,
                    color: used_color,
                });
            }
        });
    f.render_widget(canvas, square);
}

/// A `width`×`height` rect in the middle of `area`, clamped to it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let muted = theme.or("subtext0", Color::Gray);
    // Each pill beside the key its cap advertises, so a click cannot mean
    // something other than what the label says.
    let caps = [
        (
            Pill::new("r", "refresh", theme.or("accent", Color::Cyan)),
            KeyCode::Char('r'),
        ),
        (
            Pill::new("esc", "close", theme.or("red", Color::Red)),
            KeyCode::Esc,
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
        .map(|((a, b), (_, code))| (a, b, *code))
        .collect();
    if app.inbox.is_some() {
        spans.push(Span::styled("fetching…", Style::default().fg(muted)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
