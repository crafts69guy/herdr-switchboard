//! Rendering and render-derived hit zones for the Git menu.

use crossterm::event::KeyCode;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{Git, ListKind, View};
use crate::data::Theme;
use crate::tui::SurfaceBackground;

pub(super) fn draw(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    background: SurfaceBackground,
    title: Color,
    g: &mut Git,
) {
    background.paint(f, area);
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    // The border recedes: herdr frames the pane in the accent already, and a
    // second accent box inside it reads as two competing frames.
    let border = theme.or("overlay0", Color::DarkGray);

    // A sub-list is a fixed-size box with its own internal layout.
    if matches!(g.view, View::List) {
        draw_list(f, area, theme, background, title, g);
        return;
    }
    if matches!(g.view, View::Confirm) {
        draw_confirm(f, area, theme, background, title, g);
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
    background.paint(f, popup);

    let cap = format!("󰊢 Git · {}", g.label);
    let block = crate::tui::boxed(&cap, title, border)
        .title(Line::from(Span::styled(" · git ", Style::default().fg(sub))).right_aligned());
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(inner);
    f.render_widget(Paragraph::new(lines), rows[0]);
    g.zones.card = popup;
    g.zones.menu = rows[0];
    g.zones.body = Rect::default();
    draw_bar(f, g, rows[1], theme);
}

/// The size warning for an all-files review, drawn to the menu card's shape:
/// what it is about to do, how much of it there is, and the two ways out.
///
/// It exists because the pane deliberately keeps the screen, draws one static
/// splash and `exec`s the review over itself — so a tree big enough to keep
/// tuicr reading for minutes is indistinguishable from a hang. This is the only
/// frame that can still say otherwise.
fn draw_confirm(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    background: SurfaceBackground,
    title: Color,
    g: &mut Git,
) {
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let border = theme.or("overlay0", Color::DarkGray);

    let lines = vec![
        Line::from(vec![
            Span::styled(" 󰈔 ", Style::default().fg(title)),
            Span::styled(
                " review all files",
                Style::default().fg(text).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!(" {} files", thousands(g.count)),
                Style::default().fg(title).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" tracked in this repo.", Style::default().fg(text)),
        ]),
        Line::from(Span::styled(
            " tuicr reads every one of them before its first",
            Style::default().fg(sub),
        )),
        Line::from(Span::styled(
            " frame; on a tree this size that is minutes.",
            Style::default().fg(sub),
        )),
    ];

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
    background.paint(f, popup);

    let cap = format!("󰊢 Git · {}", g.label);
    let block = crate::tui::boxed(&cap, title, border).title(
        Line::from(Span::styled(" · large repo ", Style::default().fg(sub))).right_aligned(),
    );
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(inner);
    f.render_widget(Paragraph::new(lines), rows[0]);
    // Nothing here is a row: only the bar can be clicked.
    g.zones.card = popup;
    g.zones.menu = Rect::default();
    g.zones.body = Rect::default();
    draw_bar(f, g, rows[1], theme);
}

/// `6699` → `6,699`. One number on one card; a formatting crate would be a
/// dependency for a comma.
pub(super) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A sub-list, drawn as a **fixed-size** card so filtering never resizes it: a
/// pinned detail header on top, a rounded search box just above the body, and —
/// the only part that scrolls — the paged row list, then the bar.
fn draw_list(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    background: SurfaceBackground,
    title: Color,
    g: &mut Git,
) {
    use ratatui::layout::Constraint::{Length, Min};

    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    let border = theme.or("overlay0", Color::DarkGray);

    // Size from the terminal, not the content, so the box is stable across searches.
    let w = (LIST_W as u16 + 6).min(area.width.saturating_sub(2));
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
    background.paint(f, popup);

    let what = g.kind.map(ListKind::title).unwrap_or("list");
    let tail = if g.rows.is_empty() {
        format!(" · {what} ")
    } else if g.filtered.is_empty() {
        format!(" · {what} · no matches ")
    } else {
        format!(" · {what} · {}/{} ", g.lsel + 1, g.filtered.len())
    };
    let cap = format!("󰊢 Git · {}", g.label);
    let block = crate::tui::boxed(&cap, title, border)
        .title(Line::from(Span::styled(tail, Style::default().fg(sub))).right_aligned());
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    g.zones.card = popup;
    g.zones.menu = Rect::default();
    g.zones.body = Rect::default();

    // Nothing came back: say which nothing, rather than an empty box.
    if g.rows.is_empty() {
        let rows = ratatui::layout::Layout::vertical([Min(1), Length(1)]).split(inner);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                g.kind.map(ListKind::empty).unwrap_or("  (nothing here)"),
                Style::default().fg(sub),
            ))),
            rows[0],
        );
        draw_bar(f, g, rows[1], theme);
        return;
    }

    // Header (fixed) · search box (rounded, near the body) · scrolling list · bar.
    let a =
        ratatui::layout::Layout::vertical([Length(LIST_HEADER_ROWS), Length(3), Min(1), Length(1)])
            .split(inner);
    let (header_area, search_area, body_area, bar_area) = (a[0], a[1], a[2], a[3]);

    f.render_widget(
        Paragraph::new(list_header_lines(
            g,
            text,
            sub,
            title,
            header_area.width as usize,
        )),
        header_area,
    );

    // The search input, its own rounded box.
    let sbox = crate::tui::framed(border);
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

    // Only this scrolls: the page of rows holding the selection.
    let viewport = body_area.height as usize;
    let start = page_start(g.lsel, g.filtered.len(), viewport);
    f.render_widget(
        Paragraph::new(list_body_lines(
            g,
            text,
            sub,
            accent,
            viewport,
            start,
            body_area.width as usize,
        )),
        body_area,
    );
    g.zones.body = body_area;
    g.zones.page_start = start;

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

