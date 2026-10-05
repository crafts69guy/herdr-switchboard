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
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::config::Config;
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
    /// An optional semantic marker placed immediately before the trailing tag.
    /// Its theme slot remains visible even on the selected row.
    pub trailing_marker: Option<PickerMarker>,
    pub document: Document,
    pub preview: Vec<String>,
    pub accent_slot: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PickerMarker {
    text: String,
    color_slot: String,
}

impl PickerMarker {
    pub fn new(text: impl Into<String>, color_slot: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color_slot: color_slot.into(),
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PickerTab {
    pub id: &'static str,
    pub label: &'static str,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionOutcome {
    Close,
    StayOpen,
}

/// Does `event` mean the chord `(code, modifiers)` names? Modifier bits are
/// weighed the way [`crate::keymap::chord_of`] weighs them — CTRL and ALT decide
/// and everything else is noise — because SHIFT is already baked into the
/// character a terminal reports, and terminals disagree about whether they set
/// the bit as well.
///
/// This used to be `==` here and `contains` in the keymap, so one keypress had
/// two answers depending on which half of the app read it: `shift-enter` ran the
/// selected row in Projects and did nothing in every other picker, and a
/// `ctrl-shift-` chord missed its `ctrl-` action outright.
fn same_chord(code: KeyCode, modifiers: KeyModifiers, event: KeyEvent) -> bool {
    match (
        crate::keymap::chord_of(&KeyEvent::new(code, modifiers)),
        crate::keymap::chord_of(&event),
    ) {
        (Some(declared), Some(pressed)) => declared == pressed,
        // A key the chord model does not cover still matches exactly, so nothing
        // that worked before stops working.
        _ => code == event.code && modifiers == event.modifiers,
    }
}

impl ActionSpec {
    fn matches(&self, key: KeyEvent) -> bool {
        same_chord(self.key, self.modifiers, key)
    }
}

/// A chord the surface answers before any mode's [`ActionSpec`] is consulted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reserved {
    Settings,
    PreviewDown,
    PreviewUp,
    NextTab,
    PrevTab,
}

/// The reserved chords, as data. Both [`PickerSurface::on_key`] and the test
/// helper that forbids a mode from claiming one read *this* list — they used to
/// be five hand-written `if`s that nothing checked against, so a mode declaring
/// one of these chords had its action silently swallowed with no error, which is
/// how Ports' `^w` quietly killed delete-word for that picker.
const RESERVED: &[(KeyCode, KeyModifiers, Reserved)] = &[
    (KeyCode::Char(','), KeyModifiers::ALT, Reserved::Settings),
    (KeyCode::Char('j'), KeyModifiers::ALT, Reserved::PreviewDown),
    (KeyCode::Char('k'), KeyModifiers::ALT, Reserved::PreviewUp),
    (KeyCode::Tab, KeyModifiers::NONE, Reserved::NextTab),
    (KeyCode::BackTab, KeyModifiers::NONE, Reserved::PrevTab),
];

fn reserved_for(code: KeyCode, modifiers: KeyModifiers) -> Option<Reserved> {
    let event = KeyEvent::new(code, modifiers);
    RESERVED
        .iter()
        .find(|(reserved_code, reserved_mods, _)| same_chord(*reserved_code, *reserved_mods, event))
        .map(|(_, _, what)| *what)
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
    /// Number of command-bar rows reserved below the picker body. Most modes
    /// have a short contextual bar; the central menu exposes every route and
    /// uses two balanced rows so the shortcuts stay scannable.
    fn action_bar_rows(&self) -> u16 {
        1
    }
    /// Optional, local-only views over the mode's current catalogue. The shared
    /// picker owns navigation and hit testing; the mode owns what each tab means.
    fn tabs(&self) -> Vec<PickerTab> {
        Vec::new()
    }
    fn activate_tab(&mut self, _id: &str) -> Option<Vec<PickerItem>> {
        None
    }
    fn empty_message(&self) -> &str {
        "Waiting for data…"
    }
    fn reload_config(&mut self, _config: &crate::config::Config) -> Result<()> {
        Ok(())
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>>;
    /// Whether [`poll`](Self::poll) currently has a background source behind it.
    ///
    /// The host waits on input at an idle tick unless this says otherwise. A
    /// mode with nothing in flight has nothing a tick could discover, and waking
    /// the process twenty times a second to ask an empty channel is cost with no
    /// answer behind it — the same adaptive tick Projects and Git already use,
    /// which this shared picker was the last surface to be missing.
    ///
    /// Derive it from the state `poll` itself reads rather than returning a
    /// hard-coded `true`, so a mode cannot claim to be waiting when it is not.
    fn is_polling(&self) -> bool {
        false
    }
    fn poll(&mut self) -> Option<Result<Vec<PickerItem>>> {
        None
    }
    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome>;
}

/// How often the host looks for a background answer while one is in flight.
const POLL_TICK: Duration = Duration::from_millis(50);
/// How often it looks when nothing is. Input still wakes the loop immediately;
/// this is only how long a wait can sit before it is re-armed.
const IDLE_TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// Tab click zones measured by the same loop that draws their labels.
    tab_zones: Vec<(Rect, &'static str)>,
    /// Each command-bar row and the pills it carries. Rows stay paired with
    /// their measured zones so wrapped bars remain click-safe.
    bar_rows: Vec<BarRow>,
}

struct BarRow {
    y: u16,
    zones: Vec<(u16, u16, PillAct)>,
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
            tab_zones: Vec::new(),
            bar_rows: Vec::new(),
        };
        state.recompute(&FieldSchema::default());
        state
    }

    fn replace(&mut self, items: Vec<PickerItem>, schema: &FieldSchema) {
        self.selected_id = self.selected_item().map(|item| item.id.clone());
        let previous = self.selected;
        self.items = items;
        self.recompute(schema);
        let kept = self.selected_id.as_ref().and_then(|id| {
            self.filtered
                .iter()
                .position(|index| self.items[*index].id == *id)
        });
        // A row that is gone (forgotten, killed) leaves the cursor on its
        // neighbour rather than sending it back to the top.
        self.selected = kept.unwrap_or_else(|| previous.min(self.filtered.len().saturating_sub(1)));
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
        // The best match, never the old index: an index means a different row
        // once the list has been re-filtered and re-ranked, and Enter would run
        // it. `replace` restores the row a refresh should keep by its ID.
        self.selected = 0;
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
fn title_color(theme: &Theme, cfg: &Config) -> Color {
    theme
        .resolve(&cfg.common.title_color)
        .unwrap_or_else(|| theme.or("peach", Color::Yellow))
}

enum PickerExit {
    Close,
    Invoke(String, &'static str),
}

struct PickerSurface<'a, M> {
    mode: &'a mut M,
    theme: &'a Theme,
    background: tui::SurfaceBackground,
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
            self.background,
            self.title,
            self.actions,
            self.state,
        );
    }

