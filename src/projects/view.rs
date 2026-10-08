//! Rendering: Search input (top), Switcher list (middle), Preview (below), and
//! a full-width colourful command bar pinned to the very bottom.

use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use crate::action::Accept;
use crate::data::{GroupFilter, Kind};
use crate::keymap::{Action, Mode};
use crate::projects::App;

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    app.background.paint(f, f.area());
    let accent = app.theme.or("accent", Color::Cyan);
    let text = app.theme.or("text", Color::Reset);
    let sub = app.theme.or("subtext0", Color::DarkGray);
    let overlay = app.theme.or("overlay0", Color::DarkGray);
    let surface = app.theme.or("surface1", Color::Indexed(236));

    // The command bar is taken off the bottom before anything else is laid out.
    // As a trailing `Constraint::Length(1)` it was the first thing ratatui gave
    // up on a short pane — a `Min` outranks a `Length` a hundred to one — so the
    // one row telling the user how to leave went before the list lost anything.
    let (top, footer) = crate::tui::reserve_bar(f.area(), 1);
    let root = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(top);

    // Body: list + preview. The footer is always a separate full-width row, so
    // the preview can sit on any side without shrinking the command bar.
    let body = root[1];
    let (context_area, content) = if body.width >= 120 {
        let columns = Layout::horizontal([Constraint::Length(22), Constraint::Min(40)]).split(body);
        (Some(columns[0]), columns[1])
    } else {
        (None, body)
    };
    let show_preview = app.preview.enabled && body.width >= 80;
    let (list_area, preview_area) = if show_preview {
        let pct = app.preview.pct;
        let rest = 100u16.saturating_sub(pct);
        match app.preview.position.as_str() {
            "right" => {
                let c =
                    Layout::horizontal([Constraint::Percentage(rest), Constraint::Percentage(pct)])
                        .split(content);
                (c[0], Some(c[1]))
            }
            "left" => {
                let c =
                    Layout::horizontal([Constraint::Percentage(pct), Constraint::Percentage(rest)])
                        .split(content);
                (c[1], Some(c[0]))
            }
            "up" => {
                let c =
                    Layout::vertical([Constraint::Percentage(pct), Constraint::Percentage(rest)])
                        .split(content);
                (c[1], Some(c[0]))
            }
            _ => {
                let c =
                    Layout::vertical([Constraint::Percentage(rest), Constraint::Percentage(pct)])
                        .split(content);
                (c[0], Some(c[1]))
            }
        }
    } else {
        (content, None)
    };

    let title = app.title_color;
    draw_input(f, app, root[0], title, accent, sub, overlay);
    if let Some(area) = context_area {
        draw_context(f, app, area, title, text, overlay, surface);
    }
    draw_list(
        f,
        app,
        list_area,
        title,
        accent,
        text,
        overlay,
        surface,
        context_area.is_none(),
    );
    if let Some(area) = preview_area {
        // Publish where the pane landed: the next render request clips the card
        // to its width, the scroll clamps to its height, and a wheel turn asks
        // whether the pointer is inside it. A resize therefore reaches the card
        // on the next request, not this frame — the shown card keeps the width
        // it was built at until the selection moves.
        app.preview.area = Some(area);
        draw_preview(f, app, area, title, overlay);
    } else {
        app.preview.area = None;
    }
    draw_footer(f, app, footer);

    match app.overlay {
        super::Overlay::Clone => super::clone::draw(f, app, f.area()),
        super::Overlay::Changelog => draw_changelog(f, app, f.area()),
        super::Overlay::Settings => crate::settings::draw(
            f,
            f.area(),
            &app.theme,
            app.background,
            app.title_color,
            &mut app.settings,
        ),
        super::Overlay::Help => draw_help(f, app, f.area()),
        super::Overlay::Handoff => draw_handoff(f, app, f.area()),
        super::Overlay::Removal => draw_removal(f, app, f.area()),
        super::Overlay::None => {}
    }
}

