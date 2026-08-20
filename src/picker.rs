//! Shared colorful picker engine for Switchboard modes.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::data::Theme;
use crate::keymap::parse_chord;
use crate::query::{CompiledQuery, Document, FieldSchema, QueryDiagnostic};
use crate::surface::{Surface, Transition};
use crate::tui::{self, Pill};

#[derive(Clone, Debug)]
pub struct PickerItem {
    pub id: String,
    pub primary: String,
    pub secondary: String,
    /// A short tag pinned to the row's right edge — a relative time, a badge.
    /// It gets its own gutter, so put facts here that repeat down the whole list
    /// and would otherwise read as a ragged column of noise.
    pub trailing: Option<String>,
    pub document: Document,
    pub preview: Vec<String>,
    pub accent_slot: Option<String>,
}

#[derive(Clone)]
pub struct ActionSpec {
    pub id: &'static str,
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub key_label: String,
    pub label: &'static str,
    pub color_slot: &'static str,
}

pub enum ActionOutcome {
    Close,
    StayOpen,
}

impl ActionSpec {
    fn matches(&self, key: KeyEvent) -> bool {
        self.key == key.code && self.modifiers == key.modifiers
    }
}

pub trait PickerMode {
    fn title(&self) -> &str;
    fn accent_slot(&self) -> &'static str;
    fn schema(&self) -> FieldSchema;
    fn actions(&self) -> Vec<ActionSpec>;
    fn key_bindings(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn action_disabled_reason(&self, _item_id: &str, _action: &str) -> Option<String> {
        None
    }
    /// Draw each row's leading word in the item's own colour, bold. For a list of
    /// shell commands that word is the program, and it is what the eye hunts for;
    /// for a list of names it would just be a stray colour.
    fn emphasize_head(&self) -> bool {
        false
    }
    /// The list's share of the body, as a percentage. A mode whose rows are long
    /// and whose preview is a short metadata card should claim more of it than the
    /// 42 that suits a card-heavy mode.
    fn list_pct(&self) -> u16 {
        42
    }
    fn reload_config(&mut self, _config: &crate::config::Config) -> Result<()> {
        Ok(())
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>>;
    fn poll(&mut self) -> Option<Result<Vec<PickerItem>>> {
        None
    }
    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome>;
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum InputMode {
    Insert,
    Normal,
}

struct State {
    items: Vec<PickerItem>,
    filtered: Vec<usize>,
    selected: usize,
    selected_id: Option<String>,
    query: String,
    diagnostic: Option<QueryDiagnostic>,
    runtime_error: Option<String>,
    matcher: Matcher,
    input_mode: InputMode,
    list_area: Rect,
    list_state: ListState,
    preview_scroll: u16,
    /// Where the preview card sat at the last draw, so a wheel turn can ask
    /// whether the pointer is over it rather than over the list.
    preview_area: Rect,
    /// The command bar's row and its pills, each carrying what it runs.
    bar_row: u16,
    bar_zones: Vec<(u16, u16, PillAct)>,
}

/// What a command-bar pill does when clicked. `Run` carries the action *id*
/// rather than an index, because the pill list is filtered by
/// `action_disabled_reason` and an index into it is not an index into `actions`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PillAct {
    Run(&'static str),
    Settings,
    Close,
}

impl State {
    fn new(items: Vec<PickerItem>, normal: bool) -> Self {
        let mut state = Self {
            items,
            filtered: Vec::new(),
            selected: 0,
            selected_id: None,
            query: String::new(),
            diagnostic: None,
            runtime_error: None,
            matcher: Matcher::new(MatcherConfig::DEFAULT),
            input_mode: if normal {
                InputMode::Normal
            } else {
                InputMode::Insert
            },
            list_area: Rect::default(),
            list_state: ListState::default(),
            preview_scroll: 0,
            preview_area: Rect::default(),
            bar_row: 0,
            bar_zones: Vec::new(),
        };
        state.recompute(&FieldSchema::default());
        state
    }

    fn replace(&mut self, items: Vec<PickerItem>, schema: &FieldSchema) {
        self.selected_id = self.selected_item().map(|item| item.id.clone());
        self.items = items;
        self.recompute(schema);
        if let Some(id) = &self.selected_id {
            if let Some(position) = self
                .filtered
                .iter()
                .position(|index| self.items[*index].id == *id)
            {
                self.selected = position;
            }
        }
    }

    fn recompute(&mut self, schema: &FieldSchema) {
        self.filtered.clear();
        self.diagnostic = None;
        match CompiledQuery::compile(&self.query, schema) {
            Ok(query) => {
                let mut scored = self
                    .items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        query
                            .score(&item.document, &mut self.matcher)
                            .map(|score| (score, index))
                    })
                    .collect::<Vec<_>>();
                if !self.query.is_empty() {
                    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
                }
                self.filtered = scored.into_iter().map(|(_, index)| index).collect();
            }
            Err(diagnostic) => self.diagnostic = Some(diagnostic),
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        self.preview_scroll = 0;
    }

    fn selected_item(&self) -> Option<&PickerItem> {
        self.filtered
            .get(self.selected)
            .map(|index| &self.items[*index])
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
        self.preview_scroll = 0;
    }
}

/// The caption colour every panel is titled in — `common.title_color` resolved
/// through the herdr theme, exactly as `App::new` resolves it for the projects
/// picker. Read here rather than passed in, so all three modes agree without
/// each caller having to remember to look it up.
fn title_color(theme: &Theme) -> Color {
    crate::config::Config::try_load()
        .ok()
        .and_then(|cfg| theme.resolve(&cfg.common.title_color))
        .unwrap_or_else(|| theme.or("peach", Color::Yellow))
}

enum PickerExit {
    Close,
    Invoke(String, &'static str),
}

struct PickerSurface<'a, M> {
    mode: &'a mut M,
    theme: &'a Theme,
    title: Color,
    actions: &'a [ActionSpec],
    schema: &'a FieldSchema,
    state: &'a mut State,
}

impl<M: PickerMode> Surface for PickerSurface<'_, M> {
    type Output = PickerExit;

    fn draw(&mut self, frame: &mut Frame) {
        draw(
            frame,
            self.mode,
            self.theme,
            self.title,
            self.actions,
            self.state,
        );
    }

    fn tick_rate(&self) -> Duration {
        Duration::from_millis(50)
    }

    fn on_tick(&mut self) -> Result<Transition<Self::Output>> {
        let Some(snapshot) = self.mode.poll() else {
            return Ok(Transition::Wait);
        };
        match snapshot {
            Ok(items) => {
                self.state.runtime_error = None;
                self.state.replace(items, self.schema);
            }
            Err(error) => self.state.runtime_error = Some(error.to_string()),
        }
        Ok(Transition::Redraw)
    }

    fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Mouse(mouse) => {
                let at: Position = (mouse.column, mouse.row).into();
                match mouse.kind {
                    MouseEventKind::ScrollDown if self.state.preview_area.contains(at) => {
                        self.state.preview_scroll = self.state.preview_scroll.saturating_add(3);
                    }
                    MouseEventKind::ScrollUp if self.state.preview_area.contains(at) => {
                        self.state.preview_scroll = self.state.preview_scroll.saturating_sub(3);
                    }
                    MouseEventKind::ScrollDown => self.state.move_selection(1),
                    MouseEventKind::ScrollUp => self.state.move_selection(-1),
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(act) =
                            tui::zone_at(&self.state.bar_zones, self.state.bar_row, at)
                        {
                            return Ok(match act {
                                PillAct::Close => Transition::Exit(PickerExit::Close),
                                PillAct::Settings => Transition::Exit(PickerExit::Invoke(
                                    String::new(),
                                    "__settings",
                                )),
                                PillAct::Run(id) => self.invoke_selected(id),
                            });
                        }
                        if self.state.list_area.contains(at) {
                            let relative =
                                mouse.row.saturating_sub(self.state.list_area.y) as usize;
                            let index = self.state.list_state.offset() + relative;
                            if index < self.state.filtered.len() {
                                if index == self.state.selected {
                                    if let Some(action) =
                                        first_enabled(self.actions, self.mode, self.state)
                                    {
                                        return Ok(self.invoke_selected(action));
                                    }
                                }
                                self.state.selected = index;
                            }
                        }
                    }
                    _ => return Ok(Transition::Wait),
                }
                Ok(Transition::Redraw)
            }
            _ => Ok(Transition::Wait),
        }
    }
}

