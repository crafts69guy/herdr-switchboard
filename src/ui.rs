//! Rendering: Search input (top), Switcher list (middle), Preview (below), and
//! a full-width colourful command bar pinned to the very bottom.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::action::Accept;
use crate::keymap::{Action, Mode};
use crate::App;

pub fn draw(f: &mut Frame, app: &mut App) {
    let accent = app.theme.or("accent", Color::Cyan);
    let text = app.theme.or("text", Color::Reset);
    let sub = app.theme.or("subtext0", Color::DarkGray);
    let overlay = app.theme.or("overlay0", Color::DarkGray);
    let surface = app.theme.or("surface1", Color::Indexed(236));

    let root = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .split(f.area());

    // Body: list + preview. The footer (root[2]) is always a separate full-width
    // row, so the preview can sit on any side without shrinking the command bar.
    let body = root[1];
    let (list_area, preview_area) = if app.preview.enabled {
        let pct = app.preview.pct;
        let rest = 100u16.saturating_sub(pct);
        match app.preview.position.as_str() {
            "right" => {
                let c =
                    Layout::horizontal([Constraint::Percentage(rest), Constraint::Percentage(pct)])
                        .split(body);
                (c[0], Some(c[1]))
            }
            "left" => {
                let c =
                    Layout::horizontal([Constraint::Percentage(pct), Constraint::Percentage(rest)])
                        .split(body);
                (c[1], Some(c[0]))
            }
            "up" => {
                let c =
                    Layout::vertical([Constraint::Percentage(pct), Constraint::Percentage(rest)])
                        .split(body);
                (c[1], Some(c[0]))
            }
            _ => {
                let c =
                    Layout::vertical([Constraint::Percentage(rest), Constraint::Percentage(pct)])
                        .split(body);
                (c[0], Some(c[1]))
            }
        }
    } else {
        (body, None)
    };

    let title = app.title_color;
    draw_input(f, app, root[0], title, accent, sub, overlay);
    draw_list(f, app, list_area, title, accent, text, overlay, surface);
    if let Some(area) = preview_area {
        // Publish where the pane landed: the next render request clips the card
        // to its width, the scroll clamps to its height, and a wheel turn asks
        // whether the pointer is inside it. A resize therefore reaches the card
        // on the next request, not this frame — the shown card keeps the width
        // it was built at until the selection moves.
        app.preview.area = Some(area);
        draw_preview(f, app, area, title, overlay);
    }
    draw_footer(f, app, root[2]);

    if app.changelog.show {
        draw_changelog(f, app, f.area());
    }
    if app.settings.show {
        crate::settings::draw(f, f.area(), &app.theme, app.title_color, &mut app.settings);
    }
    if app.show_help {
        draw_help(f, app, f.area());
    }
}

/// The changelog, over the list rather than instead of it: reading what changed should
/// not cost you your place. Same parser and renderer as the standalone `--changelog`
/// pane, so the two cannot drift.
fn draw_changelog(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));
    let sub = t.or("subtext0", Color::Gray);
    let border = t.or("accent", Color::Cyan);
    let title = app.title_color;

    let w = area.width.saturating_sub(8).clamp(48, 84);
    let h = area.height.saturating_sub(4).clamp(8, 32);
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
            "  Changelog ",
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(Span::styled(
                " ↑↓ scroll · esc close ",
                Style::default().fg(sub),
            ))
            .right_aligned(),
        );
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let lines = crate::markdown::render(
        &app.changelog.blocks,
        inner.width.saturating_sub(2) as usize,
        &app.theme,
        title,
    );
    let c = &mut app.changelog;
    c.len = lines.len() as u16;
    c.rows = inner.height;
    c.scroll = c.scroll.min(c.len.saturating_sub(c.rows));

    f.render_widget(Paragraph::new(lines).scroll((c.scroll, 0)), inner);
}

