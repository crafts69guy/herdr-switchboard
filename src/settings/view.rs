//! Rendering for the embedded and standalone settings card.

use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::catalog::SETTINGS;
use super::document::setting_path;
use super::Settings;
use crate::data::Theme;
use crate::tui::{self, Pill};

pub(super) const TABS: [&str; 4] = ["Common", "Projects", "Commands", "Ports"];

pub(super) fn setting_tab(key: &str) -> usize {
    match setting_path(key).0 {
        "common" | "zen" | "usage" => 0,
        "commands" => 2,
        "ports" => 3,
        _ => 1,
    }
}

/// Which section headings fill the left column; the rest go right. Kept in display
/// order within each column, so navigating down walks the left column top-to-bottom
/// and then continues into the right.
const LEFT_GROUPS: &[&str] = &["Open", "Sources", "Keys", "Preview"];
const RIGHT_GROUPS: &[&str] = &[
    "Appearance",
    "Clone",
    "Git",
    "Updates",
    "Notifications",
    "Catalog",
    "Monitor",
    "Zen",
    "Usage",
];

const NAME_W: usize = 21; // widest key ("notification_position")
const PILL_W: usize = 12; // widest value ("bottom-right")
/// Marker + key + gap + padded pill = one column's width. Both columns share it so
/// their pills line up; a per-row hint would not fit, so it moves to the footer.
const COL_W: usize = 2 + NAME_W + 1 + (PILL_W + 2);

/// One column's lines: a title-coloured heading when the group changes, then a
/// `marker · key · value-pill` row per setting whose group belongs to this column.
fn column(
    s: &Settings,
    theme: &Theme,
    title: Color,
    groups: &[&str],
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let ink = theme.or("panel_bg", Color::Black);
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::Gray);
    let accent = theme.or("accent", Color::Cyan);
    let peach = theme.or("peach", Color::Yellow);

    let mut lines: Vec<Line> = Vec::new();
    // Parallel to `lines`: which `SETTINGS` entry each screen row draws.
    let mut map: Vec<Option<usize>> = Vec::new();
    let mut last_group = "";
    for (i, setting) in SETTINGS.iter().enumerate() {
        if !groups.contains(&setting.group) || setting_tab(setting.key) != s.tab {
            continue;
        }
        if setting.group != last_group {
            if !lines.is_empty() {
                lines.push(Line::from(""));
                map.push(None);
            }
            lines.push(Line::from(Span::styled(
                format!(" {}", setting.group),
                Style::default().fg(title).add_modifier(Modifier::BOLD),
            )));
            map.push(None);
            last_group = setting.group;
        }

        let selected = i == s.sel;
        let editing = selected && s.editing.is_some();
        let changed = s.values[i] != s.saved[i];
        let value = match &s.editing {
            Some(buf) if selected => format!("{buf}▏"),
            _ => s.values[i].clone(),
        };
        // The selected (or editing) row's value pill uses the title colour to pop;
        // the rest sit in a calm accent, the way the cheatsheet's key caps do.
        let pill_bg = if selected || editing { title } else { accent };
        lines.push(Line::from(vec![
            // Two one-cell marks: the selection bar, then a peach dot on a row whose
            // draft differs from disk — so you can see what an apply would write.
            Span::styled(
                if selected { "▌" } else { " " },
                Style::default().fg(accent),
            ),
            Span::styled(if changed { "●" } else { " " }, Style::default().fg(peach)),
            Span::styled(
                format!("{:<NAME_W$}", setting.key),
                Style::default().fg(if selected { text } else { sub }),
            ),
            Span::raw(" "),
            Span::styled(
                format!(" {value:<PILL_W$} "),
                Style::default()
                    .bg(pill_bg)
                    .fg(ink)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        map.push(Some(i));
    }
    (lines, map)
}

/// Draw the settings card centred in `area`, over whatever is behind it. The picker
/// owns `theme`/`title`, so the overlay matches the rest of its surfaces.
pub fn draw(f: &mut Frame, area: Rect, theme: &Theme, title: Color, s: &mut Settings) {
    let ink = theme.or("panel_bg", Color::Black);
    let sub = theme.or("subtext0", Color::Gray);
    let border = theme.or("accent", Color::Cyan);

    // A centred, rounded, ink-filled floating card — the `?` cheatsheet's shape — over
    // the picker. Two columns of settings, the selected row's hint spelled out below
    // them, and the command-bar pills at the foot.
    let (left, left_rows) = column(s, theme, title, LEFT_GROUPS);
    let (right, right_rows) = column(s, theme, title, RIGHT_GROUPS);
    let body_h = left.len().max(right.len()) as u16;

    let want_w = (2 * COL_W + 4) as u16; // two columns + inner margin + border
    let w = want_w.min(area.width.saturating_sub(2));
    let want_h = body_h + 2 /* border */ + 3 /* tabs + hint + pills */;
    let h = want_h.min(area.height.saturating_sub(1)).max(6);
    let popup = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(ink))
        .title(Span::styled(
            " 󰒓 Switchboard Settings ",
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(if s.dirty() {
                Span::styled(
                    " ● unsaved ",
                    Style::default()
                        .fg(theme.or("peach", Color::Yellow))
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" saved ", Style::default().fg(sub))
            })
            .right_aligned(),
        );
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    // Laid out here rather than by a `Tabs` widget, because a widget that
    // positions itself internally cannot be measured from outside without the
    // two drifting — the same reason the switcher builds its own tab strip.
    let mut tab_spans: Vec<Span> = Vec::new();
    let mut x = rows[0].x;
    let mut tab_zones = Vec::new();
    for (i, name) in TABS.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::styled(" · ", Style::default().fg(sub)));
            x += 3;
        }
        let style = if i == s.tab {
            Style::default().fg(title).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(sub)
        };
        let w = name.chars().count() as u16;
        tab_zones.push((x, x + w, i));
        x += w;
        tab_spans.push(Span::styled(*name, style));
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), rows[0]);

    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .horizontal_margin(1)
        .split(rows[1]);
    f.render_widget(Paragraph::new(left), cols[0]);
    f.render_widget(Paragraph::new(right), cols[1]);
    s.zones.card = popup;
    s.zones.tab_row = rows[0].y;
    s.zones.tab_zones = tab_zones;
    s.zones.cols = [cols[0], cols[1]];
    s.zones.rows = [left_rows, right_rows];

    // The hint the narrow columns cannot carry, shown for the selected row only.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", SETTINGS[s.sel].hint),
            Style::default().fg(sub),
        ))),
        rows[2],
    );

    draw_bar(f, s, rows[3], theme);
}