impl<M: PickerMode> PickerSurface<'_, M> {
    fn invoke_selected(&mut self, action: &'static str) -> Transition<PickerExit> {
        let Some(item) = self.state.selected_item() else {
            return Transition::Wait;
        };
        if let Some(reason) = self.mode.action_disabled_reason(&item.id, action) {
            self.state.runtime_error = Some(reason);
            return Transition::Redraw;
        }
        Transition::Exit(PickerExit::Invoke(item.id.clone(), action))
    }

    fn on_key(&mut self, key: KeyEvent) -> Result<Transition<PickerExit>> {
        if self.state.runtime_error.take().is_some() {
            return Ok(Transition::Redraw);
        }
        if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char(',') {
            return Ok(Transition::Exit(PickerExit::Invoke(
                String::new(),
                "__settings",
            )));
        }
        if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('j') {
            self.state.preview_scroll = self.state.preview_scroll.saturating_add(1);
            return Ok(Transition::Redraw);
        }
        if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('k') {
            self.state.preview_scroll = self.state.preview_scroll.saturating_sub(1);
            return Ok(Transition::Redraw);
        }
        if let Some(action) = self.actions.iter().find(|action| action.matches(key)) {
            if self.state.diagnostic.is_none() {
                return Ok(self.invoke_selected(action.id));
            }
            return Ok(Transition::Wait);
        }

        match self.state.input_mode {
            InputMode::Insert => match key.code {
                KeyCode::Esc => self.state.input_mode = InputMode::Normal,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(Transition::Exit(PickerExit::Close));
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.state.query.clear();
                    self.state.recompute(self.schema);
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    delete_word(&mut self.state.query);
                    self.state.recompute(self.schema);
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.state.query.push(character);
                    self.state.recompute(self.schema);
                }
                KeyCode::Backspace => {
                    self.state.query.pop();
                    self.state.recompute(self.schema);
                }
                KeyCode::Down => self.state.move_selection(1),
                KeyCode::Up => self.state.move_selection(-1),
                _ => return Ok(Transition::Wait),
            },
            InputMode::Normal => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    return Ok(Transition::Exit(PickerExit::Close));
                }
                KeyCode::Char('i') | KeyCode::Char('/') => {
                    self.state.input_mode = InputMode::Insert;
                }
                KeyCode::Char('j') | KeyCode::Down => self.state.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => self.state.move_selection(-1),
                KeyCode::Char('g') | KeyCode::Home => self.state.selected = 0,
                KeyCode::Char('G') | KeyCode::End => {
                    self.state.selected = self.state.filtered.len().saturating_sub(1);
                }
                _ => return Ok(Transition::Wait),
            },
        }
        Ok(Transition::Redraw)
    }
}