fn draw_input(
    f: &mut Frame,
    app: &App,
    area: Rect,
    title: Color,
    accent: Color,
    sub: Color,
    border: Color,
) {
    let count = format!(
        " {}/{} ",
        app.picker.filtered.len(),
        app.picker.entries.len()
    );
    // Which mode owns the keys — a vimmer's `-- INSERT --`. Normal is always one
    // Esc away, so the tag is always shown; a pending `␣` leader appends a dot.
    let ink = app.theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let (tag, bg) = match app.mode {
        Mode::Normal => (
            if app.leader_pending {
                " NORMAL ␣ "
            } else {
                " NORMAL "
            },
            accent,
        ),
        Mode::Insert => (" INSERT ", app.theme.or("green", Color::Green)),
    };
    let block = crate::tui::boxed("Search", title, border)
        .title(Line::from(Span::styled(count, Style::default().fg(sub))).right_aligned())
        .title(Line::from(Span::styled(
            tag,
            Style::default().bg(bg).fg(ink).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // In Normal mode the prompt caret is dim: keys are commands, not text.
    let prompt = if app.mode == Mode::Normal {
        sub
    } else {
        accent
    };
    let line = Line::from(vec![
        Span::styled("  ", Style::default().fg(prompt)),
        Span::raw(&app.picker.query),
    ]);
    f.render_widget(Paragraph::new(line), inner);
    // Cursor after the prompt + query.
    let cx = inner.x + 2 + app.picker.query.chars().count() as u16;
    f.set_cursor_position(Position::new(
        cx.min(inner.x + inner.width.saturating_sub(1)),
        inner.y,
    ));
}

#[allow(clippy::too_many_arguments)]
fn draw_list(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    title: Color,
    accent: Color,
    text: Color,
    border: Color,
    surface: Color,
) {
    let items: Vec<ListItem> = app
        .picker
        .filtered
        .iter()
        .map(|&i| {
            let e = &app.picker.entries[i];
            let mut primary = e.primary.clone();
            let width = 38usize;
            let plen = primary.chars().count();
            if plen < width {
                primary.push_str(&" ".repeat(width - plen));
            }
            // The secondary column carries the entry's own colour (host tint
            // for repos, live state for agents, accent for workspaces) so the
            // list reads as colourful at a glance instead of a wall of grey.
            ListItem::new(Line::from(vec![
                Span::styled(e.icon.clone(), Style::default().fg(e.icon_color)),
                Span::raw(" "),
                Span::styled(primary, Style::default().fg(text)),
                Span::raw(" "),
                Span::styled(
                    e.secondary.clone(),
                    Style::default()
                        .fg(e.icon_color)
                        .add_modifier(Modifier::DIM),
                ),
            ]))
        })
        .collect();

    // Title row = a group tab strip (All + each present kind) with the active
    // tab highlighted, plus a right-aligned sort indicator.
    let ink = app.theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let mut tab_spans: Vec<Span> = Vec::new();
    // A tab's click zone is measured in the loop that lays it out: the two
    // cannot drift, because there is only one place that decides where a tab is.
    // Titles start one column in, past the block's corner.
    let mut x = area.x + 1;
    let mut zones = Vec::new();
    for g in app.picker.tabs() {
        let style = if g == app.picker.group {
            Style::default()
                .fg(ink)
                .bg(title)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(border)
        };
        let label = format!(" {} ", g.label());
        let w = label.chars().count() as u16;
        zones.push((x, x + w, g));
        x += w + 1; // the gap span below
        tab_spans.push(Span::styled(label, style));
        tab_spans.push(Span::raw(" "));
    }
    app.zones.tab_zones = zones;
    let sort_hint = Span::styled(
        format!(" sort: {} ", app.picker.sort.label()),
        Style::default().fg(border),
    );
    // `framed` rather than `boxed`: this panel's caption slot is the tab strip,
    // a multi-span line, not a word — but the frame itself must still be the
    // shared one, or it drifts the way the git card did.
    let block = crate::tui::framed(border)
        .title(Line::from(tab_spans))
        .title(Line::from(sort_hint).right_aligned());

    let list = List::new(items)
        .block(block)
        .highlight_symbol("▌ ")
        .highlight_style(
            Style::default()
                .fg(accent)
                .bg(surface)
                .add_modifier(Modifier::BOLD),
        );

    // The state carries the scroll offset between frames — a click can only be
    // turned back into an entry if we know which row was showing first. It also
    // means the list keeps its scroll position instead of re-deriving it from
    // the top on every frame.
    let selected = (!app.picker.filtered.is_empty()).then_some(app.picker.selected);
    app.zones.list_state.select(selected);
    app.zones.list_area = area;
    f.render_stateful_widget(list, area, &mut app.zones.list_state);
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect, title: Color, border: Color) {
    let mut block = crate::tui::boxed("󰈈 Preview", title, border);
    // Say so only when there is something below the fold, and say where you are
    // — an offset on a card that fits would be noise.
    if app.preview.scroll > 0 || app.preview.len > app.preview.rows() {
        let sub = app.theme.or("subtext0", Color::DarkGray);
        let last = app.preview.scroll + app.preview.rows().min(app.preview.len);
        block = block.title(
            Line::from(Span::styled(
                format!(" ⌥jk {last}/{} ", app.preview.len),
                Style::default().fg(sub),
            ))
            .right_aligned(),
        );
    }
    // A slow render shows the placeholder rather than the previous entry's
    // preview, which would otherwise read as the current one.
    let (body, scroll) = match app.preview.placeholder_frame() {
        Some(frame) => (placeholder(app, frame, area), 0),
        None => (app.preview.text.clone(), app.preview.scroll),
    };
    // No `Wrap`: every body is clipped to the pane, so one card line is one row
    // and `scroll` counts what the eye counts. Wrapping would make the offset
    // drift from the content as soon as a line ran long.
    let para = Paragraph::new(body).block(block).scroll((scroll, 0));
    f.render_widget(para, area);
}

/// Braille spinner over a travelling wave, centred in the preview pane, shown
/// while the worker renders. `frame` advances once per animation tick.
fn placeholder(app: &App, frame: usize, area: Rect) -> Text<'static> {
    const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
    const WAVE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    let accent = app.theme.or("accent", Color::Cyan);
    let sub = app.theme.or("subtext0", Color::DarkGray);
    // Inside the block's borders.
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;

    let wave: String = (0..width.min(28))
        .map(|i| {
            // Each column trails the one before it, so the crest travels right.
            let phase = i as f32 * 0.6 - frame as f32 * 0.5;
            let level = (phase.sin() + 1.0) / 2.0 * (WAVE.len() - 1) as f32;
            WAVE[(level.round() as usize).min(WAVE.len() - 1)]
        })
        .collect();

    let mut label = app.preview.label.clone();
    if label.chars().count() > width {
        label = label.chars().take(width.saturating_sub(1)).collect();
        label.push('…');
    }

    let centred = |s: String, style: Style| {
        let pad = width.saturating_sub(s.chars().count()) / 2;
        Line::from(vec![Span::raw(" ".repeat(pad)), Span::styled(s, style)])
    };

    // The block is 5 rows tall; sit it in the middle of the pane.
    let mut lines: Vec<Line> = vec![Line::raw(""); height.saturating_sub(5) / 2];
    lines.push(centred(
        SPINNER[frame % SPINNER.len()].to_string(),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));
    lines.push(centred(label, Style::default().fg(sub)));
    lines.push(Line::raw(""));
    lines.push(centred(
        wave,
        Style::default().fg(accent).add_modifier(Modifier::DIM),
    ));
    Text::from(lines)
}

fn draw_footer(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    // Dark ink for text sitting on the coloured pills.
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));
    // The bar's order, colour, and short label are fixed; the key cap is read
    // from the keymap for the *current mode*, so a remap or an Insert↔Normal
    // switch re-labels every pill (e.g. `update` shows `^r` in Insert, `␣u` in
    // Normal). An action with no binding in this mode drops out of the bar.
    let items: [(Action, &str, Color); 10] = [
        (
            Action::Accept(Accept::Default),
            "open",
            t.or("accent", Color::Cyan),
        ),
        (
            Action::Accept(Accept::Tab),
            "tab",
            t.or("green", Color::Green),
        ),
        (
            Action::Accept(Accept::Split),
            "split",
            t.or("yellow", Color::Yellow),
        ),
        (
            Action::Accept(Accept::Pane),
            "cd",
            t.or("blue", Color::Blue),
        ),
        (
            Action::Accept(Accept::Workspace),
            "workspace",
            t.or("mauve", Color::Magenta),
        ),
        (
            Action::Accept(Accept::Update),
            "update",
            t.or("teal", Color::Cyan),
        ),
        (
            Action::Accept(Accept::Remove),
            "remove",
            t.or("red", Color::Red),
        ),
        (
            Action::Accept(Accept::Clone),
            "clone",
            t.or("lavender", Color::Magenta),
        ),
        (Action::Settings, "settings", t.or("teal", Color::Cyan)),
        (Action::Help, "help", t.or("lavender", Color::White)),
    ];
    // Own the caps so the `Pill`s can borrow them for `pill_row`.
    let shown: Vec<(String, &str, Color, Action)> = items
        .iter()
        .filter(|&&(action, _, _)| app.action_available(action))
        .filter_map(|&(action, label, color)| {
            app.keymap
                .label_for(app.mode, action)
                .map(|cap| (cap, label, color, action))
        })
        .collect();
    let pills: Vec<crate::tui::Pill> = shown
        .iter()
        .map(|(cap, label, color, _)| crate::tui::Pill::new(cap, label, *color))
        .collect();
    let (spans, zones) = crate::tui::pill_row(&pills, ink, area.x);
    app.zones.footer_zones = zones
        .into_iter()
        .zip(shown.iter().map(|(_, _, _, a)| *a))
        .map(|((a, b), act)| (a, b, act))
        .collect();
    app.zones.footer_row = area.y;
    let pills_width: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    // A newer version, mentioned once, at the far end and out of the way of the keys.
    // Nothing here installs anything; it is a fact, not a prompt — so it yields to the
    // command bar rather than overdrawing it, and simply goes unsaid when the keys
    // already fill the row. The changelog pane still shows the version.
    if let Some(v) = &app.update {
        let badge = format!(" ↑ v{v} ");
        let w = badge.chars().count() as u16;
        if area.width >= pills_width + w {
            let at = Rect::new(area.x + area.width - w, area.y, w, 1);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    badge,
                    Style::default()
                        .bg(t.or("peach", Color::Yellow))
                        .fg(ink)
                        .add_modifier(Modifier::BOLD),
                ))),
                at,
            );
        }
    }
}