/// The picker's coloured-pill command bar, with this form's verbs.
fn draw_bar(f: &mut Frame, s: &mut Settings, area: Rect, theme: &Theme) {
    let ink = theme.or("panel_bg", Color::Black);

    if let Some(err) = &s.error {
        let red = theme.or("red", Color::Red);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {err} "),
                Style::default().fg(red),
            ))),
            area,
        );
        return;
    }

    // The verbs follow the state: typing an edit, an unsaved draft (apply/discard on
    // offer), or a clean form (nothing to save, so `esc` just closes).
    // Each pill beside the key its cap advertises, so clicking one and pressing
    // it are the same code path. `↑ ↓` names a pair rather than one action.
    let caps: Vec<(Pill, Option<KeyCode>)> = if s.editing.is_some() {
        vec![
            (
                Pill::new("↵", "set", theme.or("accent", Color::Cyan)),
                Some(KeyCode::Enter),
            ),
            (
                Pill::new("esc", "cancel", theme.or("red", Color::Red)),
                Some(KeyCode::Esc),
            ),
        ]
    } else if s.dirty() {
        vec![
            (
                Pill::new("↵", "change", theme.or("accent", Color::Cyan)),
                Some(KeyCode::Enter),
            ),
            (
                Pill::new("↑ ↓", "move", theme.or("blue", Color::Blue)),
                None,
            ),
            (
                Pill::new("a", "apply", theme.or("green", Color::Green)),
                Some(KeyCode::Char('a')),
            ),
            (
                Pill::new("esc", "discard", theme.or("red", Color::Red)),
                Some(KeyCode::Esc),
            ),
        ]
    } else {
        vec![
            (
                Pill::new("↵", "change", theme.or("accent", Color::Cyan)),
                Some(KeyCode::Enter),
            ),
            (
                Pill::new("↑ ↓", "move", theme.or("blue", Color::Blue)),
                None,
            ),
            (
                Pill::new("esc", "close", theme.or("red", Color::Red)),
                Some(KeyCode::Esc),
            ),
        ]
    };
    let pills: Vec<Pill> = caps
        .iter()
        .map(|(p, _)| Pill::new(p.key, p.label, p.color))
        .collect();

    let (spans, zones) = tui::pill_row(&pills, ink, area.x);
    s.zones.bar_row = area.y;
    s.zones.bar_zones = zones
        .into_iter()
        .zip(caps.iter())
        .filter_map(|((a, b), (_, code))| code.map(|c| (a, b, c)))
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
