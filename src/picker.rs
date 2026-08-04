//! Shared colorful picker engine for Switchboard modes.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::data::Theme;
use crate::query::{CompiledQuery, Document, FieldSchema, QueryDiagnostic};
use crate::tui::{self, Pill};

#[derive(Clone, Debug)]
pub struct PickerItem {
    pub id: String,
    pub primary: String,
    pub secondary: String,
    pub document: Document,
    pub preview: Vec<String>,
    pub accent_slot: Option<String>,
}

#[derive(Clone)]
pub struct ActionSpec {
    pub id: &'static str,
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub key_label: &'static str,
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

pub fn run<M: PickerMode>(mut mode: M, mut theme: Theme, normal: bool) -> Result<()> {
    let schema = mode.schema();
    let mut state = State::new(mode.initial()?, normal);
    state.recompute(&schema);
    loop {
        let mut actions = mode.actions();
        apply_bindings(&mut actions, &mode.key_bindings());
        let mut terminal = crate::init_terminal();
        let outcome: Result<Option<(String, &'static str)>> = (|| loop {
            terminal.draw(|frame| draw(frame, &mode, &theme, &actions, &mut state))?;
            if let Some(snapshot) = mode.poll() {
                match snapshot {
                    Ok(items) => {
                        state.runtime_error = None;
                        state.replace(items, &schema);
                    }
                    Err(error) => state.runtime_error = Some(error.to_string()),
                }
            }
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if state.runtime_error.take().is_some() {
                        continue;
                    }
                    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char(',') {
                        break Ok(Some((String::new(), "__settings")));
                    }
                    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('j') {
                        state.preview_scroll = state.preview_scroll.saturating_add(1);
                        continue;
                    }
                    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('k') {
                        state.preview_scroll = state.preview_scroll.saturating_sub(1);
                        continue;
                    }
                    if let Some(action) = actions.iter().find(|action| action.matches(key)) {
                        if state.diagnostic.is_none() && state.runtime_error.is_none() {
                            if let Some(item) = state.selected_item() {
                                if let Some(reason) =
                                    mode.action_disabled_reason(&item.id, action.id)
                                {
                                    state.runtime_error = Some(reason);
                                    continue;
                                }
                                break Ok(Some((item.id.clone(), action.id)));
                            }
                        }
                        continue;
                    }
                    match state.input_mode {
                        InputMode::Insert => match key.code {
                            KeyCode::Esc => state.input_mode = InputMode::Normal,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break Ok(None)
                            }
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.query.clear();
                                state.recompute(&schema);
                            }
                            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                delete_word(&mut state.query);
                                state.recompute(&schema);
                            }
                            KeyCode::Char(ch)
                                if key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT =>
                            {
                                state.query.push(ch);
                                state.recompute(&schema);
                            }
                            KeyCode::Backspace => {
                                state.query.pop();
                                state.recompute(&schema);
                            }
                            KeyCode::Down => state.move_selection(1),
                            KeyCode::Up => state.move_selection(-1),
                            _ => {}
                        },
                        InputMode::Normal => match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => break Ok(None),
                            KeyCode::Char('i') | KeyCode::Char('/') => {
                                state.input_mode = InputMode::Insert
                            }
                            KeyCode::Char('j') | KeyCode::Down => state.move_selection(1),
                            KeyCode::Char('k') | KeyCode::Up => state.move_selection(-1),
                            KeyCode::Char('g') | KeyCode::Home => state.selected = 0,
                            KeyCode::Char('G') | KeyCode::End => {
                                state.selected = state.filtered.len().saturating_sub(1)
                            }
                            _ => {}
                        },
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollDown => state.move_selection(1),
                    MouseEventKind::ScrollUp => state.move_selection(-1),
                    MouseEventKind::Down(MouseButton::Left)
                        if state.list_area.contains((mouse.column, mouse.row).into()) =>
                    {
                        let relative = mouse.row.saturating_sub(state.list_area.y) as usize;
                        let offset = state.list_state.offset();
                        if offset + relative < state.filtered.len() {
                            state.selected = offset + relative;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        })();
        crate::restore_terminal();
        let Some((item, action)) = outcome? else {
            return Ok(());
        };
        if action == "__settings" {
            let cfg = crate::config::Config::try_load()?;
            crate::settings::main(cfg, Theme::load())?;
            let cfg = crate::config::Config::try_load()?;
            mode.reload_config(&cfg)?;
            state.input_mode = if cfg.common.keymode == "normal" {
                InputMode::Normal
            } else {
                InputMode::Insert
            };
            theme = Theme::load();
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
        if let Some((code, modifiers)) = parse_chord(chord.trim()) {
            action.key = code;
            action.modifiers = modifiers;
            action.key_label = Box::leak(chord.trim().to_string().into_boxed_str());
        }
    }
}