fn draw_removal(f: &mut Frame, app: &mut App, area: Rect) {
    use super::removal::Control;

    let title = app.title_color;
    let border = app.theme.or("overlay0", Color::DarkGray);
    let text = app.theme.or("text", Color::Reset);
    let red = app.theme.or("red", Color::Red);
    let sub = app.theme.or("subtext0", Color::DarkGray);
    let popup = crate::tui::centered(area, 92, 21);
    app.background.paint(f, popup);
    let outer = crate::tui::boxed("Remove worktree", title, border);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);
    let (body, bar) = crate::tui::reserve_bar(inner, 1);
    let (body, feedback) = crate::tui::reserve_bar(body, 3);
    let state = &mut app.removal;
    state.zones.clear();
    let entry = state.entry.as_ref();
    let branch = state.snapshot.as_ref().and_then(|s| s.branch.as_deref());
    let condition = match state.snapshot.as_ref() {
        Some(snapshot) if snapshot.dirty => "Uncommitted or untracked changes",
        Some(_) => "Clean worktree",
        None => "Not yet checked",
    };
    let lines = vec![
        Line::from(format!("Name: {}", entry.map_or("", |e| e.label.as_str()))),
        Line::from(format!(
            "Path: {}",
            entry.and_then(|e| e.dir.as_deref()).unwrap_or("")
        )),
        Line::from(format!(
            "Branch: {}",
            branch.unwrap_or("detached / not yet checked")
        )),
        Line::from(condition),
        Line::from(""),
        Line::from("Deletes the checkout, including ignored files."),
        Line::from("Running panes and agents may be affected; they stay open."),
        Line::from(if state.force {
            "WARNING: Force discards all local changes."
        } else {
            "Git refuses dirty worktrees unless Force is enabled."
        }),
        Line::from(""),
        Line::from(format!("Type exactly: {}", state.expected())),
        Line::from(format!(
            "{} Confirm: {}▏",
            if state.focus == 0 { "›" } else { " " },
            state.confirmation
        )),
        Line::from(format!(
            "{} [{}] Force removal",
            if state.focus == 1 { "›" } else { " " },
            if state.force { "x" } else { " " }
        )),
        Line::from(format!(
            "{} [{}] Delete branch (merged only){}",
            if state.focus == 2 { "›" } else { " " },
            if state.delete_branch { "x" } else { " " },
            if branch.is_none() {
                " — unavailable"
            } else {
                ""
            }
        )),
        Line::from("Tab: next field   Space: toggle checkbox"),
    ];
    f.render_widget(Paragraph::new(lines).style(Style::default().fg(text)), body);
    for (row, control) in [
        (10, Control::Input),
        (11, Control::Force),
        (12, Control::Branch),
    ] {
        if row < body.height {
            state
                .zones
                .push((Rect::new(body.x, body.y + row, body.width, 1), control));
        }
    }
    let message = state.error.as_deref().or(state.status).unwrap_or("");
    f.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(if state.error.is_some() { red } else { sub }))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        feedback,
    );
    let ink = app.theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let pills = [
        crate::tui::Pill {
            key: "enter",
            label: "remove",
            color: if state.confirmed() { red } else { border },
        },
        crate::tui::Pill {
            key: "esc",
            label: "cancel",
            color: sub,
        },
    ];
    let (spans, zones) = crate::tui::pill_row(&pills, ink, bar.x);
    f.render_widget(Paragraph::new(Line::from(spans)), bar);
    if bar.height > 0 {
        for ((start, end), control) in zones.into_iter().zip([Control::Confirm, Control::Cancel]) {
            let start = start.min(bar.right());
            let end = end.min(bar.right());
            state
                .zones
                .push((Rect::new(start, bar.y, end - start, 1), control));
        }
    }
}