pub fn run<M: PickerMode>(mut mode: M, mut theme: Theme, normal: bool) -> Result<()> {
    let schema = mode.schema();
    let mut state = State::new(mode.initial()?, normal);
    state.recompute(&schema);
    let mut title = title_color(&theme);
    loop {
        let mut actions = mode.actions();
        apply_bindings(&mut actions, &mode.key_bindings());
        let outcome = crate::surface::run(&mut PickerSurface {
            mode: &mut mode,
            theme: &theme,
            title,
            actions: &actions,
            schema: &schema,
            state: &mut state,
        })?;
        let PickerExit::Invoke(item, action) = outcome else {
            return Ok(());
        };
        if action == "__settings" {
            let cfg = crate::config::Config::try_load()?;
            crate::settings::main(cfg, Theme::load())?;
            let cfg = crate::config::Config::try_load()?;
            mode.reload_config(&cfg)?;
            state.input_mode = if cfg.common.keymode == crate::config::KeyMode::Normal {
                InputMode::Normal
            } else {
                InputMode::Insert
            };
            theme = Theme::load();
            // `title_color` is one of the settings the overlay writes, so re-resolve
            // it against the reloaded theme rather than keeping the stale colour.
            title = theme
                .resolve(&cfg.common.title_color)
                .unwrap_or_else(|| theme.or("peach", Color::Yellow));
            state.replace(mode.initial()?, &schema);
            continue;
        }
        match mode.execute(&item, action) {
            Ok(ActionOutcome::Close) => return Ok(()),
            Ok(ActionOutcome::StayOpen) => state.replace(mode.initial()?, &schema),
            Err(error) => state.runtime_error = Some(error.to_string()),
        }
    }
}

fn apply_bindings(actions: &mut [ActionSpec], bindings: &HashMap<String, String>) {
    for action in actions {
        let Some(chord) = bindings
            .get(action.id)
            .and_then(|value| value.split(',').next())
        else {
            continue;
        };
        if let Some(parsed) = parse_chord(chord) {
            let (code, modifiers) = parsed.event_parts();
            action.key = code;
            action.modifiers = modifiers;
            action.key_label = parsed.label();
        }
    }
}