/// Target content width of the fixed sub-list card, driving the card's width and
/// the label wrap.
const LIST_W: usize = 68;

/// The pinned detail header's height: one meta row plus up to two label rows.
const LIST_HEADER_ROWS: u16 = 3;

/// The date column in a list row, padded so labels line up.
const DATE_COL: usize = 14;

/// Widest an id may draw in a list row. A PR number is two characters, but a
/// tuicr session slug is a whole revset —
/// `herdr-switchboard@main/staged-and-unstaged/4e27385` — and unclipped it eats the row
/// until the label has nothing left. The pinned header still shows the id at the
/// card's full width, so nothing is only visible here.
const ID_COL: usize = 26;

/// The leading run of the sub-list search line (a leading space, the filter icon,
/// a space) — shared so `draw_list` can place the real terminal cursor exactly
/// where the query text starts.
const SEARCH_PREFIX: &str = " 󰍉 ";

/// Case-insensitive subsequence fuzzy match: every char of `needle` appears in
/// `haystack` in order. An empty needle matches everything.
pub(super) fn fuzzy_match(needle: &str, haystack: &str) -> bool {
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

/// The pinned detail header for the highlighted row — its id, date, and detail,
/// then the (wrapped, two-row-capped) label. Fixed height, so it never shrinks
/// while filtering; a clipped list row is never the only place a label appears.
/// Falls back to a placeholder when the filter matches nothing.
fn list_header_lines(
    g: &Git,
    text: Color,
    sub: Color,
    title: Color,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(r) = g.filtered.get(g.lsel).and_then(|&i| g.rows.get(i)) else {
        return vec![Line::from(Span::styled(
            "  no matches",
            Style::default().fg(sub),
        ))];
    };
    let mut meta = vec![
        Span::raw(" "),
        Span::styled(
            r.id.clone(),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ),
    ];
    if !r.meta.is_empty() {
        meta.push(Span::styled(
            format!("  {}", r.meta),
            Style::default().fg(sub),
        ));
    }
    if !r.detail.is_empty() {
        meta.push(Span::styled(
            format!("  ·  {}", r.detail),
            Style::default().fg(sub),
        ));
    }
    let mut lines = vec![Line::from(meta)];
    for (prefix, line) in crate::markdown::wrap(&r.label, width.saturating_sub(1), "", "")
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

/// The scrolling body: the page of the filtered list holding the selection, each
/// row a marker, id, date column, and clipped label. This is the only part of the
/// card that moves; `viewport` is the body area's height.
/// The first row of the page `sel` falls on. Paged rather than scrolled, so a
/// long list moves a screen at a time instead of one row under the cursor.
///
/// Computed once per draw and stored in [`Zones`], because a click has to read
/// the *same* page the render used — recomputing it in the hit test is how the
/// two silently disagree.
fn page_start(sel: usize, len: usize, viewport: usize) -> usize {
    let viewport = viewport.max(1);
    if len <= viewport {
        0
    } else {
        (sel / viewport) * viewport
    }
}

fn list_body_lines(
    g: &Git,
    text: Color,
    sub: Color,
    accent: Color,
    viewport: usize,
    start: usize,
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
    let end = (start + viewport).min(len);
    g.filtered
        .iter()
        .enumerate()
        .take(end)
        .skip(start)
        .filter_map(|(rank, &i)| {
            let r = g.rows.get(i)?;
            let selected = rank == g.lsel;
            // marker(2) + id + space + date column + space, then the label fills the rest.
            let id = clip(&r.id, ID_COL);
            let room = width.saturating_sub(5 + id.chars().count() + DATE_COL);
            let date = clip(&r.meta, DATE_COL);
            Some(Line::from(vec![
                Span::styled(
                    if selected { "▌ " } else { "  " },
                    Style::default().fg(accent),
                ),
                Span::styled(
                    format!("{id} "),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{date:<DATE_COL$} "), Style::default().fg(sub)),
                Span::styled(
                    clip(&r.label, room),
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
        View::List => vec![
            crate::tui::Pill::new(
                "↵",
                g.kind.map(ListKind::verb).unwrap_or("open"),
                theme.or("accent", Color::Cyan),
            ),
            crate::tui::Pill::new("↑ ↓", "move", theme.or("blue", Color::Blue)),
            crate::tui::Pill::new("esc", "back", theme.or("red", Color::Red)),
        ],
        View::Confirm => vec![
            crate::tui::Pill::new("↵", "open anyway", theme.or("accent", Color::Cyan)),
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

fn draw_bar(f: &mut Frame, g: &mut Git, area: Rect, theme: &Theme) {
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let pills = bar_pills(g, theme);
    let (spans, zones) = crate::tui::pill_row(&pills, ink, area.x);
    // A pill's payload is the key printed on its cap, so clicking one and
    // pressing it are the same code path and cannot drift apart. `↑ ↓` names a
    // pair rather than a verb and is not clickable.
    let codes = bar_keys(g);
    g.zones.bar_row = area.y;
    g.zones.bar_zones = zones
        .into_iter()
        .zip(codes)
        .filter_map(|((a, b), code)| code.map(|c| (a, b, c)))
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The key each pill in [`bar_pills`] stands for, in the same order. `None` for
/// a pill that names a pair of keys rather than one action.
fn bar_keys(g: &Git) -> Vec<Option<KeyCode>> {
    match g.view {
        View::Menu | View::List => vec![Some(KeyCode::Enter), None, Some(KeyCode::Esc)],
        View::Confirm => vec![Some(KeyCode::Enter), Some(KeyCode::Esc)],
    }
}