fn draw_handoff(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let title = app.title_color;
    let text = t.or("text", Color::Reset);
    let sub = t.or("subtext0", Color::DarkGray);
    let border = t.or("overlay0", Color::DarkGray);
    let accent = t.or("accent", Color::Cyan);
    let red = t.or("red", Color::Red);
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));

    let width = area.width.saturating_sub(10).clamp(48, 92);
    let height = area.height.saturating_sub(6).clamp(10, 24);
    let popup = crate::tui::centered(area, width, height);
    app.background.paint(f, popup);

    let scope = match app.handoff.scope {
        Some(super::TargetScope::SameWorktree) => "same worktree",
        Some(super::TargetScope::SameDirectory) => "same directory",
        Some(super::TargetScope::AllAgents) => "all running",
        None => "resolving",
    };
    let outer = crate::tui::framed(accent)
        .title(Span::styled(
            " Send path to agent ",
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(Span::styled(format!(" {scope} "), Style::default().fg(sub)))
                .right_aligned(),
        );
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);
    // Bottom-up, in the order the rows may be given up: the pills, then the
    // feedback line, then the search line and the list. Trailing them as
    // `Length`s behind the list's `Min` reversed that — the bar went first.
    let (head, bar_area) = crate::tui::reserve_bar(inner, 1);
    let (head, status_area) = crate::tui::reserve_bar(head, 1);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(head);

    let query = if app.handoff.query.is_empty() {
        "type to filter".to_string()
    } else {
        format!("{}▏", app.handoff.query)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Search  ", Style::default().fg(title)),
            Span::styled(query, Style::default().fg(text)),
        ])),
        rows[0],
    );

    app.handoff.list_area = rows[1];
    app.handoff
        .list_state
        .select((!app.handoff.filtered.is_empty()).then_some(app.handoff.selected));
    let items: Vec<ListItem> = if app.handoff.filtered.is_empty() {
        let empty = if app.handoff.status.is_some() {
            "  finding promptable agents…"
        } else {
            "  no promptable agents; blocked agents cannot receive prompts"
        };
        vec![ListItem::new(Line::from(Span::styled(
            empty,
            Style::default().fg(sub),
        )))]
    } else {
        app.handoff
            .filtered
            .iter()
            .filter_map(|&index| app.handoff.targets.get(index))
            .map(|target| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:<12} ", target.agent),
                        Style::default().fg(text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{:<9} ", target.status), Style::default().fg(sub)),
                    Span::styled(target.cwd.clone(), Style::default().fg(sub)),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(crate::tui::boxed("Agents", title, border))
        .highlight_symbol("▌")
        .highlight_style(Style::default().fg(title));
    f.render_stateful_widget(list, rows[1], &mut app.handoff.list_state);

    let feedback = app
        .handoff
        .error
        .as_ref()
        .map(|message| (message.as_str(), red))
        .or_else(|| {
            app.handoff
                .status
                .as_ref()
                .map(|message| (message.as_str(), sub))
        });
    if let Some((message, color)) = feedback {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(color),
            ))),
            status_area,
        );
    }

    let pills = [
        crate::tui::Pill::new("↵", "send", t.or("green", Color::Green)),
        crate::tui::Pill::new("esc", "back", red),
    ];
    let (spans, zones) = crate::tui::pill_row(&pills, ink, bar_area.x);
    app.handoff.footer_row = bar_area.y;
    app.handoff.footer_zones = if bar_area.height == 0 {
        Vec::new()
    } else {
        zones
            .into_iter()
            .zip([super::HandoffAction::Send, super::HandoffAction::Back])
            .map(|((start, end), action)| (start, end, action))
            .collect()
    };
    f.render_widget(Paragraph::new(Line::from(spans)), bar_area);
}