fn delete_word(query: &mut String) {
    while query.ends_with(char::is_whitespace) {
        query.pop();
    }
    while query.chars().last().is_some_and(|ch| !ch.is_whitespace()) {
        query.pop();
    }
}

fn draw<M: PickerMode>(
    frame: &mut Frame,
    mode: &M,
    theme: &Theme,
    title_color: Color,
    actions: &[ActionSpec],
    state: &mut State,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);
    let accent = theme.or(mode.accent_slot(), Color::Cyan);
    let ink = theme.or("panel_bg", Color::Black);
    let text = theme.or("text", Color::White);
    let muted = theme.or("subtext0", Color::DarkGray);
    // The projects picker's palette, slot for slot: borders recede in `overlay0`
    // so herdr's own accent pane frame stays the loudest line on screen, and every
    // caption is `title_color`. Nothing here paints a background — the panes are
    // transparent, like the projects picker, so the terminal shows through all
    // three of them instead of two opaque cards beside a see-through list.
    let border = theme.or("overlay0", Color::DarkGray);
    let surface = theme.or("surface1", Color::Indexed(236));

    // Whichever mode owns the keys, tagged the way the projects picker tags it:
    // a bold ink-on-colour chip in the border, not a muted word off to the right.
    let (tag, tag_bg) = match state.input_mode {
        InputMode::Normal => (" NORMAL ", accent),
        InputMode::Insert => (" INSERT ", theme.or("green", Color::Green)),
    };
    // A bad query takes over the caption and reddens the border; the mode title
    // has moved to the list, so there is room to say what is actually wrong.
    let (caption, caption_color) = if let Some(error) = &state.runtime_error {
        (error.clone(), theme.or("red", Color::Red))
    } else if let Some(diagnostic) = &state.diagnostic {
        (
            format!(
                "{} [{}..{}]",
                diagnostic.message, diagnostic.span.start, diagnostic.span.end
            ),
            theme.or("red", Color::Red),
        )
    } else {
        ("Search".into(), title_color)
    };
    let bad = state.diagnostic.is_some() || state.runtime_error.is_some();
    let search = tui::boxed(
        &caption,
        caption_color,
        if bad { caption_color } else { border },
    )
    .title(
        Line::from(Span::styled(
            format!(" {}/{} ", state.filtered.len(), state.items.len()),
            Style::default().fg(muted),
        ))
        .right_aligned(),
    )
    .title(Line::from(Span::styled(
        tag,
        Style::default()
            .bg(tag_bg)
            .fg(ink)
            .add_modifier(Modifier::BOLD),
    )));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  ",
                Style::default().fg(if state.input_mode == InputMode::Normal {
                    muted
                } else {
                    accent
                }),
            ),
            Span::styled(state.query.clone(), Style::default().fg(text)),
        ]))
        .block(search),
        rows[0],
    );

    let list_pct = mode.list_pct();
    let cols = Layout::horizontal([
        Constraint::Percentage(list_pct),
        Constraint::Percentage(100 - list_pct),
    ])
    .split(rows[1]);
    state.list_area = Rect::new(
        cols[0].x + 1,
        cols[0].y + 1,
        cols[0].width.saturating_sub(2),
        cols[0].height.saturating_sub(2),
    );
    // A row is laid out against the panel's real width, not padded to the widest
    // entry: padding to a measured column went ragged the moment one entry ran
    // past the cap, and anything past the border was cut mid-word by the block.
    // So the trailing tag is right-aligned in its own gutter, and what is left is
    // the budget the primary and secondary must fit — with an ellipsis if they
    // don't, which is the difference between "…follows '" and "follows ' \".
    //
    // The width available is the block's inner width less the two columns the
    // highlight symbol reserves on *every* row, selected or not. A row must add no
    // indent of its own on top of that: a single uncounted leading space is enough
    // to push the last character of the gutter under the border.
    let row_width = state.list_area.width.saturating_sub(2) as usize;
    let gutter = state
        .filtered
        .iter()
        .filter_map(|&index| state.items[index].trailing.as_deref())
        .map(|tag| tag.chars().count())
        .max()
        .unwrap_or(0);
    let emphasize = mode.emphasize_head();
    let items = state
        .filtered
        .iter()
        .map(|index| {
            let item = &state.items[*index];
            let slot = item
                .accent_slot
                .as_deref()
                .map(|slot| theme.or(slot, accent));
            // An emphasized head falls back to the mode's accent, never to `muted`:
            // the point of the head is to stand out, and a slotless item drawing it
            // dimmer than its own tail is the opposite of that. The secondary note
            // does fall back to `muted` — being quiet is its job.
            let head_color = slot.unwrap_or(accent);
            let note_color = slot.unwrap_or(muted);
            // Reserve the gutter (plus a space before it) so no row can grow into
            // the column the tags live in.
            let budget = row_width.saturating_sub(if gutter == 0 { 0 } else { gutter + 2 });

            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut used = 0usize;
            let mut push = |text: &str, style: Style, spans: &mut Vec<Span<'static>>| {
                if used >= budget {
                    return;
                }
                let len = text.chars().count();
                if used + len <= budget {
                    used += len;
                    spans.push(Span::styled(text.to_string(), style));
                } else {
                    let room = budget - used;
                    let cut: String = text.chars().take(room.saturating_sub(1)).collect();
                    used = budget;
                    spans.push(Span::styled(format!("{cut}…"), style));
                }
            };

            // For a wall of shell commands the leading word is what the eye hunts
            // for, so a mode can ask for it in its own colour and bold. Everything
            // else stays plain `text`, the projects picker's division of colour.
            match item.primary.split_once(' ').filter(|_| emphasize) {
                Some((head, tail)) => {
                    push(
                        head,
                        Style::default().fg(head_color).add_modifier(Modifier::BOLD),
                        &mut spans,
                    );
                    push(" ", Style::default(), &mut spans);
                    push(tail, Style::default().fg(text), &mut spans);
                }
                None => {
                    let style = if emphasize {
                        Style::default().fg(head_color).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(text)
                    };
                    push(&item.primary, style, &mut spans);
                }
            }
            if !item.secondary.is_empty() {
                push("  ", Style::default(), &mut spans);
                push(
                    &item.secondary,
                    Style::default().fg(note_color).add_modifier(Modifier::DIM),
                    &mut spans,
                );
            }

            // Right-align the tag: pad out to the gutter, then draw it.
            if gutter > 0 {
                let tag = item.trailing.clone().unwrap_or_default();
                let pad = row_width
                    .saturating_sub(used)
                    .saturating_sub(tag.chars().count());
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(
                    tag,
                    Style::default().fg(muted).add_modifier(Modifier::DIM),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(tui::boxed(mode.title(), title_color, border))
        .highlight_style(
            Style::default()
                .fg(accent)
                .bg(surface)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");
    state
        .list_state
        .select((!state.filtered.is_empty()).then_some(state.selected));
    frame.render_stateful_widget(list, cols[0], &mut state.list_state);

    let preview = state
        .selected_item()
        .map(|item| {
            item.preview
                .iter()
                .map(|line| Line::from(line.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                " Waiting for data…",
                Style::default().fg(muted),
            ))]
        });
    // Say where you are in the card only when there is something below the fold,
    // in the projects picker's wording.
    let mut block = tui::boxed("󰈈 Preview", title_color, border);
    let rows_visible = cols[1].height.saturating_sub(2);
    let len = preview.len() as u16;
    state.preview_scroll = state.preview_scroll.min(len.saturating_sub(rows_visible));
    if state.preview_scroll > 0 || len > rows_visible {
        block = block.title(
            Line::from(Span::styled(
                format!(
                    " ⌥jk {}/{len} ",
                    state.preview_scroll + rows_visible.min(len)
                ),
                Style::default().fg(muted),
            ))
            .right_aligned(),
        );
    }
    frame.render_widget(
        Paragraph::new(preview)
            .scroll((state.preview_scroll, 0))
            .block(block)
            .style(Style::default().fg(text)),
        cols[1],
    );

    state.preview_area = cols[1];

    // Each pill beside what it runs, built in the one chain that lays them out —
    // the list is filtered per selection, so a payload paired anywhere else would
    // point at the wrong action the moment a row disables one.
    let caps: Vec<(Pill, PillAct)> = actions
        .iter()
        .filter(|action| {
            state
                .selected_item()
                .is_some_and(|item| mode.action_disabled_reason(&item.id, action.id).is_none())
        })
        .map(|action| {
            (
                Pill::new(
                    &action.key_label,
                    action.label,
                    theme.or(action.color_slot, accent),
                ),
                PillAct::Run(action.id),
            )
        })
        .chain(std::iter::once((
            Pill::new("⌥,", "settings", theme.or("yellow", Color::Yellow)),
            PillAct::Settings,
        )))
        .chain(std::iter::once((
            Pill::new("esc", "mode/close", theme.or("red", Color::Red)),
            PillAct::Close,
        )))
        .collect();
    let pills: Vec<Pill> = caps
        .iter()
        .map(|(p, _)| Pill::new(p.key, p.label, p.color))
        .collect();
    let (spans, zones) = tui::pill_row(&pills, ink, rows[2].x);
    state.bar_row = rows[2].y;
    state.bar_zones = zones
        .into_iter()
        .zip(caps.iter())
        .map(|((a, b), (_, act))| (a, b, *act))
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), rows[2]);
}