fn parse_chord(chord: &str) -> Option<(KeyCode, KeyModifiers)> {
    let mut modifiers = KeyModifiers::NONE;
    let mut key = None;
    for part in chord.split('-') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" => modifiers.insert(KeyModifiers::CONTROL),
            "alt" => modifiers.insert(KeyModifiers::ALT),
            "shift" => modifiers.insert(KeyModifiers::SHIFT),
            "enter" => key = Some(KeyCode::Enter),
            "tab" => key = Some(KeyCode::Tab),
            "esc" => key = Some(KeyCode::Esc),
            "space" => key = Some(KeyCode::Char(' ')),
            value if value.chars().count() == 1 => key = value.chars().next().map(KeyCode::Char),
            _ => return None,
        }
    }
    key.map(|key| (key, modifiers))
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
    let mode_label = if state.input_mode == InputMode::Insert {
        "INSERT"
    } else {
        "NORMAL"
    };
    let title = if let Some(error) = &state.runtime_error {
        format!(" {} · {} ", mode.title(), error)
    } else if let Some(diagnostic) = &state.diagnostic {
        format!(
            " {} · {} [{}..{}] ",
            mode.title(),
            diagnostic.message,
            diagnostic.span.start,
            diagnostic.span.end
        )
    } else {
        format!(" {} · {} results ", mode.title(), state.filtered.len())
    };
    let search = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(
            if state.diagnostic.is_some() || state.runtime_error.is_some() {
                theme.or("red", Color::Red)
            } else {
                accent
            },
        ))
        .title(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .title(
            Line::from(Span::styled(
                format!(" {mode_label} "),
                Style::default().fg(muted),
            ))
            .right_aligned(),
        );
    frame.render_widget(
        Paragraph::new(format!(" {}", state.query))
            .block(search)
            .style(Style::default().bg(ink).fg(text)),
        rows[0],
    );

    let cols =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(rows[1]);
    state.list_area = Rect::new(
        cols[0].x + 1,
        cols[0].y + 1,
        cols[0].width.saturating_sub(2),
        cols[0].height.saturating_sub(2),
    );
    let items = state
        .filtered
        .iter()
        .map(|index| {
            let item = &state.items[*index];
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {}", item.primary),
                    Style::default().fg(item
                        .accent_slot
                        .as_deref()
                        .map(|slot| theme.or(slot, text))
                        .unwrap_or(text)),
                ),
                Span::styled(format!("  {}", item.secondary), Style::default().fg(muted)),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(accent))
                .title(" List "),
        )
        .highlight_style(
            Style::default()
                .bg(accent)
                .fg(ink)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌");
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
    frame.render_widget(
        Paragraph::new(preview)
            .scroll((state.preview_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent))
                    .title(" Preview "),
            )
            .style(Style::default().bg(ink).fg(text)),
        cols[1],
    );

    let pills = actions
        .iter()
        .filter(|action| {
            state
                .selected_item()
                .is_some_and(|item| mode.action_disabled_reason(&item.id, action.id).is_none())
        })
        .map(|action| {
            Pill::new(
                action.key_label,
                action.label,
                theme.or(action.color_slot, accent),
            )
        })
        .chain(std::iter::once(Pill::new(
            "⌥,",
            "settings",
            theme.or("yellow", Color::Yellow),
        )))
        .chain(std::iter::once(Pill::new(
            "esc",
            "mode/close",
            theme.or("red", Color::Red),
        )))
        .collect::<Vec<_>>();
    let (spans, _) = tui::pill_row(&pills, ink, rows[2].x);
    frame.render_widget(Paragraph::new(Line::from(spans)), rows[2]);
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

    #[test]
    fn snapshot_replacement_preserves_selection_by_identity() {
        let item = |id: &str| PickerItem {
            id: id.into(),
            primary: id.into(),
            secondary: String::new(),
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
            key_label: "↵",
            label: "copy",
            color_slot: "blue",
        }];
        apply_bindings(
            &mut actions,
            &HashMap::from([("copy".into(), "ctrl-y".into())]),
        );
        assert_eq!(actions[0].key, KeyCode::Char('y'));
        assert_eq!(actions[0].modifiers, KeyModifiers::CONTROL);
        assert_eq!(actions[0].key_label, "ctrl-y");
    }
}