fn draw_context(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    title: Color,
    text: Color,
    border: Color,
    surface: Color,
) {
    let mut lines = Vec::new();
    let mut zones = Vec::new();
    for (row, group) in app.picker.tabs().into_iter().enumerate() {
        let selected = group == app.picker.group;
        let count = app.picker.group_count(group);
        let style = if selected {
            Style::default()
                .fg(title)
                .bg(surface)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text)
        };
        lines.push(Line::from(Span::styled(
            format!(" {:<12} {:>3} ", group.label(), count),
            style,
        )));
        zones.push((
            Rect::new(
                area.x + 1,
                area.y + 1 + row as u16,
                area.width.saturating_sub(2),
                1,
            ),
            group,
        ));
    }
    app.zones.tab_zones = zones;
    let block = crate::tui::boxed("Context", title, border).title(
        Line::from(Span::styled(
            format!(" {} ", app.picker.sort.label()),
            Style::default().fg(border),
        ))
        .right_aligned(),
    );
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The changelog, over the list rather than instead of it: reading what changed should
/// not cost you your place. Same parser and renderer as the standalone `--changelog`
/// pane, so the two cannot drift.
fn draw_changelog(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let sub = t.or("subtext0", Color::Gray);
    let border = t.or("accent", Color::Cyan);
    let title = app.title_color;

    let w = area.width.saturating_sub(8).clamp(48, 84);
    let h = area.height.saturating_sub(4).clamp(8, 32);
    let popup = crate::tui::centered(area, w, h);
    app.background.paint(f, popup);

    let block = crate::tui::framed(border)
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
    let count = match app.catalog {
        super::CatalogState::Loading => " Standing by… ".to_string(),
        super::CatalogState::Refreshing => " Refreshing… ".to_string(),
        super::CatalogState::Failed(_) => " Unavailable ".to_string(),
        super::CatalogState::Ready => format!(
            " {}/{} ",
            app.picker.filtered.len(),
            app.picker.entries.len()
        ),
    };
    // Which mode owns the keys — a vimmer's `-- INSERT --`. Normal is always one
    // Esc away, so the tag is always shown.
    let ink = app.theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let (tag, bg) = match app.mode {
        Mode::Normal => (" NORMAL ", accent),
        Mode::Insert => (" INSERT ", app.theme.or("green", Color::Green)),
    };
    let (caption, caption_color) = app
        .feedback
        .as_ref()
        .map(|message| (format!(" {message} "), app.theme.or("red", Color::Red)))
        .unwrap_or((count, sub));
    let block = crate::tui::boxed("Search", title, border)
        .title(
            Line::from(Span::styled(caption, Style::default().fg(caption_color))).right_aligned(),
        )
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
    if matches!(
        app.catalog,
        super::CatalogState::Ready | super::CatalogState::Refreshing
    ) {
        // Cursor after the prompt + query.
        let cx = inner.x + 2 + app.picker.query.chars().count() as u16;
        f.set_cursor_position(Position::new(
            cx.min(inner.x + inner.width.saturating_sub(1)),
            inner.y,
        ));
    }
}