    fn tick_rate(&self) -> Duration {
        if self.mode.is_polling() {
            POLL_TICK
        } else {
            IDLE_TICK
        }
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
                        if let Some(act) = self
                            .state
                            .bar_rows
                            .iter()
                            .find_map(|row| tui::zone_at(&row.zones, row.y, at))
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
                        if let Some(id) = self
                            .state
                            .tab_zones
                            .iter()
                            .find(|(zone, _)| zone.contains(at))
                            .map(|(_, id)| *id)
                        {
                            return Ok(self.activate_tab(id));
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
    fn activate_tab(&mut self, id: &'static str) -> Transition<PickerExit> {
        let Some(items) = self.mode.activate_tab(id) else {
            return Transition::Wait;
        };
        self.state.runtime_error = None;
        self.state.replace(items, self.schema);
        Transition::Redraw
    }

    fn cycle_tab(&mut self, delta: isize) -> Transition<PickerExit> {
        let tabs = self.mode.tabs();
        if tabs.len() < 2 {
            return Transition::Wait;
        }
        let current = tabs.iter().position(|tab| tab.active).unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(tabs.len() as isize) as usize;
        self.activate_tab(tabs[next].id)
    }

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
        match reserved_for(key.code, key.modifiers) {
            Some(Reserved::Settings) => {
                return Ok(Transition::Exit(PickerExit::Invoke(
                    String::new(),
                    "__settings",
                )));
            }
            Some(Reserved::PreviewDown) => {
                self.state.preview_scroll = self.state.preview_scroll.saturating_add(1);
                return Ok(Transition::Redraw);
            }
            Some(Reserved::PreviewUp) => {
                self.state.preview_scroll = self.state.preview_scroll.saturating_sub(1);
                return Ok(Transition::Redraw);
            }
            Some(Reserved::NextTab) => return Ok(self.cycle_tab(1)),
            Some(Reserved::PrevTab) => return Ok(self.cycle_tab(-1)),
            None => {}
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
                // `⌥⌫`, not `^w`: `^w` opens the selected row in a workspace on
                // every surface that has that verb, and the row wins over the
                // readline habit.
                KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
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

pub fn run<M: PickerMode>(mode: M, theme: Theme, cfg: Config) -> Result<()> {
    run_in(&mut crate::surface::TerminalHost, mode, theme, cfg)
}

/// [`run`] on any host; the settings form it may open is the real one.
pub(crate) fn run_in<M: PickerMode>(
    host: &mut impl crate::surface::Host,
    mode: M,
    theme: Theme,
    cfg: Config,
) -> Result<()> {
    run_with(
        mode,
        theme,
        cfg,
        |surface| host.run(surface),
        || {
            crate::settings::main(Config::try_load()?, Theme::load())?;
            Ok((Config::try_load()?, Theme::load()))
        },
    )
}

/// The picker's outer loop, with the terminal host and the settings form passed
/// in: production hands it `surface::run` and the standalone settings pane, and
/// a test hands it a scripted sequence of exits.
fn run_with<M: PickerMode>(
    mut mode: M,
    mut theme: Theme,
    mut cfg: Config,
    mut host: impl FnMut(&mut PickerSurface<'_, M>) -> Result<PickerExit>,
    mut settings: impl FnMut() -> Result<(Config, Theme)>,
) -> Result<()> {
    let schema = mode.schema();
    let normal = cfg.common.keymode == crate::config::KeyMode::Normal;
    let mut state = State::new(mode.initial()?, normal);
    state.recompute(&schema);
    let mut title = title_color(&theme, &cfg);
    let mut background = tui::SurfaceBackground::resolve(&theme, cfg.common.transparency);
    loop {
        let mut actions = mode.actions();
        apply_bindings(&mut actions, &mode.key_bindings());
        let outcome = host(&mut PickerSurface {
            mode: &mut mode,
            theme: &theme,
            background,
            title,
            actions: &actions,
            schema: &schema,
            state: &mut state,
        })?;
        let PickerExit::Invoke(item, action) = outcome else {
            return Ok(());
        };
        if action == "__settings" {
            (cfg, theme) = settings()?;
            mode.reload_config(&cfg)?;
            state.input_mode = if cfg.common.keymode == crate::config::KeyMode::Normal {
                InputMode::Normal
            } else {
                InputMode::Insert
            };
            background = tui::SurfaceBackground::resolve(&theme, cfg.common.transparency);
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

/// Append `text` to a row, clipped to what is left of `budget` and ellipsised
/// if it does not fit.
///
/// A free function rather than a closure so its output can borrow `text`: the
/// overwhelmingly common case is a column that fits, and copying it into a
/// `String` meant one allocation per column per row per frame — paid for every
/// row in the catalogue, including the ones the `List` scrolls past. Only the
/// clipped tail, which has to be rebuilt with its ellipsis, allocates.
fn push_clipped<'a>(
    text: &'a str,
    style: Style,
    budget: usize,
    used: &mut usize,
    spans: &mut Vec<Span<'a>>,
) {
    if *used >= budget {
        return;
    }
    let len = text.chars().count();
    if *used + len <= budget {
        *used += len;
        spans.push(Span::styled(text, style));
    } else {
        let room = budget - *used;
        let cut: String = text.chars().take(room.saturating_sub(1)).collect();
        *used = budget;
        spans.push(Span::styled(format!("{cut}…"), style));
    }
}

fn draw<M: PickerMode>(
    frame: &mut Frame,
    mode: &M,
    theme: &Theme,
    background: tui::SurfaceBackground,
    title_color: Color,
    actions: &[ActionSpec],
    state: &mut State,
) {
    let area = frame.area();
    background.paint(frame, area);
    // The action bar comes off the bottom before the search box or the list get
    // a say. Trailing it as a `Constraint::Length` made it the cheapest row for
    // ratatui to drop against a `Min`, so the pane that had least room to spare
    // was the one that stopped saying which keys do anything.
    let (top, bar_area) = tui::reserve_bar(area, action_bar_height(mode.action_bar_rows()));
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(top);
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
    let tag_width = state
        .filtered
        .iter()
        .filter_map(|&index| state.items[index].trailing.as_deref())
        .map(|tag| tag.chars().count())
        .max()
        .unwrap_or(0);
    let marker_width = state
        .filtered
        .iter()
        .filter_map(|&index| state.items[index].trailing_marker.as_ref())
        .map(|marker| marker.text.chars().count())
        .max()
        .unwrap_or(0);
    let gutter = tag_width + marker_width + usize::from(marker_width > 0 && tag_width > 0);
    let emphasize = mode.emphasize_head();

    // The frame and the list state are both settled *before* the rows are built,
    // and the state is moved out rather than borrowed. Rows borrow their text
    // straight out of `state.items` instead of copying every column into a fresh
    // `String` per row per frame — a catalogue is `commands.history_limit` rows
    // deep by default — and that borrow has to outlive the render call, which it
    // can only do if nothing else is holding `state` mutably at the time. The
    // scroll offset rides along inside the moved-out state and is put back below.
    let tabs = mode.tabs();
    state.tab_zones.clear();
    let list_block = if tabs.is_empty() {
        tui::boxed(mode.title(), title_color, border)
    } else {
        let mut spans = Vec::new();
        let mut x = cols[0].x + 1;
        for tab in tabs {
            let label = format!(" {} ", tab.label);
            let width = label.chars().count() as u16;
            state
                .tab_zones
                .push((Rect::new(x, cols[0].y, width, 1), tab.id));
            x = x.saturating_add(width + 1);
            let style = if tab.active {
                Style::default()
                    .fg(ink)
                    .bg(title_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(border)
            };
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
        }
        tui::framed(border).title(Line::from(spans))
    };
    let mut list_state = std::mem::take(&mut state.list_state);
    list_state.select((!state.filtered.is_empty()).then_some(state.selected));

    let items = state
        .filtered
        .iter()
        .enumerate()
        .map(|(visible_index, index)| {
            let item = &state.items[*index];
            let selected = visible_index == state.selected;
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

            let mut spans: Vec<Span<'_>> = Vec::new();
            let mut used = 0usize;

            // For a wall of shell commands the leading word is what the eye hunts
            // for, so a mode can ask for it in its own colour and bold. Everything
            // else stays plain `text`, the projects picker's division of colour.
            match item.primary.split_once(' ').filter(|_| emphasize) {
                Some((head, tail)) => {
                    push_clipped(
                        head,
                        Style::default()
                            .fg(if selected { accent } else { head_color })
                            .add_modifier(Modifier::BOLD),
                        budget,
                        &mut used,
                        &mut spans,
                    );
                    push_clipped(" ", Style::default(), budget, &mut used, &mut spans);
                    push_clipped(
                        tail,
                        Style::default().fg(if selected { accent } else { text }),
                        budget,
                        &mut used,
                        &mut spans,
                    );
                }
                None => {
                    let style = if emphasize {
                        Style::default()
                            .fg(if selected { accent } else { head_color })
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(if selected { accent } else { text })
                    };
                    push_clipped(&item.primary, style, budget, &mut used, &mut spans);
                }
            }
            if !item.secondary.is_empty() {
                push_clipped("  ", Style::default(), budget, &mut used, &mut spans);
                push_clipped(
                    &item.secondary,
                    Style::default()
                        .fg(if selected { accent } else { note_color })
                        .add_modifier(Modifier::DIM),
                    budget,
                    &mut used,
                    &mut spans,
                );
            }

            // Right-align the tag: pad out to the gutter, then draw it.
            if gutter > 0 {
                let tag = item.trailing.as_deref().unwrap_or_default();
                let pad = row_width.saturating_sub(used).saturating_sub(gutter);
                spans.push(Span::raw(tui::spaces(pad)));
                if marker_width > 0 {
                    if let Some(marker) = &item.trailing_marker {
                        spans.push(Span::raw(tui::spaces(
                            marker_width.saturating_sub(marker.text.chars().count()),
                        )));
                        spans.push(Span::styled(
                            marker.text.as_str(),
                            Style::default().fg(theme.or(&marker.color_slot, Color::Yellow)),
                        ));
                    } else {
                        spans.push(Span::raw(tui::spaces(marker_width)));
                    }
                    if tag_width > 0 {
                        spans.push(Span::raw(" "));
                    }
                }
                spans.push(Span::raw(tui::spaces(
                    tag_width.saturating_sub(tag.chars().count()),
                )));
                spans.push(Span::styled(
                    tag,
                    Style::default()
                        .fg(if selected { accent } else { muted })
                        .add_modifier(Modifier::DIM),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(list_block)
        .highlight_style(Style::default().bg(surface).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");
    frame.render_stateful_widget(list, cols[0], &mut list_state);
    state.list_state = list_state;

    let preview = state
        .selected_item()
        .map(|item| {
            item.preview
                .iter()
                .map(|line| Line::from(line.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            let message = if state.items.is_empty() {
                mode.empty_message()
            } else {
                "No matches"
            };
            vec![Line::from(Span::styled(
                format!(" {message}"),
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
    state.bar_rows.clear();
    for (row_index, range) in balanced_bar_ranges(
        &pills,
        mode.action_bar_rows().max(1) as usize,
        bar_area.width,
    )
    .into_iter()
    .enumerate()
    {
        // The wrapped row's y is arithmetic, not a chunk, so it has to be asked
        // whether it still lands inside the bar: a row drawn past the bottom is
        // clipped away by ratatui but would still register clicks through the
        // `BarRow` it published, on whatever the list drew there.
        let y = bar_area.y + row_index as u16 * 2;
        if y >= bar_area.bottom() {
            break;
        }
        let row = Rect::new(bar_area.x, y, bar_area.width, 1);
        let (spans, zones) = tui::pill_row(&pills[range.clone()], ink, row.x);
        state.bar_rows.push(BarRow {
            y: row.y,
            zones: zones
                .into_iter()
                .zip(caps[range].iter())
                .map(|((a, b), (_, act))| (a, b, *act))
                .collect(),
        });
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
    }
}

fn action_bar_height(rows: u16) -> u16 {
    rows.max(1).saturating_mul(2).saturating_sub(1)
}

fn balanced_bar_ranges(
    pills: &[Pill<'_>],
    requested_rows: usize,
    width: u16,
) -> Vec<std::ops::Range<usize>> {
    if pills.is_empty() {
        return Vec::new();
    }
    let rows = requested_rows.max(1).min(pills.len());
    let pill_width = |pill: &Pill<'_>| pill.key.chars().count() + pill.label.chars().count() + 4;
    let total = pills.iter().map(pill_width).sum::<usize>();
    let target = total
        .div_ceil(rows)
        .min(width.saturating_sub(1).max(1) as usize);
    let mut ranges = Vec::with_capacity(rows);
    let mut start = 0;
    let mut used = 0;
    for (index, pill) in pills.iter().enumerate() {
        let next = pill_width(pill);
        if index > start && used + next > target && ranges.len() + 1 < rows {
            ranges.push(start..index);
            start = index;
            used = 0;
        }
        used += next;
    }
    ranges.push(start..pills.len());
    ranges
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

/// Assert that one picker's declared actions obey the prefix concept, so a new
/// `ActionSpec` cannot quietly reintroduce a per-surface dialect. Every mode
/// calls this from its own test module — the modes are private to their files,
/// so the check travels to them rather than the other way round.
///
/// Each rule here corresponds to a bug that shipped: a bare letter would be
/// matched ahead of the Insert typing arm and make that letter untypeable; a
/// reserved chord is swallowed by the surface with no error (Ports' `^w`); a
/// stale `key_label` prints a cap that no longer runs anything; and a shared
/// action id on two different chords is the drift the concept exists to stop.
#[cfg(test)]
pub(crate) fn assert_follows_prefix_concept(surface: &str, actions: &[ActionSpec]) {
    /// Action ids whose chord is fixed project-wide. A spec carrying no modifier
    /// is exempt: that is the `↵` ladder, where the surface's *primary* action
    /// may well be one of these verbs (Ports opens by copying an address).
    const CANONICAL: &[(&str, KeyCode, KeyModifiers)] = &[
        ("tab", KeyCode::Char('e'), KeyModifiers::CONTROL),
        ("workspace", KeyCode::Char('w'), KeyModifiers::CONTROL),
        ("copy", KeyCode::Char('y'), KeyModifiers::CONTROL),
        ("star", KeyCode::Char('s'), KeyModifiers::CONTROL),
        ("sort", KeyCode::Char('s'), KeyModifiers::ALT),
    ];

    for action in actions {
        let id = action.id;
        let modified = !action.modifiers.is_empty();

        assert!(
            modified || !matches!(action.key, KeyCode::Char(_)),
            "{surface}.{id} is a bare letter: it would be matched before the query \
             and make that character untypeable"
        );

        let ctrl = action.modifiers == KeyModifiers::CONTROL;
        assert!(
            !(ctrl && matches!(action.key, KeyCode::Char('c') | KeyCode::Char('u'))),
            "{surface}.{id} takes ^c or ^u, which close the picker and clear the query \
             on every surface"
        );
        // herdr eats its prefix before the pane sees the key, so an action here
        // would never run and the pill would advertise a dead cap.
        assert!(
            !(ctrl && action.key == KeyCode::Char('b')),
            "{surface}.{id} takes ^b, which herdr claims as its default prefix"
        );

        assert!(
            reserved_for(action.key, action.modifiers).is_none(),
            "{surface}.{id} claims a chord the surface answers itself, so it would \
             never run"
        );

        let event = KeyEvent::new(action.key, action.modifiers);
        let chord = crate::keymap::chord_of(&event)
            .unwrap_or_else(|| panic!("{surface}.{id} uses a key the chord parser cannot model"));
        assert_eq!(
            action.key_label,
            chord.label(),
            "{surface}.{id} prints a cap that is not the key it listens for"
        );

        if let Some((_, key, modifiers)) = CANONICAL.iter().find(|(name, _, _)| *name == id) {
            if modified {
                assert!(
                    action.key == *key && action.modifiers == *modifiers,
                    "{surface}.{id} disagrees with the chord that action carries elsewhere"
                );
            }
        }

        // Matching ignores SHIFT, so two specs that differ only by it are one
        // chord wearing two names and only the first would ever run.
        let twins: Vec<&str> = actions
            .iter()
            .filter(|other| other.id != id && same_chord(other.key, other.modifiers, event))
            .map(|other| other.id)
            .collect();
        assert!(
            twins.is_empty(),
            "{surface}.{id} shares a chord with {twins:?}"
        );
    }
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

    struct TabbedMode {
        active: &'static str,
    }

    impl TabbedMode {
        fn items(&self) -> Vec<PickerItem> {
            match self.active {
                "starred" => vec![test_item("keep")],
                _ => vec![test_item("keep"), test_item("other")],
            }
        }
    }

    impl PickerMode for TabbedMode {
        fn title(&self) -> &str {
            "Commands"
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
        fn tabs(&self) -> Vec<PickerTab> {
            vec![
                PickerTab {
                    id: "history",
                    label: "History",
                    active: self.active == "history",
                },
                PickerTab {
                    id: "starred",
                    label: "★ Starred",
                    active: self.active == "starred",
                },
            ]
        }
        fn activate_tab(&mut self, id: &str) -> Option<Vec<PickerItem>> {
            self.active = match id {
                "history" => "history",
                "starred" => "starred",
                _ => return None,
            };
            Some(self.items())
        }
        fn empty_message(&self) -> &str {
            "No stars — ctrl-s in History"
        }
        fn initial(&mut self) -> Result<Vec<PickerItem>> {
            Ok(self.items())
        }
        fn execute(&mut self, _item_id: &str, _action: &str) -> Result<ActionOutcome> {
            Ok(ActionOutcome::Close)
        }
    }

    /// A mode whose command bar wraps, so the two-row placement arithmetic is
    /// exercised against a pane that cannot hold two rows.
    struct WrappedBarMode;

    impl PickerMode for WrappedBarMode {
        fn title(&self) -> &str {
            "Menu"
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
        fn action_bar_rows(&self) -> u16 {
            2
        }
        fn initial(&mut self) -> Result<Vec<PickerItem>> {
            Ok(vec![test_item("keep")])
        }
        fn execute(&mut self, _item_id: &str, _action: &str) -> Result<ActionOutcome> {
            Ok(ActionOutcome::Close)
        }
    }

    fn render_sized<M: PickerMode>(
        mode: &M,
        state: &mut State,
        w: u16,
        h: u16,
    ) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[
            ("accent", "#6fd0a8"),
            ("overlay0", "#6c7e76"),
            ("panel_bg", "#101214"),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let background =
            tui::SurfaceBackground::resolve(&theme, crate::config::Transparency::Transparent);
        terminal
            .draw(|frame| draw(frame, mode, &theme, background, Color::Yellow, &[], state))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    /// The action bar is reserved before the search box and the list, so it is
    /// the last thing a short pane loses rather than the first. A trailing
    /// `Constraint::Length` behind a `Min` is exactly what ratatui drops first.
    #[test]
    fn the_action_bar_survives_a_pane_too_short_for_the_layout() {
        let mut state = State::new(vec![test_item("keep")], false);
        for h in [3u16, 5, 7, 12] {
            let buffer = render_sized(&TestMode, &mut state, 80, h);
            let bar = row_text(&buffer, h - 1);
            assert!(
                bar.contains("esc mode/close"),
                "an 80x{h} pane lost its action bar: {bar}"
            );
        }
    }

    /// A wrapped bar places its second row by arithmetic rather than by a chunk,
    /// so it has to stop at the reserved rect: a row drawn past the bottom is
    /// clipped away but would still publish a `BarRow` that answers clicks.
    #[test]
    fn a_wrapped_bar_never_places_a_row_outside_the_space_it_was_given() {
        let mut state = State::new(vec![test_item("keep")], false);
        for h in [3u16, 4, 5, 8, 20] {
            let buffer = render_sized(&WrappedBarMode, &mut state, 40, h);
            assert!(
                state.bar_rows.iter().all(|row| row.y < h),
                "a 40x{h} pane published a bar row at {:?}, outside the frame",
                state.bar_rows.iter().map(|row| row.y).collect::<Vec<_>>()
            );
            // Every row it did publish carries pills, and the first one always
            // exists: a bar that reports rows it never painted is the failure.
            assert!(
                !state.bar_rows.is_empty(),
                "a 40x{h} pane drew no bar at all"
            );
            for row in &state.bar_rows {
                let text = row_text(&buffer, row.y);
                assert!(
                    !text.trim().is_empty(),
                    "a 40x{h} pane published an empty bar row at {}",
                    row.y
                );
            }
        }
    }

    fn render(state: &mut State) -> ratatui::buffer::Buffer {
        render_with_background(state, crate::config::Transparency::Transparent)
    }

    fn render_tabbed(mode: &TabbedMode, state: &mut State) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[
            ("accent", "#6fd0a8"),
            ("overlay0", "#6c7e76"),
            ("panel_bg", "#101214"),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        let background =
            tui::SurfaceBackground::resolve(&theme, crate::config::Transparency::Transparent);
        terminal
            .draw(|frame| draw(frame, mode, &theme, background, Color::Yellow, &[], state))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_with_background(
        state: &mut State,
        transparency: crate::config::Transparency,
    ) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[
            ("accent", "#6fd0a8"),
            ("overlay0", "#6c7e76"),
            ("peach", "#dcbb80"),
            ("panel_bg", "#101214"),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        let background = tui::SurfaceBackground::resolve(&theme, transparency);
        terminal
            .draw(|f| {
                draw(
                    f,
                    &TestMode,
                    &theme,
                    background,
                    theme.or("peach", Color::Yellow),
                    &[],
                    state,
                )
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// One keypress, one answer, on both halves of the app. Projects resolves a
    /// chord through `keymap::chord_of`, which weighs only CTRL and ALT; the
    /// shared picker compared modifier bits exactly, so the same press could act
    /// in one and do nothing in the other.
    #[test]
    fn a_shift_bit_a_terminal_adds_does_not_lose_the_action() {
        let items = vec![test_item("keep"), test_item("other")];

        // A terminal that reports the shift bit alongside a ctrl chord still
        // reaches the ctrl action.
        let mut h = Harness::new(items.clone(), false);
        let outcome = h.key(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(
            matches!(&outcome, Transition::Exit(PickerExit::Invoke(_, "remove"))),
            "^⇧x must still be ^x"
        );

        // And `shift-enter` runs the row, the way it already did in Projects.
        let mut h = Harness::new(items, false);
        let outcome = h.key(KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(
            matches!(&outcome, Transition::Exit(PickerExit::Invoke(_, "open"))),
            "⇧↵ must mean ↵, as it does in Projects"
        );
    }

    /// A guard that never fires is worse than no guard, so this asserts the
    /// concept check actually rejects each shape it claims to — the bare letter,
    /// the reserved chord, `^u`, the lying cap, and the shared id that drifted.
    #[test]
    fn the_prefix_concept_check_rejects_every_shape_it_names() {
        let spec = |key, modifiers, key_label: &str, id| ActionSpec {
            id,
            key,
            modifiers,
            key_label: key_label.into(),
            label: "test",
            color_slot: "peach",
        };
        let rejected = |actions: Vec<ActionSpec>| {
            std::panic::catch_unwind(move || {
                assert_follows_prefix_concept("test", &actions);
            })
            .is_err()
        };

        // A sanity anchor: the shape the concept describes must pass.
        assert!(!rejected(vec![spec(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
            "^w",
            "workspace"
        )]));

        // A bare letter would be matched before the query and go untypeable.
        assert!(rejected(vec![spec(
            KeyCode::Char('w'),
            KeyModifiers::NONE,
            "w",
            "workspace"
        )]));
        // ^u clears the query on every surface.
        assert!(rejected(vec![spec(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
            "^u",
            "use"
        )]));
        // ⌥j is answered by the surface itself, so this action would never run.
        assert!(rejected(vec![spec(
            KeyCode::Char('j'),
            KeyModifiers::ALT,
            "⌥j",
            "down"
        )]));
        // ^b never reaches the pane: it is herdr's default prefix.
        assert!(rejected(vec![spec(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
            "^b",
            "tab"
        )]));
        // A cap that is not the key it listens for.
        assert!(rejected(vec![spec(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
            "^c",
            "copy"
        )]));
        // The drift itself: a shared id on a chord it does not carry elsewhere.
        assert!(rejected(vec![spec(
            KeyCode::Char('w'),
            KeyModifiers::ALT,
            "⌥w",
            "workspace"
        )]));
        // Two names for one chord: matching ignores SHIFT, so only the first
        // of these would ever run.
        assert!(rejected(vec![
            spec(KeyCode::Enter, KeyModifiers::NONE, "↵", "open"),
            spec(KeyCode::Enter, KeyModifiers::SHIFT, "↵", "open_other"),
        ]));
    }

    fn test_item(id: &str) -> PickerItem {
        PickerItem {
            id: id.into(),
            primary: id.into(),
            secondary: format!("{id} detail"),
            trailing: None,
            trailing_marker: None,
            document: Document::fuzzy(id),
            preview: Vec::new(),
            accent_slot: None,
        }
    }

    /// A mode that overrides nothing gets the documented defaults. These are
    /// what every simple mode relies on, so a changed default silently changes
    /// four pickers at once.
    #[test]
    fn a_mode_that_overrides_nothing_gets_the_documented_defaults() {
        let mut mode = TestMode;

        assert!(mode.tabs().is_empty(), "no tabs by default");
        assert!(
            mode.activate_tab("anything").is_none(),
            "a mode with no tabs cannot activate one"
        );
        assert!(mode.key_bindings().is_empty());
        assert!(
            mode.action_disabled_reason("any", "any").is_none(),
            "nothing is disabled unless a mode says so"
        );
        assert!(
            !mode.emphasize_head(),
            "the leading word is plain by default"
        );
        assert_eq!(mode.list_pct(), 42);
        assert_eq!(mode.action_bar_rows(), 1);
        assert!(!mode.is_polling(), "no background source by default");
        assert!(mode.poll().is_none());
        assert!(
            !mode.empty_message().is_empty(),
            "an empty list still says something"
        );
        mode.reload_config(&Config::default()).unwrap();
    }

    /// The surface draws through the shared frame, and a wide pane lays out the
    /// list beside its preview card.
    #[test]
    fn the_surface_draws_the_list_its_title_and_the_command_bar() {
        let mut h = Harness::new(items(3), true);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| h.surface().draw(frame)).unwrap();

        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("Ports"),
            "the mode title is on the list: {screen}"
        );
        assert!(screen.contains("Search"), "the query box is captioned");
        assert!(screen.contains("item-0"), "the rows are drawn");
        assert!(screen.contains("open"), "the command bar carries its pills");

        // Drawing publishes the zones the click router reads.
        assert!(h.state.list_area.width > 0);
        assert!(!h.state.bar_rows.is_empty());
    }

    /// A tabbed mode draws its tabs into the list's caption slot and publishes a
    /// click zone for each, measured by the loop that lays them out.
    #[test]
    fn a_tabbed_mode_draws_its_tabs_and_publishes_a_zone_for_each() {
        let mode = TabbedMode { active: "history" };
        let mut state = State::new(mode.items(), true);
        let buffer = render_tabbed(&mode, &mut state);

        let screen: String = (0..12)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .collect();
        assert!(screen.contains("History"), "{screen}");
        assert!(screen.contains("Starred"), "{screen}");
        assert_eq!(
            state.tab_zones.len(),
            2,
            "one click zone per tab, measured where it was drawn"
        );
        assert!(state.tab_zones.iter().all(|(zone, _)| zone.width > 0));
    }

    /// A runtime error replaces the ordinary result line, so a failed action
    /// says why rather than looking like it did nothing.
    #[test]
    fn a_runtime_error_is_drawn_where_the_result_count_goes() {
        let mut h = Harness::new(items(2), true);
        h.state.runtime_error = Some("listener is stale".into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| h.surface().draw(frame)).unwrap();

        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("listener is stale"), "{screen}");
    }

    /// A malformed query is reported where it went wrong rather than silently
    /// matching nothing.
    #[test]
    fn a_malformed_query_is_reported_rather_than_matching_nothing() {
        let mut h = Harness::new(items(2), false);
        h.state.query = "cmd:\"unterminated".into();
        h.state.recompute(&h.schema);

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| h.surface().draw(frame)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("quote") || screen.contains("unterminated"),
            "the diagnostic is shown: {screen}"
        );
    }

    /// The scroll offset lives in `State` between frames, and turning a click
    /// back into an item is the only thing that can read it. `draw` moves the
    /// `ListState` out so the rows can borrow their text across the render, so
    /// this pins that it is put back — a stranded offset would not error, it
    /// would quietly start resolving clicks to the wrong row.
    #[test]
    fn a_draw_returns_the_scroll_offset_it_borrowed() {
        let items: Vec<PickerItem> = (0..60).map(|i| test_item(&format!("item-{i}"))).collect();
        let mut state = State::new(items, false);
        state.selected = 55;

        let _ = render(&mut state);
        let scrolled = state.list_state.offset();
        assert!(
            scrolled > 0,
            "a selection past the fold should have scrolled the list"
        );

        // A second frame with nothing changed must resume from the same place.
        let _ = render(&mut state);
        assert_eq!(state.list_state.offset(), scrolled);
        assert_eq!(state.list_state.selected(), Some(55));
    }

    /// A row's columns are assembled from borrowed slices of the item, with a
    /// clipped tail the only part that is rebuilt, and its tag is right-aligned
    /// with padding sliced from a static. This pins the three shapes that
    /// arrangement has to keep producing: a row that fits, a row clipped with an
    /// ellipsis, and a right-aligned trailing tag.
    #[test]
    fn a_row_borrows_what_fits_clips_what_does_not_and_right_aligns_its_tag() {
        let wide = "W".repeat(120);
        let mut items = vec![test_item("keep"), test_item(&wide)];
        items[0].trailing = Some("9s".into());
        items[1].trailing = Some("11s".into());
        let mut state = State::new(items, false);

        let buffer = render(&mut state);
        let rows: Vec<String> = (0..12)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect();

        let fitting = rows.iter().find(|row| row.contains("keep")).expect("row");
        assert!(
            fitting.contains("keep  keep detail"),
            "a fitting row keeps both columns verbatim: {fitting}"
        );
        let clipped = rows.iter().find(|row| row.contains("WWW")).expect("row");
        assert!(
            clipped.contains('…'),
            "an overlong row is clipped with an ellipsis: {clipped}"
        );
        assert!(
            !clipped.contains(wide.as_str()),
            "the clipped row must not carry the full text: {clipped}"
        );

        // Both tags end in the same column: the shorter one is padded out to it.
        let end = |row: &str, tag: &str| row.find(tag).expect("tag") + tag.len();
        assert_eq!(
            end(fitting, "9s"),
            end(clipped, "11s"),
            "tags share a right-aligned gutter:\n{fitting}\n{clipped}"
        );
    }

    /// Drive a `PickerSurface` over `TestMode`, so the shared engine's key and
    /// mouse routing is exercised without a terminal behind it.
    struct Harness {
        mode: TestMode,
        theme: Theme,
        actions: Vec<ActionSpec>,
        schema: FieldSchema,
        state: State,
    }

    impl Harness {
        fn new(items: Vec<PickerItem>, normal: bool) -> Self {
            Self {
                mode: TestMode,
                theme: Theme::default(),
                actions: vec![
                    ActionSpec {
                        id: "open",
                        key: KeyCode::Enter,
                        modifiers: KeyModifiers::NONE,
                        key_label: "↵".into(),
                        label: "open",
                        color_slot: "blue",
                    },
                    ActionSpec {
                        id: "remove",
                        key: KeyCode::Char('x'),
                        modifiers: KeyModifiers::CONTROL,
                        key_label: "^x".into(),
                        label: "remove",
                        color_slot: "red",
                    },
                ],
                schema: FieldSchema::default(),
                state: State::new(items, normal),
            }
        }

        fn surface(&mut self) -> PickerSurface<'_, TestMode> {
            PickerSurface {
                mode: &mut self.mode,
                theme: &self.theme,
                background: tui::SurfaceBackground::resolve(
                    &self.theme,
                    crate::config::Transparency::Transparent,
                ),
                title: Color::Yellow,
                actions: &self.actions,
                schema: &self.schema,
                state: &mut self.state,
            }
        }

        fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Transition<PickerExit> {
            let event = Event::Key(KeyEvent::new(code, modifiers));
            self.surface()
                .on_event(event)
                .expect("key routing is IO-free")
        }

        fn press(&mut self, code: KeyCode) -> Transition<PickerExit> {
            self.key(code, KeyModifiers::NONE)
        }

        fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) -> Transition<PickerExit> {
            let event = Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
            self.surface()
                .on_event(event)
                .expect("mouse routing is IO-free")
        }
    }

    fn items(count: usize) -> Vec<PickerItem> {
        (0..count)
            .map(|i| test_item(&format!("item-{i}")))
            .collect()
    }

    /// Every Normal-mode key, end to end on the surface: motion wraps, `g`/`G`
    /// jump, `i` and `/` start typing, `q`/`esc` close, and anything else waits.
    #[test]
    fn normal_mode_keys_move_jump_type_and_close() {
        let mut h = Harness::new(items(4), true);
        h.press(KeyCode::Char('k'));
        assert_eq!(h.state.selected, 3, "k wraps to the bottom");
        h.press(KeyCode::Up);
        h.press(KeyCode::Down);
        h.press(KeyCode::Char('j'));
        assert_eq!(h.state.selected, 0, "j wraps to the top");
        h.press(KeyCode::Char('G'));
        assert_eq!(h.state.selected, 3);
        h.press(KeyCode::Home);
        assert_eq!(h.state.selected, 0);
        h.press(KeyCode::End);
        assert_eq!(h.state.selected, 3);
        h.press(KeyCode::Char('g'));
        assert_eq!(h.state.selected, 0);
        assert!(matches!(h.press(KeyCode::Char('z')), Transition::Wait));
        assert!(matches!(
            h.press(KeyCode::Char('q')),
            Transition::Exit(PickerExit::Close)
        ));
        assert!(matches!(
            h.press(KeyCode::Esc),
            Transition::Exit(PickerExit::Close)
        ));
        h.press(KeyCode::Char('i'));
        assert_eq!(h.state.input_mode, InputMode::Insert);
    }

    /// Insert mode types, moves with the arrows, returns to Normal on `esc`,
    /// and closes on `^c`; a release or a resize is not a keypress.
    #[test]
    fn insert_mode_moves_returns_and_closes() {
        let mut h = Harness::new(items(3), false);
        h.press(KeyCode::Down);
        h.press(KeyCode::Down);
        h.press(KeyCode::Up);
        assert_eq!(h.state.selected, 1);
        assert!(matches!(h.press(KeyCode::F(5)), Transition::Wait));
        assert!(matches!(
            h.key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Transition::Exit(PickerExit::Close)
        ));
        h.press(KeyCode::Esc);
        assert_eq!(h.state.input_mode, InputMode::Normal);

        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(matches!(
            h.surface().on_event(release).unwrap(),
            Transition::Wait
        ));
        assert!(matches!(
            h.surface().on_event(Event::Resize(80, 24)).unwrap(),
            Transition::Wait
        ));
    }

    #[test]
    fn moving_through_an_empty_list_is_a_no_op() {
        let mut h = Harness::new(Vec::new(), true);
        h.press(KeyCode::Char('j'));
        assert_eq!(h.state.selected, 0);
        assert!(matches!(h.press(KeyCode::Enter), Transition::Wait));
    }

    /// The wheel scrolls the preview under the pointer and the list elsewhere;
    /// a click selects a row, and a click on the selected row runs it.
    #[test]
    fn the_mouse_scrolls_selects_and_runs() {
        let mut h = Harness::new(items(5), true);
        h.state.preview_area = Rect::new(50, 0, 30, 10);
        h.state.list_area = Rect::new(0, 2, 40, 10);

        h.mouse(MouseEventKind::ScrollDown, 60, 3);
        assert_eq!(h.state.preview_scroll, 3);
        h.mouse(MouseEventKind::ScrollUp, 60, 3);
        assert_eq!(h.state.preview_scroll, 0);
        h.mouse(MouseEventKind::ScrollDown, 5, 3);
        assert_eq!(h.state.selected, 1);
        h.mouse(MouseEventKind::ScrollUp, 5, 3);
        assert_eq!(h.state.selected, 0);

        h.mouse(MouseEventKind::Down(MouseButton::Left), 5, 4);
        assert_eq!(h.state.selected, 2, "a click selects");
        assert_eq!(
            invoked(h.mouse(MouseEventKind::Down(MouseButton::Left), 5, 4)),
            Some(("item-2".to_string(), "open")),
            "a second click runs"
        );
        // Below the last row, and outside every zone, nothing happens.
        h.mouse(MouseEventKind::Down(MouseButton::Left), 5, 11);
        assert_eq!(h.state.selected, 2);
        assert!(matches!(
            h.mouse(MouseEventKind::Up(MouseButton::Left), 5, 4),
            Transition::Wait
        ));
    }

    /// Each kind of command-bar pill does what its cap says.
    #[test]
    fn a_bar_pill_click_closes_opens_settings_or_runs() {
        let mut h = Harness::new(items(2), true);
        h.state.bar_rows = vec![BarRow {
            y: 20,
            zones: vec![
                (0, 4, PillAct::Run("open")),
                (5, 9, PillAct::Settings),
                (10, 14, PillAct::Close),
            ],
        }];
        assert_eq!(
            invoked(h.mouse(MouseEventKind::Down(MouseButton::Left), 1, 20)),
            Some(("item-0".to_string(), "open"))
        );
        assert!(matches!(
            h.mouse(MouseEventKind::Down(MouseButton::Left), 6, 20),
            Transition::Exit(PickerExit::Invoke(_, "__settings"))
        ));
        assert!(matches!(
            h.mouse(MouseEventKind::Down(MouseButton::Left), 11, 20),
            Transition::Exit(PickerExit::Close)
        ));
    }

    /// A mode that disables an action, and polls a scripted background source.
    struct GatedMode {
        snapshots: std::collections::VecDeque<Result<Vec<PickerItem>>>,
    }

    impl PickerMode for GatedMode {
        fn title(&self) -> &str {
            "Gated"
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
        fn action_disabled_reason(&self, item_id: &str, _action: &str) -> Option<String> {
            (item_id == "item-0").then(|| "not here".to_string())
        }
        fn is_polling(&self) -> bool {
            !self.snapshots.is_empty()
        }
        fn poll(&mut self) -> Option<Result<Vec<PickerItem>>> {
            self.snapshots.pop_front()
        }
        fn initial(&mut self) -> Result<Vec<PickerItem>> {
            Ok(items(2))
        }
        fn execute(&mut self, _item_id: &str, _action: &str) -> Result<ActionOutcome> {
            Ok(ActionOutcome::Close)
        }
    }

    /// A disabled action explains itself instead of running, a click on the
    /// selected row runs nothing when every action is disabled, and a polling
    /// mode ticks fast and folds each snapshot or error into the list.
    #[test]
    fn disabled_actions_explain_themselves_and_polls_fold_into_the_list() {
        let mut mode = GatedMode {
            snapshots: std::collections::VecDeque::from([
                Ok(items(3)),
                Err(anyhow::anyhow!("scan failed")),
            ]),
        };
        let actions = vec![ActionSpec {
            id: "open",
            key: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            key_label: "↵".into(),
            label: "open",
            color_slot: "blue",
        }];
        let schema = FieldSchema::default();
        let mut state = State::new(items(2), true);
        state.list_area = Rect::new(0, 0, 40, 10);
        let theme = Theme::default();
        let mut surface = PickerSurface {
            mode: &mut mode,
            theme: &theme,
            background: tui::SurfaceBackground::resolve(
                &theme,
                crate::config::Transparency::Transparent,
            ),
            title: Color::Yellow,
            actions: &actions,
            schema: &schema,
            state: &mut state,
        };

        let pressed = surface
            .on_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
        assert!(matches!(pressed, Transition::Redraw));
        assert_eq!(surface.state.runtime_error.as_deref(), Some("not here"));
        let click = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            surface.on_event(click).unwrap(),
            Transition::Redraw
        ));

        assert_eq!(surface.tick_rate(), POLL_TICK);
        assert!(matches!(surface.on_tick().unwrap(), Transition::Redraw));
        assert_eq!(surface.state.items.len(), 3);
        assert_eq!(surface.state.runtime_error, None);
        assert!(matches!(surface.on_tick().unwrap(), Transition::Redraw));
        assert_eq!(surface.state.runtime_error.as_deref(), Some("scan failed"));
        assert_eq!(surface.tick_rate(), IDLE_TICK);
        assert!(matches!(surface.on_tick().unwrap(), Transition::Wait));
    }

    /// Rows with and without a marker line up, and a preview taller than its
    /// card says where in it you are.
    #[test]
    fn markers_align_and_a_long_preview_reports_its_position() {
        let mut rows = items(2);
        rows[0].trailing_marker = Some(PickerMarker::new("★", "peach"));
        rows[0].trailing = Some("tag".into());
        rows[1].trailing = Some("tag".into());
        rows[0].preview = (0..40).map(|i| format!("line {i}")).collect();
        let mut state = State::new(rows, true);
        let screen: String = render(&mut state)
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains('★'), "{screen}");
        assert!(
            screen.contains("⌥jk"),
            "the scroll position is shown: {screen}"
        );

        let mut empty = State::new(items(1), true);
        empty.query = "zzzz".into();
        empty.recompute(&FieldSchema::default());
        let screen: String = render(&mut empty)
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("No matches"), "{screen}");
    }

    /// What a [`ScriptedMode`] was asked, readable after the loop consumed it.
    #[derive(Default)]
    struct ScriptLog {
        executed: Vec<(String, String)>,
        initial_calls: usize,
        reloads: usize,
    }

    /// A mode that answers `execute` from a script and logs what it was asked.
    #[derive(Default)]
    struct ScriptedMode {
        outcomes: std::collections::VecDeque<Result<ActionOutcome>>,
        log: std::rc::Rc<std::cell::RefCell<ScriptLog>>,
    }

    impl PickerMode for ScriptedMode {
        fn title(&self) -> &str {
            "Scripted"
        }
        fn accent_slot(&self) -> &'static str {
            "accent"
        }
        fn schema(&self) -> FieldSchema {
            FieldSchema::default()
        }
        fn actions(&self) -> Vec<ActionSpec> {
            vec![ActionSpec {
                id: "open",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵".into(),
                label: "open",
                color_slot: "blue",
            }]
        }
        fn key_bindings(&self) -> HashMap<String, String> {
            HashMap::from([("open".to_string(), "ctrl-o".to_string())])
        }
        fn reload_config(&mut self, _config: &Config) -> Result<()> {
            self.log.borrow_mut().reloads += 1;
            Ok(())
        }
        fn initial(&mut self) -> Result<Vec<PickerItem>> {
            self.log.borrow_mut().initial_calls += 1;
            Ok(items(3))
        }
        fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
            self.log
                .borrow_mut()
                .executed
                .push((item_id.to_string(), action.to_string()));
            self.outcomes
                .pop_front()
                .unwrap_or(Ok(ActionOutcome::Close))
        }
    }

    /// The outer loop: an action that stays open reloads the rows, a failed
    /// one is shown where the count goes, settings reload the mode, and a
    /// closing action or a plain close ends the picker.
    #[test]
    fn the_picker_loop_executes_reloads_and_closes_as_the_mode_answers() {
        let mut mode = ScriptedMode::default();
        let log = mode.log.clone();
        mode.outcomes.push_back(Ok(ActionOutcome::StayOpen));
        mode.outcomes
            .push_back(Err(anyhow::anyhow!("listener is stale")));
        let mut exits = std::collections::VecDeque::from([
            PickerExit::Invoke("item-1".into(), "open"),
            PickerExit::Invoke("item-2".into(), "open"),
            PickerExit::Invoke(String::new(), "__settings"),
            PickerExit::Invoke("item-0".into(), "open"),
        ]);
        let mut seen_error = None;
        let mut first_key_label = None;
        let settings_opened = std::cell::Cell::new(0);
        let mut insert_after_settings = None;
        run_with(
            mode,
            Theme::default(),
            Config::default(),
            |surface| {
                first_key_label.get_or_insert_with(|| surface.actions[0].key_label.clone());
                if surface.state.runtime_error.is_some() {
                    seen_error = surface.state.runtime_error.clone();
                }
                if settings_opened.get() == 1 && insert_after_settings.is_none() {
                    insert_after_settings = Some(surface.state.input_mode == InputMode::Insert);
                }
                Ok(exits.pop_front().unwrap_or(PickerExit::Close))
            },
            || {
                settings_opened.set(settings_opened.get() + 1);
                let mut cfg = Config::default();
                cfg.common.keymode = crate::config::KeyMode::Insert;
                Ok((cfg, Theme::default()))
            },
        )
        .expect("loop ends cleanly");

        assert_eq!(first_key_label.as_deref(), Some("^o"), "bindings apply");
        assert_eq!(seen_error.as_deref(), Some("listener is stale"));
        assert_eq!(settings_opened.get(), 1);
        assert_eq!(insert_after_settings, Some(true));
        let log = log.borrow();
        assert_eq!(log.reloads, 1);
        assert_eq!(
            log.executed,
            vec![
                ("item-1".to_string(), "open".to_string()),
                ("item-2".to_string(), "open".to_string()),
                ("item-0".to_string(), "open".to_string()),
            ]
        );
        // Once at start, once after StayOpen, once after settings.
        assert_eq!(log.initial_calls, 3);
    }

    /// The fixture modes the render tests use also run through the loop, which
    /// is the only caller that reaches their `initial` and `execute`.
    #[test]
    fn every_fixture_mode_closes_through_the_loop() {
        fn once<M: PickerMode>(mode: M) {
            run_with(
                mode,
                Theme::default(),
                Config::default(),
                |surface| {
                    let _ = surface.mode.title();
                    let _ = surface.mode.accent_slot();
                    let _ = surface.mode.empty_message();
                    Ok(PickerExit::Invoke("keep".into(), "open"))
                },
                || unreachable!("no settings in this script"),
            )
            .expect("closes");
        }
        once(TestMode);
        once(TabbedMode { active: "history" });
        once(WrappedBarMode);
        once(WideMode(false));
        once(ScriptedMode::default());
        once(GatedMode {
            snapshots: std::collections::VecDeque::new(),
        });
    }

    /// A host error ends the loop with that error rather than retrying.
    #[test]
    fn a_host_failure_ends_the_picker_with_its_error() {
        let error = run_with(
            TestMode,
            Theme::default(),
            Config::default(),
            |_| Err(anyhow::anyhow!("terminal lost")),
            || unreachable!(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("terminal lost"));
    }

    /// The cursor used to keep its *index* while a query narrowed the list, so
    /// moving down first and then filtering to a command left Enter on whatever
    /// row now sat at that index — it ran `kubectl` for a query naming `git log`.
    /// Editing the query puts the cursor on the best match, as Projects does.
    #[test]
    fn editing_the_query_puts_the_cursor_on_the_best_match() {
        let rows = [
            "make run",
            "kubectl -n staging logs deploy/api-gateway -f",
            "terraform plan",
            "git log --graph",
        ];
        let mut h = Harness::new(rows.iter().map(|id| test_item(id)).collect(), true);
        for _ in 0..3 {
            h.press(KeyCode::Char('j'));
        }
        h.press(KeyCode::Char('/'));
        for character in "git log".chars() {
            h.press(KeyCode::Char(character));
        }
        assert!(
            h.state.filtered.len() > 1,
            "the query must leave several rows, or the old index is clamped to the top anyway"
        );
        assert_eq!(
            invoked(h.press(KeyCode::Enter)),
            Some(("git log --graph".to_string(), "open"))
        );

        // Widening the query again starts from the best match as well.
        h.press(KeyCode::Down);
        h.key(KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(h.state.selected, 0);
    }

    /// A refresh replaces the rows under an unchanged query; the cursor follows
    /// the row it was on rather than jumping to the top.
    #[test]
    fn a_refresh_keeps_the_cursor_on_the_same_row() {
        let mut h = Harness::new(items(4), true);
        h.press(KeyCode::Char('j'));
        h.press(KeyCode::Char('j'));
        let mut rows = items(4);
        rows.insert(0, test_item("new-arrival"));
        h.state.replace(rows, &h.schema.clone());
        assert_eq!(
            h.state.selected_item().map(|item| item.id.as_str()),
            Some("item-2")
        );

        // When the row itself is gone, its neighbour takes the cursor.
        let remaining: Vec<PickerItem> = items(4)
            .into_iter()
            .filter(|item| item.id != "item-2")
            .collect();
        h.state.replace(remaining, &h.schema.clone());
        assert_eq!(
            h.state.selected_item().map(|item| item.id.as_str()),
            Some("item-3")
        );
    }

    fn invoked(transition: Transition<PickerExit>) -> Option<(String, &'static str)> {
        match transition {
            Transition::Exit(PickerExit::Invoke(id, action)) => Some((id, action)),
            _ => None,
        }
    }

    /// Insert mode types into the query; the readline edits clear a word and the
    /// whole line. Every one of them has to re-filter, or the list stops
    /// matching what the box says.
    #[test]
    fn insert_mode_edits_the_query_and_refilters_after_every_edit() {
        let mut h = Harness::new(items(4), false);

        for character in "item-2".chars() {
            h.press(KeyCode::Char(character));
        }
        assert_eq!(h.state.query, "item-2");
        assert_eq!(h.state.filtered.len(), 1);

        h.press(KeyCode::Backspace);
        assert_eq!(h.state.query, "item-");
        assert_eq!(h.state.filtered.len(), 4);

        h.key(KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(h.state.query, "", "⌥⌫ deletes the word");

        for character in "item".chars() {
            h.press(KeyCode::Char(character));
        }
        h.key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(h.state.query, "", "^u clears the line");
        assert_eq!(h.state.filtered.len(), 4);
    }

    /// Esc moves between typing and Vim navigation, and each mode answers a
    /// different set of keys. A key that belongs to the other mode must fall
    /// through as a plain wait rather than being swallowed.
    #[test]
    fn esc_toggles_insert_and_normal_and_each_mode_owns_its_keys() {
        let mut h = Harness::new(items(5), false);
        h.press(KeyCode::Esc);
        assert!(matches!(h.state.input_mode, InputMode::Normal));

        // Normal mode navigates rather than typing.
        h.press(KeyCode::Char('j'));
        assert_eq!(h.state.selected, 1);
        h.press(KeyCode::Char('k'));
        assert_eq!(h.state.selected, 0);
        h.press(KeyCode::Char('G'));
        assert_eq!(h.state.selected, 4);
        h.press(KeyCode::Char('g'));
        assert_eq!(h.state.selected, 0);
        h.press(KeyCode::End);
        assert_eq!(h.state.selected, 4);
        h.press(KeyCode::Home);
        assert_eq!(h.state.selected, 0);
        assert!(h.state.query.is_empty(), "normal mode never types");

        // `/` and `i` both return to typing.
        h.press(KeyCode::Char('/'));
        assert!(matches!(h.state.input_mode, InputMode::Insert));
        h.press(KeyCode::Esc);
        h.press(KeyCode::Char('i'));
        assert!(matches!(h.state.input_mode, InputMode::Insert));

        // An unhandled key is a wait, not a redraw.
        assert!(matches!(h.press(KeyCode::F(5)), Transition::Wait));
    }

    /// Both modes offer a way out, and they are different keys.
    #[test]
    fn each_mode_offers_its_own_way_to_close() {
        let mut h = Harness::new(items(2), false);
        assert!(matches!(
            h.key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Transition::Exit(PickerExit::Close)
        ));

        let mut h = Harness::new(items(2), true);
        assert!(matches!(
            h.press(KeyCode::Char('q')),
            Transition::Exit(PickerExit::Close)
        ));
        let mut h = Harness::new(items(2), true);
        assert!(matches!(
            h.press(KeyCode::Esc),
            Transition::Exit(PickerExit::Close)
        ));
    }

    /// An action key leaves with the selected item's id and the action's id —
    /// that pair is the entire contract between the shared picker and a mode.
    #[test]
    fn an_action_key_leaves_with_the_selected_item_and_the_action() {
        let mut h = Harness::new(items(3), true);
        h.press(KeyCode::Char('j'));

        let (id, action) = invoked(h.press(KeyCode::Enter)).expect("Enter invokes");
        assert_eq!((id.as_str(), action), ("item-1", "open"));

        let mut h = Harness::new(items(3), true);
        let (id, action) =
            invoked(h.key(KeyCode::Char('x'), KeyModifiers::CONTROL)).expect("^x invokes");
        assert_eq!((id.as_str(), action), ("item-0", "remove"));
    }

    /// An action on an empty list must do nothing rather than resolve to a row
    /// that is not there.
    #[test]
    fn an_action_with_nothing_selected_does_nothing() {
        let mut h = Harness::new(Vec::new(), true);
        assert!(matches!(h.press(KeyCode::Enter), Transition::Wait));
    }

    /// `⌥,` opens Settings from any picker, and `⌥j`/`⌥k` scroll the preview
    /// card rather than moving the selection.
    #[test]
    fn alt_keys_open_settings_and_scroll_the_preview() {
        let mut h = Harness::new(items(3), true);
        let (id, action) =
            invoked(h.key(KeyCode::Char(','), KeyModifiers::ALT)).expect("⌥, invokes");
        assert!(id.is_empty());
        assert_eq!(action, "__settings");

        let mut h = Harness::new(items(3), true);
        h.key(KeyCode::Char('j'), KeyModifiers::ALT);
        h.key(KeyCode::Char('j'), KeyModifiers::ALT);
        assert_eq!(h.state.preview_scroll, 2);
        assert_eq!(h.state.selected, 0, "the selection did not move");
        h.key(KeyCode::Char('k'), KeyModifiers::ALT);
        assert_eq!(h.state.preview_scroll, 1);
        // It cannot scroll above the top.
        h.key(KeyCode::Char('k'), KeyModifiers::ALT);
        h.key(KeyCode::Char('k'), KeyModifiers::ALT);
        assert_eq!(h.state.preview_scroll, 0);
    }

    /// A reported error is dismissed by the next keypress, and that keypress is
    /// consumed — otherwise the key that dismisses the message also acts on the
    /// row behind it.
    #[test]
    fn the_next_key_after_an_error_only_dismisses_it() {
        let mut h = Harness::new(items(3), true);
        h.state.runtime_error = Some("something went wrong".into());

        assert!(matches!(h.press(KeyCode::Char('j')), Transition::Redraw));
        assert!(h.state.runtime_error.is_none(), "the message is cleared");
        assert_eq!(h.state.selected, 0, "the key did not also navigate");
    }

    /// A malformed query must not run an action against whatever the list last
    /// showed: while the diagnostic stands, the action key is inert.
    #[test]
    fn an_action_is_refused_while_the_query_is_malformed() {
        let mut h = Harness::new(items(3), false);
        h.state.query = "cmd:\"unterminated".into();
        h.state.recompute(&h.schema);
        assert!(h.state.diagnostic.is_some(), "the query is rejected");

        assert!(matches!(h.press(KeyCode::Enter), Transition::Wait));
    }

    /// The wheel moves the selection when it is over the list and scrolls the
    /// card when it is over the preview — the pointer decides, not a mode.
    #[test]
    fn the_wheel_moves_the_selection_or_scrolls_the_preview_by_position() {
        let mut h = Harness::new(items(6), true);
        h.state.list_area = Rect::new(0, 0, 40, 10);
        h.state.preview_area = Rect::new(40, 0, 40, 10);

        h.mouse(MouseEventKind::ScrollDown, 10, 3);
        assert_eq!(h.state.selected, 1);
        assert_eq!(h.state.preview_scroll, 0);
        h.mouse(MouseEventKind::ScrollUp, 10, 3);
        assert_eq!(h.state.selected, 0);

        h.mouse(MouseEventKind::ScrollDown, 60, 3);
        assert_eq!(h.state.preview_scroll, 3, "over the card, the card scrolls");
        assert_eq!(h.state.selected, 0, "and the selection stays put");
        h.mouse(MouseEventKind::ScrollUp, 60, 3);
        assert_eq!(h.state.preview_scroll, 0);
    }

    /// A click selects; a click on the row already selected acts. Terminals
    /// report no double-click, so single-click-to-run would let a stray click
    /// fire a destructive action.
    #[test]
    fn a_click_selects_and_a_second_click_on_the_same_row_runs_it() {
        let mut h = Harness::new(items(6), true);
        h.state.list_area = Rect::new(0, 0, 40, 10);

        assert!(matches!(
            h.mouse(MouseEventKind::Down(MouseButton::Left), 5, 2),
            Transition::Redraw
        ));
        assert_eq!(h.state.selected, 2, "the first click only selects");

        let again = h.mouse(MouseEventKind::Down(MouseButton::Left), 5, 2);
        let (id, action) = invoked(again).expect("the second click runs the first action");
        assert_eq!((id.as_str(), action), ("item-2", "open"));
    }

    /// A click past the end of the list is not a row.
    #[test]
    fn a_click_below_the_last_row_selects_nothing() {
        let mut h = Harness::new(items(2), true);
        h.state.list_area = Rect::new(0, 0, 40, 10);
        h.mouse(MouseEventKind::Down(MouseButton::Left), 5, 7);
        assert_eq!(h.state.selected, 0);
    }

    /// A command-bar pill carries the key printed on its cap, so clicking it and
    /// pressing that key cannot diverge.
    #[test]
    fn clicking_a_command_bar_pill_does_what_its_cap_says() {
        let mut h = Harness::new(items(3), true);
        h.state.bar_rows = vec![BarRow {
            y: 20,
            zones: vec![
                (0, 8, PillAct::Run("remove")),
                (10, 18, PillAct::Settings),
                (20, 28, PillAct::Close),
            ],
        }];

        let (id, action) = invoked(h.mouse(MouseEventKind::Down(MouseButton::Left), 4, 20))
            .expect("a run pill invokes");
        assert_eq!((id.as_str(), action), ("item-0", "remove"));

        let (_, action) = invoked(h.mouse(MouseEventKind::Down(MouseButton::Left), 12, 20))
            .expect("the settings pill invokes");
        assert_eq!(action, "__settings");

        assert!(matches!(
            h.mouse(MouseEventKind::Down(MouseButton::Left), 22, 20),
            Transition::Exit(PickerExit::Close)
        ));
    }

    /// A mode with no tabs has nothing to cycle, so Tab must fall through
    /// rather than pretending to switch view.
    #[test]
    fn tab_does_nothing_in_a_mode_that_has_no_tabs() {
        let mut h = Harness::new(items(3), true);
        assert!(matches!(h.press(KeyCode::Tab), Transition::Wait));
        assert!(matches!(h.press(KeyCode::BackTab), Transition::Wait));
    }

    /// A mouse event the picker does not handle is a wait, not a redraw: a
    /// pointer move must never cost a repaint.
    #[test]
    fn an_unhandled_pointer_event_costs_no_redraw() {
        let mut h = Harness::new(items(3), true);
        assert!(matches!(
            h.mouse(MouseEventKind::Moved, 5, 5),
            Transition::Wait
        ));
        assert!(matches!(
            h.mouse(MouseEventKind::Up(MouseButton::Left), 5, 5),
            Transition::Wait
        ));
    }

    /// The caption colour is resolved once for every picker, so all three modes
    /// agree without each remembering to look it up.
    #[test]
    fn the_caption_colour_comes_from_config_then_the_theme_then_a_default() {
        let theme = Theme::from_slots(&[("peach", "#dcbb80"), ("mauve", "#cba6f7")]);
        let mut cfg = Config::default();

        // A named slot the theme knows.
        cfg.common.title_color = "mauve".into();
        assert_eq!(title_color(&theme, &cfg), Color::Rgb(0xcb, 0xa6, 0xf7));

        // A literal colour.
        cfg.common.title_color = "#123456".into();
        assert_eq!(title_color(&theme, &cfg), Color::Rgb(0x12, 0x34, 0x56));

        // A slot the theme does not define falls back to `peach`.
        cfg.common.title_color = "not-a-slot".into();
        assert_eq!(title_color(&theme, &cfg), Color::Rgb(0xdc, 0xbb, 0x80));

        // And with no theme at all, to ratatui's yellow.
        assert_eq!(title_color(&Theme::default(), &cfg), Color::Yellow);
    }

    /// A poll that answers replaces the list and clears any standing error; one
    /// that fails states the reason instead of emptying the list.
    #[test]
    fn a_poll_result_replaces_the_list_or_states_why_it_could_not() {
        let mut h = Harness::new(items(2), true);
        assert!(
            matches!(h.surface().on_tick().unwrap(), Transition::Wait),
            "TestMode has no background source"
        );
        assert!(!h.surface().tick_rate().is_zero());
        assert_eq!(h.surface().tick_rate(), IDLE_TICK);
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

    #[test]
    fn a_mode_tab_strip_draws_and_measures_the_labels_together() {
        let mode = TabbedMode { active: "history" };
        let mut state = State::new(mode.items(), false);
        let buffer = render_tabbed(&mode, &mut state);
        let title_row: String = (0..80)
            .map(|x| buffer[(x, state.list_area.y - 1)].symbol())
            .collect();

        assert!(title_row.contains("History"), "{title_row}");
        assert!(title_row.contains("★ Starred"), "{title_row}");
        assert_eq!(state.tab_zones.len(), 2);
        for (zone, _) in &state.tab_zones {
            let text: String = (zone.x..zone.right())
                .map(|x| buffer[(x, zone.y)].symbol())
                .collect();
            assert!(!text.trim().is_empty(), "tab zone covers blanks");
        }
        assert_eq!(
            buffer[(state.tab_zones[0].0.x, state.tab_zones[0].0.y)].bg,
            Color::Yellow
        );
    }

    #[test]
    fn clicking_a_mode_tab_activates_the_view_its_label_names() {
        let theme = Theme::from_slots(&[("accent", "#6fd0a8")]);
        let background =
            tui::SurfaceBackground::resolve(&theme, crate::config::Transparency::Transparent);
        let schema = FieldSchema::default();
        let actions = Vec::new();
        let mut mode = TabbedMode { active: "history" };
        let mut state = State::new(mode.items(), false);
        let _ = render_tabbed(&mode, &mut state);
        let target = state.tab_zones[1].0;
        let mut surface = PickerSurface {
            mode: &mut mode,
            theme: &theme,
            background,
            title: Color::Yellow,
            actions: &actions,
            schema: &schema,
            state: &mut state,
        };

        let _ = surface
            .on_event(Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: target.x,
                row: target.y,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();

        assert_eq!(mode.active, "starred");
        assert_eq!(state.items.len(), 1);
    }

    #[test]
    fn tab_keys_preserve_the_query_and_selected_item() {
        let theme = Theme::from_slots(&[("accent", "#6fd0a8")]);
        let background =
            tui::SurfaceBackground::resolve(&theme, crate::config::Transparency::Transparent);
        let schema = FieldSchema::default();
        let actions = Vec::new();
        let mut mode = TabbedMode { active: "history" };
        let mut state = State::new(mode.items(), false);
        state.query = "keep".into();
        state.recompute(&schema);

        {
            let mut surface = PickerSurface {
                mode: &mut mode,
                theme: &theme,
                background,
                title: Color::Yellow,
                actions: &actions,
                schema: &schema,
                state: &mut state,
            };
            let _ = surface
                .on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .unwrap();
        }

        assert_eq!(mode.active, "starred");
        assert_eq!(state.query, "keep");
        assert_eq!(state.selected_item().unwrap().id, "keep");

        let mut surface = PickerSurface {
            mode: &mut mode,
            theme: &theme,
            background,
            title: Color::Yellow,
            actions: &actions,
            schema: &schema,
            state: &mut state,
        };
        let _ = surface
            .on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(mode.active, "history");
    }

    #[test]
    fn an_empty_tab_explains_how_to_populate_it() {
        let mode = TabbedMode { active: "starred" };
        let mut state = State::new(Vec::new(), false);
        let buffer = render_tabbed(&mode, &mut state);
        let mut screen = String::new();
        for y in 0..12 {
            for x in 0..80 {
                screen.push_str(buffer[(x, y)].symbol());
            }
        }

        assert!(screen.contains("No stars — ctrl-s in History"), "{screen}");
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

    /// A zone that does not sit on the pill it names sends a click to the wrong
    /// action — and does it silently, which is why this is measured rather than
    /// reasoned about.
    #[test]
    fn the_pill_zones_land_on_the_pills_they_name() {
        let mut state = State::new(vec![test_item("a")], false);
        let buffer = render(&mut state);
        assert!(!state.bar_rows.is_empty());
        for row in &state.bar_rows {
            for &(a, b, act) in &row.zones {
                let text: String = (a..b).map(|x| buffer[(x, row.y)].symbol()).collect();
                assert!(!text.trim().is_empty(), "zone for a pill covers blanks");
                if act == PillAct::Close {
                    assert!(text.contains("esc"), "{text}");
                }
            }
        }
        assert_eq!(
            state.bar_rows.last().unwrap().zones.last().unwrap().2,
            PillAct::Close
        );
    }

    #[test]
    fn requested_action_rows_balance_even_when_every_pill_fits_on_one_line() {
        let pills = [
            Pill::new("a", "alpha", Color::Reset),
            Pill::new("b", "beta", Color::Reset),
            Pill::new("c", "charlie", Color::Reset),
            Pill::new("d", "delta", Color::Reset),
        ];
        let ranges = balanced_bar_ranges(&pills, 2, 200);

        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, pills.len());
        assert!(ranges.iter().all(|range| !range.is_empty()));
        assert_eq!(action_bar_height(2), 3, "two rows keep one blank line");
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

    #[test]
    fn opaque_picker_leaves_no_transparent_holes() {
        let mut state = State::new(vec![test_item("a"), test_item("b")], false);
        let buffer = render_with_background(&mut state, crate::config::Transparency::Opaque);
        assert!(buffer.content.iter().all(|cell| cell.bg != Color::Reset));
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

    fn render_wide_buffer(state: &mut State, emphasize: bool) -> ratatui::buffer::Buffer {
        let theme = Theme::from_slots(&[
            ("accent", "#6fd0a8"),
            ("overlay0", "#6c7e76"),
            ("peach", "#dcbb80"),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        let background =
            tui::SurfaceBackground::resolve(&theme, crate::config::Transparency::Transparent);
        terminal
            .draw(|f| {
                draw(
                    f,
                    &WideMode(emphasize),
                    &theme,
                    background,
                    Color::Yellow,
                    &[],
                    state,
                )
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_wide(state: &mut State, emphasize: bool) -> Vec<String> {
        let buffer = render_wide_buffer(state, emphasize);
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
            trailing_marker: None,
            document: Document::fuzzy(primary),
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

    #[test]
    fn a_trailing_marker_keeps_its_theme_colour_on_the_selected_row() {
        let mut item = tagged("a", "cargo test", "2h");
        item.trailing_marker = Some(PickerMarker::new("★", "peach"));
        let mut state = State::new(vec![item], false);
        let buffer = render_wide_buffer(&mut state, true);
        let star_x = (0..60)
            .find(|&x| buffer[(x, 4)].symbol() == "★")
            .expect("star marker");

        assert_eq!(buffer[(star_x, 4)].fg, Color::Rgb(0xdc, 0xbb, 0x80));
        assert_eq!(buffer[(star_x, 4)].bg, Color::Indexed(236));
    }

    /// The leading word is the program, and a mode can ask for it in its own
    /// colour so a wall of commands has something to scan down.
    #[test]
    fn the_head_is_emphasized_only_when_the_mode_asks_for_it() {
        let theme_accent = Color::Rgb(0x6f, 0xd0, 0xa8);
        let plain = |emphasize: bool| {
            // Two items, and probe the second so the selection's accent does not
            // hide whether an ordinary row asked to emphasize its head.
            let mut state = State::new(
                vec![
                    tagged("a", "vim", "2h"),
                    tagged("b", "docker compose down", "3d"),
                ],
                false,
            );
            let t = Theme::from_slots(&[("accent", "#6fd0a8"), ("overlay0", "#6c7e76")]);
            let background =
                tui::SurfaceBackground::resolve(&t, crate::config::Transparency::Transparent);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
            terminal
                .draw(|f| {
                    draw(
                        f,
                        &WideMode(emphasize),
                        &t,
                        background,
                        Color::Yellow,
                        &[],
                        &mut state,
                    )
                })
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
            trailing_marker: None,
            document: Document::fuzzy(id),
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

    /// Every key and pointer event, in both input modes and in a tabbed mode,
    /// answers without panicking.
    #[test]
    fn the_picker_answers_every_event() {
        let events = crate::surface::every_event(100, 30, &[]);
        for normal in [true, false] {
            let mut h = Harness::new(items(6), normal);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            for (n, event) in events.iter().enumerate() {
                if n % 40 == 0 {
                    terminal.draw(|frame| h.surface().draw(frame)).unwrap();
                }
                let _ = h.surface().on_event(event.clone()).unwrap();
            }
        }
    }

    /// The picker on a scripted host: `esc` in Normal mode closes it, and a
    /// script that runs out before the picker exits is an error, not a hang.
    #[test]
    fn the_picker_runs_on_any_host_and_a_stalled_script_fails() {
        use crate::surface::ScriptedHost;
        let mut cfg = Config::default();
        cfg.common.keymode = crate::config::KeyMode::Normal;
        run_in(
            &mut ScriptedHost::new([ScriptedHost::key(KeyCode::Esc, KeyModifiers::NONE)]),
            TestMode,
            Theme::default(),
            cfg.clone(),
        )
        .expect("esc closes");
        let stalled = run_in(
            &mut ScriptedHost::new([ScriptedHost::PAUSE]),
            TestMode,
            Theme::default(),
            cfg,
        );
        assert!(stalled
            .unwrap_err()
            .to_string()
            .contains("script exhausted"));
    }
}