/// The primary action for the selected row: the first one the mode does not
/// disable. That is what Enter runs, and so what a click on an already-selected
/// row runs.
fn first_enabled<M: PickerMode>(
    actions: &[ActionSpec],
    mode: &M,
    state: &State,
) -> Option<&'static str> {
    let item = state.selected_item()?;
    actions
        .iter()
        .find(|action| mode.action_disabled_reason(&item.id, action.id).is_none())
        .map(|action| action.id)
}

pub fn fields(pairs: &[(&str, String)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestMode;

    impl PickerMode for TestMode {
        fn title(&self) -> &str {
            "Ports"
        }
        fn accent_slot(&self) -> &'static str {
            "accent"
        }
        fn schema(&self) -> FieldSchema {
            FieldSchema::default()
        }
        fn actions(&self) -> Vec<ActionSpec> {
            Vec::new()
        }
        fn initial(&mut self) -> Result<Vec<PickerItem>> {
            Ok(Vec::new())
        }
        fn execute(&mut self, _item_id: &str, _action: &str) -> Result<ActionOutcome> {
            Ok(ActionOutcome::Close)
        }
    }

    fn render(state: &mut State) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[
            ("accent", "#6fd0a8"),
            ("overlay0", "#6c7e76"),
            ("peach", "#dcbb80"),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &TestMode,
                    &theme,
                    theme.or("peach", Color::Yellow),
                    &[],
                    state,
                )
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn test_item(id: &str) -> PickerItem {
        PickerItem {
            id: id.into(),
            primary: id.into(),
            secondary: format!("{id} detail"),
            trailing: None,
            document: Document {
                fuzzy: id.into(),
                fields: HashMap::new(),
            },
            preview: Vec::new(),
            accent_slot: None,
        }
    }

    /// The mode title belongs to the list, not the search box: herdr already puts
    /// the pane's name on the frame it draws, and captioning the search box with it
    /// too is what showed `Ports` twice, two rows apart.
    #[test]
    fn the_search_box_is_captioned_search_and_the_list_carries_the_mode_title() {
        let mut state = State::new(vec![test_item("a"), test_item("b")], false);
        let buffer = render(&mut state);
        let rows: Vec<String> = (0..12)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect();

        assert!(rows[0].contains(" Search "), "{}", rows[0]);
        assert!(rows[0].contains(" INSERT "), "{}", rows[0]);
        assert!(rows[0].contains(" 2/2 "), "{}", rows[0]);
        assert!(rows[3].contains(" Ports "), "{}", rows[3]);
        assert!(rows[3].contains(" 󰈈 Preview "), "{}", rows[3]);
        assert!(
            !rows[0].contains("Ports"),
            "the mode title must not double the pane frame's: {}",
            rows[0]
        );
    }

    /// Borders recede in `overlay0` and captions are `title_color`, the way the
    /// projects picker paints them — not accent boxes with terminal-default titles.
    #[test]
    fn panels_use_the_projects_pickers_border_and_caption_slots() {
        let mut state = State::new(vec![test_item("a")], false);
        let buffer = render(&mut state);
        let overlay = Color::Rgb(0x6c, 0x7e, 0x76);
        let peach = Color::Rgb(0xdc, 0xbb, 0x80);

        // The list block's top-left corner, and the first cell of its caption.
        assert_eq!(buffer[(0, 3)].fg, overlay, "list border");
        assert_eq!(buffer[(0, 0)].fg, overlay, "search border");
        let caption = (0..80).find(|&x| buffer[(x, 3)].symbol() == "P").unwrap();
        assert_eq!(buffer[(caption, 3)].fg, peach, "list caption");
    }

    /// Nothing paints a background: all three panes are transparent, so the
    /// terminal shows through consistently instead of two opaque cards next to a
    /// see-through list.
    /// A zone that does not sit on the pill it names sends a click to the wrong
    /// action — and does it silently, which is why this is measured rather than
    /// reasoned about.
    #[test]
    fn the_pill_zones_land_on_the_pills_they_name() {
        let mut state = State::new(vec![test_item("a")], false);
        let buffer = render(&mut state);
        let row = state.bar_row;
        assert!(!state.bar_zones.is_empty());
        for &(a, b, act) in &state.bar_zones {
            let text: String = (a..b).map(|x| buffer[(x, row)].symbol()).collect();
            assert!(!text.trim().is_empty(), "zone for a pill covers blanks");
            if act == PillAct::Close {
                assert!(text.contains("esc"), "{text}");
            }
        }
        assert_eq!(state.bar_zones.last().unwrap().2, PillAct::Close);
    }

    /// The preview's rect is what tells a wheel turn whether it is over the card
    /// or over the list. It is published by the draw for exactly that reason.
    #[test]
    fn the_draw_publishes_where_the_preview_landed() {
        let mut state = State::new(vec![test_item("a")], false);
        let _ = render(&mut state);
        assert!(
            state.preview_area.width > 0,
            "the preview pane was measured"
        );
        assert!(
            state.preview_area.x > state.list_area.x,
            "the card sits right of the list"
        );
        assert!(!state
            .preview_area
            .contains((state.list_area.x + 1, state.list_area.y + 1).into()));
    }

    #[test]
    fn no_panel_paints_an_opaque_background() {
        let mut state = State::new(vec![test_item("a"), test_item("b")], false);
        let buffer = render(&mut state);
        // Rows 0-2 are the search box (its border carries the mode chip), row 4 is
        // the selected list row (the highlight bar), row 11 is the pill bar. Every
        // other cell — including the unselected rows and the whole preview — must
        // be transparent, which is what the opaque `panel_bg` cards got wrong.
        for y in [3, 5, 6, 7, 8, 9, 10] {
            for x in 0..80 {
                assert_eq!(
                    buffer[(x, y)].bg,
                    Color::Reset,
                    "cell ({x},{y}) painted a background"
                );
            }
        }
        // The selection bar is the one thing that does paint, in `surface1`.
        assert_eq!(buffer[(2, 4)].bg, Color::Indexed(236), "selection bar");
    }

    struct WideMode(bool);

    impl PickerMode for WideMode {
        fn title(&self) -> &str {
            "Commands"
        }
        fn accent_slot(&self) -> &'static str {
            "accent"
        }
        fn emphasize_head(&self) -> bool {
            self.0
        }
        fn list_pct(&self) -> u16 {
            58
        }
        fn schema(&self) -> FieldSchema {
            FieldSchema::default()
        }
        fn actions(&self) -> Vec<ActionSpec> {
            Vec::new()
        }
        fn initial(&mut self) -> Result<Vec<PickerItem>> {
            Ok(Vec::new())
        }
        fn execute(&mut self, _item_id: &str, _action: &str) -> Result<ActionOutcome> {
            Ok(ActionOutcome::Close)
        }
    }

    fn render_wide(state: &mut State, emphasize: bool) -> Vec<String> {
        let theme = Theme::from_slots(&[("accent", "#6fd0a8"), ("overlay0", "#6c7e76")]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|f| draw(f, &WideMode(emphasize), &theme, Color::Yellow, &[], state))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..10)
            .map(|y| (0..60).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    fn tagged(id: &str, primary: &str, tag: &str) -> PickerItem {
        PickerItem {
            id: id.into(),
            primary: primary.into(),
            secondary: String::new(),
            trailing: Some(tag.into()),
            document: Document {
                fuzzy: primary.into(),
                fields: HashMap::new(),
            },
            preview: Vec::new(),
            accent_slot: None,
        }
    }

    /// A row longer than the panel used to be cut wherever the border fell, mid
    /// word and with no sign it had been cut. It must end in an ellipsis instead,
    /// inside the border.
    #[test]
    fn an_overlong_row_is_truncated_with_an_ellipsis_not_clipped_by_the_border() {
        let long = "cd '/Users/caongoccuong/Developments/github.com/crafts69guy/herdr-ghq'";
        let mut state = State::new(vec![tagged("a", long, "2h")], false);
        let rows = render_wide(&mut state, false);
        let row = &rows[4];

        assert!(row.contains('…'), "expected an ellipsis in {row:?}");
        // The block's right border survives, so nothing overran the panel.
        assert!(row.ends_with('│'), "{row:?}");
        assert!(
            !row.contains("herdr-ghq"),
            "the tail should be cut: {row:?}"
        );
    }

    /// Every tag lands in one column at the right edge, whatever the rows in front
    /// of them do — the ragged `shell` column was the whole complaint.
    #[test]
    fn trailing_tags_line_up_in_one_right_hand_gutter() {
        let mut state = State::new(
            vec![
                tagged("a", "vim", "2h"),
                tagged("b", "docker compose -f compose.dev.yaml up -d", "3d"),
                tagged("c", "g st", "12mo"),
            ],
            false,
        );
        let rows = render_wide(&mut state, false);
        // In *columns*, not bytes: the box-drawing border is three bytes wide, so
        // a byte offset would compare the wrong things.
        let end_column = |row: &str, tag: &str| {
            let cells: Vec<char> = row.chars().collect();
            let want: Vec<char> = tag.chars().collect();
            cells
                .windows(want.len())
                .position(|w| w == want.as_slice())
                .map(|i| i + want.len())
                .unwrap_or_else(|| panic!("{tag} in {row:?}"))
        };

        // Right-aligned means the tags share an *end* column, not a start column.
        let ends: Vec<usize> = [(4, "2h"), (5, "3d"), (6, "12mo")]
            .iter()
            .map(|(y, tag)| end_column(&rows[*y], tag))
            .collect();
        assert_eq!(ends[0], ends[1], "{rows:#?}");
        assert_eq!(ends[1], ends[2], "{rows:#?}");
    }

    /// The leading word is the program, and a mode can ask for it in its own
    /// colour so a wall of commands has something to scan down.
    #[test]
    fn the_head_is_emphasized_only_when_the_mode_asks_for_it() {
        let theme_accent = Color::Rgb(0x6f, 0xd0, 0xa8);
        let plain = |emphasize: bool| {
            // Two items, and probe the *second*: `highlight_style` repaints the
            // whole selected row, so the first one would report the selection's
            // colour no matter what the head is styled with.
            let mut state = State::new(
                vec![
                    tagged("a", "vim", "2h"),
                    tagged("b", "docker compose down", "3d"),
                ],
                false,
            );
            let t = Theme::from_slots(&[("accent", "#6fd0a8"), ("overlay0", "#6c7e76")]);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
            terminal
                .draw(|f| draw(f, &WideMode(emphasize), &t, Color::Yellow, &[], &mut state))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            // Column 3 is the first letter of `docker`: past the border and the two
            // columns the highlight symbol reserves. Row 5 is the second entry.
            let cell = buffer[(3, 5)].clone();
            (cell.symbol().to_string(), cell.fg, cell.modifier)
        };

        let (symbol, fg, modifier) = plain(true);
        assert_eq!(symbol, "d");
        assert_eq!(fg, theme_accent, "the head takes the item's slot");
        assert!(modifier.contains(Modifier::BOLD));

        let (_, fg, _) = plain(false);
        assert_ne!(fg, theme_accent, "a plain mode leaves the head alone");
    }

    #[test]
    fn snapshot_replacement_preserves_selection_by_identity() {
        let item = |id: &str| PickerItem {
            id: id.into(),
            primary: id.into(),
            secondary: String::new(),
            trailing: None,
            document: Document {
                fuzzy: id.into(),
                fields: HashMap::new(),
            },
            preview: Vec::new(),
            accent_slot: None,
        };
        let mut state = State::new(vec![item("a"), item("b")], false);
        state.selected = 1;
        state.replace(vec![item("b"), item("c")], &FieldSchema::default());
        assert_eq!(
            state.selected_item().map(|item| item.id.as_str()),
            Some("b")
        );
    }

    #[test]
    fn picker_action_binding_is_mode_scoped_and_updates_footer_label() {
        let mut actions = vec![ActionSpec {
            id: "copy",
            key: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            key_label: "↵".into(),
            label: "copy",
            color_slot: "blue",
        }];
        apply_bindings(
            &mut actions,
            &HashMap::from([("copy".into(), "ctrl-y".into())]),
        );
        assert_eq!(actions[0].key, KeyCode::Char('y'));
        assert_eq!(actions[0].modifiers, KeyModifiers::CONTROL);
        assert_eq!(actions[0].key_label, "^y");
    }
}