/// The Navigator's fixed primary column.
///
/// The column is padded with [`crate::tui::spaces`] rather than by growing a
/// clone of the entry's own text: two adjacent spans that share one style paint
/// exactly the cells one pre-padded span did, and neither of them allocates.
pub(super) const PRIMARY_WIDTH: usize = 38;

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
    show_tabs: bool,
) {
    // Split `app` into disjoint field borrows up front. Each row's text is
    // borrowed straight out of `picker.entries` instead of cloning the icon,
    // primary, and secondary columns per row per frame — and that borrow has to
    // stay alive across the zone write-back below, which is only possible
    // because the two touch different fields of `App`.
    let App {
        picker,
        theme,
        catalog,
        zones,
        ..
    } = app;
    let star_color = theme.or("peach", Color::Yellow);

    let items: Vec<ListItem> = if picker.filtered.is_empty()
        && picker.group == crate::data::GroupFilter::Starred
        && picker.query.is_empty()
    {
        vec![ListItem::new(Line::from(Span::styled(
            "  No starred repos or worktrees yet",
            Style::default().fg(border).add_modifier(Modifier::DIM),
        )))]
    } else {
        picker
            .filtered
            .iter()
            .enumerate()
            .map(|(visible_index, &i)| {
                let e = &picker.entries[i];
                let selected = visible_index == picker.selected;
                let primary_style = Style::default().fg(if selected { accent } else { text });
                let pad = PRIMARY_WIDTH.saturating_sub(e.primary.chars().count());
                // The secondary column carries the entry's own colour (host tint
                // for repos, live state for agents, accent for workspaces) so the
                // list reads as colourful at a glance instead of a wall of grey.
                ListItem::new(Line::from(vec![
                    Span::styled(
                        e.icon.as_str(),
                        Style::default().fg(if selected { accent } else { e.icon_color }),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        if picker.is_starred(e) { "★" } else { " " },
                        Style::default().fg(star_color),
                    ),
                    Span::raw(" "),
                    Span::styled(e.primary.as_str(), primary_style),
                    Span::styled(crate::tui::spaces(pad), primary_style),
                    Span::raw(" "),
                    Span::styled(
                        e.secondary.as_str(),
                        Style::default()
                            .fg(if selected { accent } else { e.icon_color })
                            .add_modifier(Modifier::DIM),
                    ),
                ]))
            })
            .collect()
    };

    // Title row = a group tab strip (All + each present kind) with the active
    // tab highlighted, plus a right-aligned sort indicator when both fit.
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    // `framed` rather than `boxed`: this panel's caption slot is the tab strip,
    // a multi-span line, not a word — but the frame itself must still be the
    // shared one, or it drifts the way the git card did.
    let catalog_unavailable = matches!(
        *catalog,
        super::CatalogState::Loading | super::CatalogState::Failed(_)
    );
    let block = if show_tabs && !catalog_unavailable {
        let groups = picker.tabs();
        let available = area.width.saturating_sub(2);
        let full = tab_labels(&groups, TabDensity::Full);
        let labels = if tab_row_width(&full) <= available {
            full
        } else {
            let compact = tab_labels(&groups, TabDensity::Compact);
            if tab_row_width(&compact) <= available {
                compact
            } else {
                tab_labels(&groups, TabDensity::Minimal)
            }
        };
        let sort_text = format!(" sort: {} ", picker.sort.label());
        let show_sort =
            tab_row_width(&labels).saturating_add(sort_text.chars().count() as u16) <= available;

        // A tab's click zone is measured in the loop that lays it out: the two
        // cannot drift, because there is only one place that decides where a tab is.
        // Titles start one column in, past the block's corner.
        let mut x = area.x + 1;
        let mut tab_zones = Vec::new();
        let mut tab_spans = Vec::new();
        for (group, label) in groups.into_iter().zip(labels) {
            let style = if group == picker.group {
                Style::default()
                    .fg(ink)
                    .bg(title)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(border)
            };
            let width = label.chars().count() as u16;
            tab_zones.push((Rect::new(x, area.y, width, 1), group));
            x += width + 1;
            tab_spans.push(Span::styled(label, style));
            tab_spans.push(Span::raw(" "));
        }
        zones.tab_zones = tab_zones;
        let block = crate::tui::framed(border).title(Line::from(tab_spans));
        if show_sort {
            block.title(
                Line::from(Span::styled(sort_text, Style::default().fg(border))).right_aligned(),
            )
        } else {
            block
        }
    } else {
        // Wide layouts publish their visible Context row zones in draw_context;
        // only clear stale Navigator-title zones when this layout owns them.
        if show_tabs {
            zones.tab_zones.clear();
        }
        crate::tui::framed(border).title(Span::styled(
            " Navigator ",
            Style::default().fg(title).add_modifier(Modifier::BOLD),
        ))
    };

    if let super::CatalogState::Loading | super::CatalogState::Failed(_) = &*catalog {
        zones.list_state.select(None);
        zones.list_area = area;
        f.render_widget(block, area);

        let (headline, detail) = match &*catalog {
            super::CatalogState::Loading => (
                "Standing by…",
                "Loading agents, workspaces, repositories, and worktrees",
            ),
            super::CatalogState::Failed(message) => ("Could not load projects", message.as_str()),
            _ => unreachable!("catalog branch is guarded"),
        };
        let inner = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        let top = inner.height.saturating_sub(2) / 2;
        let mut lines = vec![Line::raw(""); top as usize];
        lines.extend([
            Line::from(Span::styled(
                headline,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                detail.to_string(),
                Style::default().fg(border),
            )),
        ]);
        let status = Text::from(lines);
        f.render_widget(Paragraph::new(status).alignment(Alignment::Center), inner);
        return;
    }

    let list = List::new(items)
        .block(block)
        .highlight_symbol("▌ ")
        .highlight_style(Style::default().bg(surface).add_modifier(Modifier::BOLD));

    // The state carries the scroll offset between frames — a click can only be
    // turned back into an entry if we know which row was showing first. It also
    // means the list keeps its scroll position instead of re-deriving it from
    // the top on every frame.
    let selected = (!picker.filtered.is_empty()).then_some(picker.selected);
    zones.list_state.select(selected);
    zones.list_area = area;
    f.render_stateful_widget(list, area, &mut zones.list_state);
}