/// Width of a cheatsheet key pill, and what a description has left beside it.
///
/// The popup is [`HELP_W`] columns at most, split into two halves; the pill and
/// its two-space gap eat the rest. A longer description is **silently cut** —
/// the column has no ellipsis to tell you, so `row` asserts instead. This is
/// how `wheel  Scroll whatever is under it` shipped as `Scroll whatever is`.
const KEY_PILL: usize = 8;
const HELP_W: u16 = 64;
const HELP_DESC: usize = (HELP_W as usize - 2) / 2 - 1 - (KEY_PILL + 1) - 2;

/// A centred, colourful keybindings cheatsheet drawn on top of everything. Every
/// key cap is read from the live keymap for the current mode, so it reflects
/// remaps and shows the Insert or Normal bindings you are actually holding.
fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));
    let text = t.or("text", Color::Reset);
    let sub = t.or("subtext0", Color::Gray);
    let title = app.title_color;
    let border = t.or("accent", Color::Cyan);

    // A row: a colour-filled key pill followed by its description.
    let row = |key: &str, color: Color, desc: &str| -> Line<'static> {
        debug_assert!(
            desc.chars().count() <= HELP_DESC,
            "help description {desc:?} is {} chars; the column fits {HELP_DESC}",
            desc.chars().count()
        );
        Line::from(vec![
            Span::styled(
                format!(" {key:<KEY_PILL$}"),
                Style::default()
                    .bg(color)
                    .fg(ink)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(desc.to_string(), Style::default().fg(text)),
        ])
    };
    let head = |s: &str| -> Line<'static> {
        Line::from(Span::styled(
            s.to_string(),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
    };
    let blank = || Line::from("");

    let green = t.or("green", Color::Green);
    let yellow = t.or("yellow", Color::Yellow);
    let blue = t.or("blue", Color::Blue);
    let mauve = t.or("mauve", Color::Magenta);
    let peach = t.or("peach", Color::Yellow);
    let teal = t.or("teal", Color::Cyan);
    let red = t.or("red", Color::Red);

    // A row for `action`, or nothing when the current mode does not bind it —
    // so Insert hides `gg`/`G` and Normal shows the manage verbs as `␣…`.
    let opt = |action: Action, color: Color, desc: &'static str| -> Option<Line<'static>> {
        if !app.action_available(action) {
            return None;
        }
        app.keymap
            .label_for(app.mode, action)
            .map(|cap| row(&cap, color, desc))
    };
    let extend = |col: &mut Vec<Line<'static>>, rows: Vec<Option<Line<'static>>>| {
        col.extend(rows.into_iter().flatten());
    };

    let mut left = vec![head(" Move")];
    extend(
        &mut left,
        vec![
            opt(Action::Down, border, "Down"),
            opt(Action::Up, border, "Up"),
            opt(Action::Top, border, "Top"),
            opt(Action::Bottom, border, "Bottom"),
            opt(Action::PageDown, border, "Page down"),
            opt(Action::PageUp, border, "Page up"),
            opt(Action::NextGroup, teal, "Next group"),
            opt(Action::PrevGroup, teal, "Prev group"),
        ],
    );
    left.push(blank());
    left.push(head(" Filter"));
    extend(
        &mut left,
        vec![
            opt(Action::EnterInsert, green, "Type to filter"),
            opt(Action::ClearQuery, sub, "Clear query"),
            opt(Action::DeleteWord, sub, "Delete word"),
            opt(Action::Backspace, sub, "Delete a char"),
            opt(Action::Help, title, "This help"),
            opt(Action::Quit, red, "Close / quit"),
        ],
    );

    let mut right = vec![head(" Open")];
    extend(
        &mut right,
        vec![
            opt(Action::Accept(Accept::Default), border, "Open"),
            opt(Action::Accept(Accept::Clone), blue, "Clone repo"),
            opt(Action::Accept(Accept::Tab), green, "Open in tab"),
            opt(Action::Accept(Accept::Split), yellow, "Open in split"),
            opt(Action::Accept(Accept::Pane), blue, "cd pane here"),
        ],
    );
    right.push(blank());
    right.push(head(" Manage"));
    extend(
        &mut right,
        vec![
            opt(Action::Accept(Accept::Workspace), mauve, "To workspace"),
            opt(Action::Accept(Accept::Update), teal, "Update repo"),
            opt(Action::Accept(Accept::Remove), red, "Remove"),
        ],
    );
    right.push(blank());
    right.push(head(" View"));
    extend(
        &mut right,
        vec![
            opt(Action::CycleSort, blue, "Cycle sort"),
            opt(Action::TogglePreview, mauve, "Toggle preview"),
            opt(Action::PreviewDown, teal, "Scroll preview"),
        ],
    );
    right.push(row("wheel", teal, "Scroll that pane"));
    right.push(row("click", teal, "Select or run it"));
    right.push(blank());
    right.push(head(" Plugin"));
    extend(
        &mut right,
        vec![
            opt(Action::Settings, teal, "Settings"),
            opt(Action::Changelog, title, "What's new"),
            opt(
                Action::Accept(Accept::UpdatePlugin),
                peach,
                "Update Switchboard",
            ),
        ],
    );

    // Centre a comfortably sized popup within the screen.
    let w = area.width.saturating_sub(6).clamp(40, HELP_W);
    let want_h = left.len().max(right.len()) as u16 + 4;
    let h = want_h.min(area.height.saturating_sub(2)).max(8);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let mode_name = match app.mode {
        Mode::Insert => "INSERT",
        Mode::Normal => "NORMAL",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(ink))
        .title(Span::styled(
            format!("  Keybindings · {mode_name} "),
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(Span::styled(" any key to close ", Style::default().fg(sub)))
                .right_aligned(),
        );
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .horizontal_margin(1)
        .vertical_margin(1)
        .split(inner);
    f.render_widget(Paragraph::new(left), cols[0]);
    f.render_widget(Paragraph::new(right), cols[1]);
}