#[derive(Clone, Copy)]
enum TabDensity {
    Full,
    Compact,
    Minimal,
}

fn tab_labels(groups: &[GroupFilter], density: TabDensity) -> Vec<String> {
    groups
        .iter()
        .map(|&group| {
            let label = match density {
                TabDensity::Full => group.label(),
                TabDensity::Compact | TabDensity::Minimal => match group {
                    GroupFilter::All => "All",
                    GroupFilter::Only(Kind::Agent) => "A",
                    GroupFilter::Only(Kind::Workspace) => "W",
                    GroupFilter::Only(Kind::Repo) => "R",
                    GroupFilter::Only(Kind::Worktree) => "T",
                    GroupFilter::Starred => "★",
                },
            };
            match density {
                TabDensity::Full | TabDensity::Compact => format!(" {label} "),
                TabDensity::Minimal => label.to_string(),
            }
        })
        .collect()
}

fn tab_row_width(labels: &[String]) -> u16 {
    labels.iter().fold(0, |width, label| {
        width.saturating_add(label.chars().count() as u16 + 1)
    })
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
    // A row that was never painted must claim no clicks: `zone_at` matches on a
    // published row coordinate alone, so a zero-height bar that still published
    // one would answer for whatever the list drew there instead.
    if area.height == 0 {
        app.zones.footer_zones.clear();
        return;
    }
    let t = &app.theme;
    // Dark ink for text sitting on the coloured pills.
    let ink = t.or("panel_bg", Color::Rgb(16, 18, 20));
    if matches!(
        app.catalog,
        super::CatalogState::Loading | super::CatalogState::Failed(_)
    ) {
        let red = t.or("red", Color::Red);
        let cap = app
            .keymap
            .label_for(app.mode, Action::Quit)
            .unwrap_or_else(|| "esc".to_string());
        let pills = [crate::tui::Pill::new(&cap, "close", red)];
        let (spans, zones) = crate::tui::pill_row(&pills, ink, area.x);
        app.zones.footer_zones = zones
            .into_iter()
            .map(|(start, end)| (start, end, Action::Quit))
            .collect();
        app.zones.footer_row = area.y;
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    // The bar's order, colour, and short label are fixed; the key cap is read
    // from the keymap for the *current mode*, so a remap or an Insert↔Normal
    // switch re-labels every pill (e.g. `update` shows `^r` in Insert, `␣u` in
    // Normal). An action with no binding in this mode drops out of the bar.
    let star_label = app
        .picker
        .selected_entry()
        .filter(|entry| app.picker.is_starred(entry))
        .map(|_| "unstar")
        .unwrap_or("star");
    let items: Vec<(Action, &str, Color)> = vec![
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
        (Action::CopyPath, "copy", t.or("peach", Color::Yellow)),
        (Action::SendToAgent, "send", t.or("green", Color::Green)),
        (Action::ToggleStar, star_label, t.or("peach", Color::Yellow)),
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
            opt(Action::CopyPath, peach, "Copy path"),
            opt(Action::SendToAgent, green, "Send to agent"),
        ],
    );
    right.push(blank());
    right.push(head(" Manage"));
    extend(
        &mut right,
        vec![
            opt(Action::Accept(Accept::Workspace), mauve, "To workspace"),
            opt(Action::ToggleStar, peach, "Star / unstar"),
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

    // Centre a comfortably sized popup within the screen. The floors are what
    // the card wants, never what it takes: `centered` still clamps to the frame.
    let w = area.width.saturating_sub(6).clamp(40, HELP_W);
    let want_h = left.len().max(right.len()) as u16 + 4;
    let h = want_h.min(area.height.saturating_sub(2)).max(8);
    let popup = crate::tui::centered(area, w, h);

    app.background.paint(f, popup);

    let mode_name = match app.mode {
        Mode::Insert => "INSERT",
        Mode::Normal => "NORMAL",
    };
    let block = crate::tui::framed(border)
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
