//! herdr-switchboard — a unified herdr switcher TUI (agents, workspaces, ghq
//! repos) with fuzzy search, a live preview, and a full-width command bar.

mod action;
mod agents;
mod changelog;
mod chrome;
mod commands;
mod config;
mod data;
mod git;
mod history;
mod keymap;
mod markdown;
mod menu;
mod notify;
mod picker;
mod ports;
mod preview;
mod query;
mod runner;
mod settings;
mod socket;
mod source;
mod splash;
mod state;
mod trace;
mod tui;
mod ui;
mod update;
mod usage;
mod zen;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher, Utf32Str};
use ratatui::layout::{Position, Rect};
use ratatui::text::Text;
use ratatui::widgets::ListState;

use action::Accept;
use data::{Config, Entry, GroupFilter, Kind, SortMode, Theme};

/// The searchable model: the entries, the query and its result, and the two
/// orderings (group filter + sort) applied to the resting list. Everything here
/// answers "what is in the list and which row is selected", nothing about how it
/// is drawn.
pub struct Picker {
    pub entries: Vec<Entry>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub query: String,
    matcher: Matcher,
    pub group: GroupFilter,
    pub sort: SortMode,
    /// id → last-opened epoch, for the Recent sort.
    recent: HashMap<String, u64>,
    /// Kinds actually present, in tab order — drives group cycling + the strip.
    pub present_kinds: Vec<Kind>,
}

/// The async preview pipeline and everything the pane needs to draw and scroll.
/// The [`Worker`](preview::Worker) renders off-thread; the rest is the shown
/// card, where it sits, and how far it is scrolled.
pub struct PreviewState {
    pub text: Text<'static>,
    /// Id of the entry the shown card is for, so a re-request for the same entry
    /// is a no-op.
    id: String,
    worker: preview::Worker,
    /// Seq of the newest render requested; results tagged older are stale.
    seq: u64,
    /// A render is queued or running, so the shown preview is one entry behind.
    pending: bool,
    /// When the in-flight render started, for the placeholder's grace + phase.
    since: Option<Instant>,
    /// Name of the entry being rendered, shown under the placeholder spinner.
    pub label: String,
    pub enabled: bool,
    pub position: String,
    pub pct: u16,
    pub scroll: u16,
    /// Where the preview pane sat at the last draw, `None` before the first one.
    /// One rect answers three questions — how wide to build the card, how many
    /// rows can show it, and whether the pointer is over it — so they cannot
    /// disagree. Only the layout knows it, which is why `run` draws before it
    /// calls `request_preview`.
    pub area: Option<Rect>,
    /// Rows the current card occupies. Because the card clips rather than wraps,
    /// one card line is one screen row, so this and [`PreviewState::rows`] bound
    /// the scroll exactly.
    pub len: u16,
}

/// The `⌥c` changelog popup: its parsed blocks and scroll position. Parsed on
/// first open, not at startup — most sessions never press `⌥c`.
pub struct ChangelogState {
    pub show: bool,
    pub blocks: Vec<markdown::Block>,
    pub scroll: u16,
    /// Rendered rows and visible rows at the last draw, so scrolling can stop.
    pub len: u16,
    pub rows: u16,
}

/// Click targets published by the last draw, so a pointer event can be turned
/// back into the thing under it. Every field here is written by `ui::draw` and
/// read by the hit-testers — that write-back is deliberate (a zone measured by
/// the loop that draws it cannot drift), which is why these live together rather
/// than being recomputed per event.
pub struct HitZones {
    /// Where the list sat at the last draw, and the state it was drawn with. The
    /// state is kept rather than rebuilt per frame because its scroll offset is
    /// the only thing that can turn a clicked row back into an entry.
    pub list_area: Rect,
    pub list_state: ListState,
    /// The group tabs along the list's top border.
    pub tab_zones: Vec<(u16, u16, GroupFilter)>,
    /// The command bar's pills, each carrying the action it runs when clicked.
    pub footer_zones: Vec<(u16, u16, keymap::Action)>,
    /// The command bar's row — it is one row tall, so this plus a zone's `x`
    /// span is the whole hit test.
    pub footer_row: u16,
}

pub struct App {
    pub theme: Theme,
    pub title_color: ratatui::style::Color,
    pub cfg: Config,
    pub script_dir: String,
    pub show_help: bool,
    /// A newer version the cache knows about; shown, never acted on.
    pub update: Option<String>,
    /// Chord → action table, built from defaults + `keys.*` config overrides.
    pub keymap: keymap::Keymap,
    /// Insert (type-to-filter) or Normal (Vim). Esc toggles between them.
    pub mode: keymap::Mode,
    /// True after the Normal-mode leader (`␣`) is pressed, waiting for the next
    /// key to select a leader action.
    pub leader_pending: bool,
    pub picker: Picker,
    pub preview: PreviewState,
    pub changelog: ChangelogState,
    /// The settings form, drawn as a floating overlay when `settings.show`.
    pub settings: settings::Settings,
    pub zones: HitZones,
}

enum Flow {
    Continue,
    Quit,
    Accept(Accept),
}

impl Picker {
    fn new(entries: Vec<Entry>, sort: SortMode, recent: HashMap<String, u64>) -> Self {
        let present_kinds = source::kinds()
            .into_iter()
            .filter(|&k| entries.iter().any(|e| e.kind == k))
            .collect();
        let mut picker = Picker {
            entries,
            filtered: Vec::new(),
            selected: 0,
            query: String::new(),
            matcher: Matcher::new(NucleoConfig::DEFAULT),
            group: GroupFilter::All,
            sort,
            recent,
            present_kinds,
        };
        // Apply the initial sort (Recent by default) to the resting list.
        picker.recompute();
        picker
    }

    fn recompute(&mut self) {
        let group = self.group;
        if self.query.is_empty() {
            // Browse mode: filter by group, then order by the active sort.
            self.filtered = browse_order(&self.entries, &self.recent, group, self.sort);
        } else {
            // Search mode: fuzzy score wins; group still narrows the candidates.
            let pat = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = Vec::new();
            for (i, e) in self.entries.iter().enumerate() {
                if !group.matches(e.kind) {
                    continue;
                }
                buf.clear();
                if let Some(score) =
                    pat.score(Utf32Str::new(&e.search, &mut buf), &mut self.matcher)
                {
                    scored.push((score, i));
                }
            }
            scored.sort_by_key(|&(score, _)| Reverse(score));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = 0;
    }

    /// The tab strip in order: All, then each present kind.
    pub fn tabs(&self) -> Vec<GroupFilter> {
        let mut v = vec![GroupFilter::All];
        v.extend(self.present_kinds.iter().map(|&k| GroupFilter::Only(k)));
        v
    }

    /// Select a configured group when it exists; disabled/empty groups fall back
    /// to All so startup and a live settings apply never produce an empty picker.
    fn select_group_or_all(&mut self, requested: GroupFilter) {
        self.group = match requested {
            GroupFilter::Only(kind) if !self.present_kinds.contains(&kind) => GroupFilter::All,
            group => group,
        };
        self.recompute();
    }

    /// Move to the next/previous non-empty group (wraps).
    fn cycle_group(&mut self, dir: i32) {
        let tabs = self.tabs();
        if tabs.len() < 2 {
            return;
        }
        let cur = tabs.iter().position(|&g| g == self.group).unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(tabs.len() as i32);
        self.group = tabs[next as usize];
        self.recompute();
    }

    fn move_sel(&mut self, delta: i32) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let cur = self.selected as i32;
        let next = (cur + delta).rem_euclid(n as i32);
        self.selected = next as usize;
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.filtered.get(self.selected).map(|&i| &self.entries[i])
    }

    /// The entry index behind the current selection, if any.
    fn selected_index(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }
}

impl PreviewState {
    fn new(cfg: &Config, theme: &Theme, script_dir: &str) -> Self {
        let enabled = cfg.get("preview", "enabled") != "disabled";
        let position = cfg.get("preview_position", "right");
        let pct = cfg
            .get("preview_size", "60%")
            .trim_end_matches('%')
            .parse::<u16>()
            .unwrap_or(52)
            .clamp(20, 80);
        let worker = preview::Worker::spawn(script_dir.to_string(), cfg.clone(), theme.clone());
        PreviewState {
            text: Text::default(),
            id: String::new(),
            worker,
            seq: 0,
            pending: false,
            since: None,
            label: String::new(),
            enabled,
            position,
            pct,
            scroll: 0,
            // Filled by the first draw, which always precedes the first request.
            area: None,
            len: 0,
        }
    }

    /// Width the card is built to: the preview pane's interior, less its border.
    /// Before the first draw there is no pane to measure, so guess a common one —
    /// the next draw publishes the real width and the card is rebuilt to it.
    fn width(&self) -> u16 {
        self.area.map_or(60, |a| a.width.saturating_sub(2))
    }

    /// Rows of card the pane can show at once.
    pub fn rows(&self) -> u16 {
        self.area.map_or(1, |a| a.height.saturating_sub(2))
    }

    /// Scroll the preview, stopping at both ends. The list keeps `^j`/`^k`, so
    /// the preview takes the `⌥` pair: the same fingers, the other pane.
    fn scroll_by(&mut self, delta: i32) {
        let max = self.len.saturating_sub(self.rows()) as i32;
        self.scroll = (self.scroll as i32 + delta).clamp(0, max) as u16;
    }

    fn toggle(&mut self) {
        self.enabled = !self.enabled;
        if self.enabled {
            // Force `request` to re-queue for the current selection.
            self.id.clear();
        }
    }

    /// Queues a render for `entry` if it differs from the shown card. Never
    /// blocks: the worker renders while the UI keeps taking keys.
    fn request(&mut self, entry: &Entry) {
        if entry.id == self.id {
            return;
        }
        self.id = entry.id.clone();
        if !self.enabled {
            return;
        }
        self.seq += 1;
        self.label = entry.primary.clone();
        self.pending = self.worker.request(self.seq, entry.clone(), self.width());
        self.since = Some(Instant::now());
    }

    fn pending(&self) -> bool {
        self.pending
    }

    /// Frame index for the pending placeholder, or `None` when the shown
    /// preview is current. Renders that finish inside `PLACEHOLDER_GRACE` —
    /// agents, small repos — never reach frame 0, so the pane doesn't flash.
    pub fn placeholder_frame(&self) -> Option<usize> {
        if !self.pending {
            return None;
        }
        let waited = self.since?.elapsed().checked_sub(PLACEHOLDER_GRACE)?;
        Some((waited.as_millis() / PLACEHOLDER_FRAME.as_millis()) as usize)
    }

    /// Installs a finished preview, reporting whether the UI needs a redraw.
    /// Results for entries already scrolled past are dropped.
    fn absorb(&mut self) -> bool {
        let mut installed = false;
        while let Some(done) = self.worker.poll() {
            if done.seq != self.seq {
                continue; // stale: the selection moved on
            }
            self.len = done.text.lines.len() as u16;
            self.text = done.text;
            // A new card starts at the top: the offset belonged to the old one.
            self.scroll = 0;
            self.pending = false;
            installed = true;
        }
        installed
    }
}

impl ChangelogState {
    fn new() -> Self {
        ChangelogState {
            show: false,
            blocks: Vec::new(),
            scroll: 0,
            len: 0,
            rows: 1,
        }
    }

    /// Parse the changelog the first time it is asked for. A failure leaves the popup
    /// open with a single line saying so, rather than a blank box.
    fn open(&mut self) {
        if self.blocks.is_empty() {
            self.blocks = match changelog::changelog_text() {
                Ok(text) => markdown::parse(&text),
                Err(e) => markdown::parse(&format!("## [unavailable]\n\n- {e}\n")),
            };
        }
        self.scroll = 0;
        self.show = true;
    }
}

impl HitZones {
    fn new() -> Self {
        HitZones {
            // A zero rect contains no point, so clicks land nowhere until the
            // first draw says where things are.
            list_area: Rect::default(),
            list_state: ListState::default(),
            tab_zones: Vec::new(),
            footer_zones: Vec::new(),
            footer_row: 0,
        }
    }
}

impl App {
    fn new(entries: Vec<Entry>, theme: Theme, cfg: Config, script_dir: String) -> Self {
        let title_color = theme
            .resolve(&cfg.get("title_color", "peach"))
            .unwrap_or_else(|| theme.or("accent", ratatui::style::Color::Cyan));
        let sort = SortMode::parse(&cfg.get("sort", "recent"));
        // Read before `cfg` moves into the struct.
        let update = update::available(&cfg);
        let recent = history::load();
        let preview = PreviewState::new(&cfg, &theme, &script_dir);
        let mut picker = Picker::new(entries, sort, recent);
        picker.select_group_or_all(GroupFilter::parse(&cfg.get("default_tab", "all")));
        let keymap = keymap::Keymap::load(&cfg);
        let mode = keymap.start_mode();
        // Seed the settings form from the same cfg before it moves into the struct.
        let settings = settings::Settings::new(&cfg);
        App {
            theme,
            title_color,
            cfg,
            script_dir,
            show_help: false,
            update,
            keymap,
            mode,
            leader_pending: false,
            picker,
            preview,
            changelog: ChangelogState::new(),
            settings,
            zones: HitZones::new(),
        }
    }

    /// Queues a preview render for the current selection if it changed.
    fn request_preview(&mut self) {
        let Some(idx) = self.picker.selected_index() else {
            return;
        };
        let entry = self.picker.entries[idx].clone();
        self.preview.request(&entry);
    }

    /// Re-read `config.toml` and re-derive the runtime state that depends on it, so a
    /// setting applied in the overlay takes effect in this session rather than on the
    /// next launch. Called after `Settings::apply` reports it wrote something.
    ///
    /// The entry list is reloaded too, so the source toggles and label style update
    /// live; that resettles the selection at the top the way a `sort` change reorders
    /// it anyway. `mode` is left as the user has it — `keymode` only picks the *start*
    /// mode.
    fn reload_config(&mut self) {
        let runner = runner::SystemRunner;
        let cfg = Config::load();
        let default_tab_changed =
            self.cfg.get("default_tab", "all") != cfg.get("default_tab", "all");

        self.title_color = self
            .theme
            .resolve(&cfg.get("title_color", "peach"))
            .unwrap_or_else(|| self.theme.or("accent", ratatui::style::Color::Cyan));
        self.picker.sort = SortMode::parse(&cfg.get("sort", "recent"));
        self.keymap = keymap::Keymap::load(&cfg);

        // Preview geometry is read straight from these fields at draw time; the readme
        // toggle lives in the worker's config, so respawn it and force a re-render.
        self.preview.enabled = cfg.get("preview", "enabled") != "disabled";
        self.preview.position = cfg.get("preview_position", "right");
        self.preview.pct = cfg
            .get("preview_size", "60%")
            .trim_end_matches('%')
            .parse::<u16>()
            .unwrap_or(52)
            .clamp(20, 80);
        self.preview.worker =
            preview::Worker::spawn(self.script_dir.clone(), cfg.clone(), self.theme.clone());
        self.preview.id.clear();

        // Reload entries so the source toggles and label style apply.
        let root = data::ghq_root(&runner);
        let ctx = source::LoadCtx {
            runner: &runner,
            theme: &self.theme,
            root: &root,
        };
        let entries = source::load_all(&cfg, &ctx);
        self.picker.present_kinds = source::kinds()
            .into_iter()
            .filter(|&k| entries.iter().any(|e| e.kind == k))
            .collect();
        self.picker.entries = entries;
        let requested = if default_tab_changed {
            GroupFilter::parse(&cfg.get("default_tab", "all"))
        } else {
            self.picker.group
        };
        self.picker.select_group_or_all(requested);

        self.cfg = cfg;
    }

    /// The entry drawn at screen row `y`, if that row holds one. Rows map back
    /// through the offset the list was last drawn with — the first visible row
    /// is `offset`, not 0, which is why the [`ListState`] is kept across frames.
    fn entry_at(&self, y: u16) -> Option<usize> {
        // The block's top border is the tab strip, not a row of the list.
        let first = self.zones.list_area.y + 1;
        let row = y.checked_sub(first)? as usize;
        let idx = self.zones.list_state.offset() + row;
        (idx < self.picker.filtered.len()).then_some(idx)
    }

    /// A left click. Selects an entry, switches a group, or runs a command —
    /// whatever it landed on.
    fn on_click(&mut self, at: Position) -> Flow {
        // A popup is modal: the click dismisses it and means nothing else, the
        // way any key does.
        if self.show_help {
            self.show_help = false;
            return Flow::Continue;
        }
        if self.changelog.show {
            self.changelog.show = false;
            return Flow::Continue;
        }
        // The settings form is modal: inside the card the pointer picks a tab, a
        // row or a pill; outside it the click dismisses. Dismissing goes through
        // `close_discarding` so a staged edit cannot survive the close — `esc`
        // discards, and a click that closed without discarding left the draft
        // alive behind a form that looked shut.
        if self.settings.show {
            if self.settings.hit(at) {
                if self.settings.on_click(at) {
                    self.reload_config();
                }
            } else {
                self.settings.close_discarding();
            }
            return Flow::Continue;
        }
        // The command bar: one row, so the x span is the whole test. A pill runs
        // its action, the same as its key would.
        if let Some(action) =
            crate::tui::zone_at(&self.zones.footer_zones, self.zones.footer_row, at)
        {
            // Accepting on nothing would be a no-op with a confirmation prompt.
            if action.is_accept() && self.picker.selected_entry().is_none() {
                return Flow::Continue;
            }
            return apply_action(self, action);
        }
        // The tab strip rides the list's top border.
        if at.y == self.zones.list_area.y {
            if let Some(&(_, _, g)) = self
                .zones
                .tab_zones
                .iter()
                .find(|&&(a, b, _)| at.x >= a && at.x < b)
            {
                if g != self.picker.group {
                    self.picker.group = g;
                    self.picker.recompute();
                }
                return Flow::Continue;
            }
        }
        if self.zones.list_area.contains(at) {
            if let Some(idx) = self.entry_at(at.y) {
                self.picker.selected = idx;
            }
        }
        Flow::Continue
    }

    /// A wheel turn moves the pane under the pointer: the card when it is over
    /// the preview, the selection anywhere else. Reports whether anything moved,
    /// so the caller can skip a redraw for a wheel over dead space.
    fn on_wheel(&mut self, at: Position, delta: i32) -> bool {
        // A modal popup takes the wheel first: it is what the pointer is over,
        // whatever is drawn underneath.
        if self.settings.show {
            self.settings.on_wheel(delta as isize);
            return true;
        }
        if self.changelog.show {
            let c = &mut self.changelog;
            let max = c.len.saturating_sub(c.rows);
            c.scroll = if delta > 0 {
                (c.scroll + 3).min(max)
            } else {
                c.scroll.saturating_sub(3)
            };
            return true;
        }
        let over_preview =
            self.preview.enabled && self.preview.area.is_some_and(|a| a.contains(at));
        if over_preview {
            let before = self.preview.scroll;
            // Three rows a notch: the conventional feel for text, and the card
            // is long enough that one row at a time would be a chore.
            self.preview.scroll_by(delta * 3);
            self.preview.scroll != before
        } else {
            // One entry a notch: the list is a menu, and overshooting it costs
            // a preview render.
            self.picker.move_sel(delta.signum());
            true
        }
    }

    /// Worktrees are openable and reviewable, but repository update/removal has
    /// different semantics and is intentionally unavailable for them.
    pub fn action_available(&self, action: keymap::Action) -> bool {
        let unsupported = matches!(
            action,
            keymap::Action::Accept(Accept::Update | Accept::Remove)
        );
        !(unsupported
            && self
                .picker
                .selected_entry()
                .is_some_and(|entry| entry.kind == Kind::Worktree))
    }
}

/// The no-query browse order: entries passing `group`, ordered by `sort`.
/// Ties break on original load order so the list is stable.
fn browse_order(
    entries: &[Entry],
    recent: &HashMap<String, u64>,
    group: GroupFilter,
    sort: SortMode,
) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len())
        .filter(|&i| group.matches(entries[i].kind))
        .collect();
    match sort {
        SortMode::Recent => idx.sort_by(|&a, &b| {
            let ra = recent.get(&entries[a].id).copied().unwrap_or(0);
            let rb = recent.get(&entries[b].id).copied().unwrap_or(0);
            rb.cmp(&ra).then(a.cmp(&b))
        }),
        SortMode::Name => idx.sort_by(|&a, &b| {
            entries[a]
                .primary
                .to_lowercase()
                .cmp(&entries[b].primary.to_lowercase())
                .then(a.cmp(&b))
        }),
        SortMode::Kind => idx.sort_by(|&a, &b| {
            entries[a]
                .kind
                .order()
                .cmp(&entries[b].kind.order())
                .then(a.cmp(&b))
        }),
    }
    idx
}

/// Delete the word before the cursor: trailing spaces, then the run of
/// non-spaces — the readline `^w` a query editor expects.
fn delete_word(q: &mut String) {
    while q.ends_with(' ') {
        q.pop();
    }
    while !q.is_empty() && !q.ends_with(' ') {
        q.pop();
    }
}

/// Run a resolved [`keymap::Action`] against the app.
fn apply_action(app: &mut App, action: keymap::Action) -> Flow {
    use keymap::Action;
    if !app.action_available(action) {
        return Flow::Continue;
    }
    match action {
        Action::Quit => return Flow::Quit,
        Action::Help => app.show_help = true,
        Action::Changelog => app.changelog.open(),
        Action::Settings => app.settings.open(),
        Action::NextGroup => app.picker.cycle_group(1),
        Action::PrevGroup => app.picker.cycle_group(-1),
        Action::Down => app.picker.move_sel(1),
        Action::Up => app.picker.move_sel(-1),
        Action::PageDown => app.picker.move_sel(10),
        Action::PageUp => app.picker.move_sel(-10),
        Action::Top => app.picker.selected = 0,
        Action::Bottom => app.picker.selected = app.picker.filtered.len().saturating_sub(1),
        Action::TogglePreview => app.preview.toggle(),
        Action::PreviewDown => app.preview.scroll_by(1),
        Action::PreviewUp => app.preview.scroll_by(-1),
        Action::CycleSort => {
            app.picker.sort = app.picker.sort.next();
            app.picker.recompute();
        }
        Action::Backspace => {
            app.picker.query.pop();
            app.picker.recompute();
        }
        Action::ClearQuery => {
            app.picker.query.clear();
            app.picker.recompute();
        }
        Action::DeleteWord => {
            delete_word(&mut app.picker.query);
            app.picker.recompute();
        }
        Action::EnterInsert => {
            app.mode = keymap::Mode::Insert;
            app.leader_pending = false;
        }
        Action::EnterNormal => app.mode = keymap::Mode::Normal,
        Action::Accept(a) => return Flow::Accept(a),
    }
    Flow::Continue
}

fn handle_key(app: &mut App, k: crossterm::event::KeyEvent) -> Flow {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

    // The settings overlay is a form: while open it owns navigation, Enter (cycle),
    // and the in-place split_ratio edit, so route every key to it. `esc`/`q` close it
    // from inside `on_key`; `^c` still quits the picker so you are never trapped.
    if app.settings.show {
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            return Flow::Quit;
        }
        // An apply persisted a change: re-read config.toml and re-derive the live
        // state so the new value takes effect now, not on the next launch.
        if app.settings.on_key(k) {
            app.reload_config();
        }
        return Flow::Continue;
    }

    // The changelog popup scrolls, so it cannot dismiss on any key the way the help
    // cheatsheet does; esc/q closes it and the movement keys drive it.
    if app.changelog.show {
        let c = &mut app.changelog;
        let page = c.rows.saturating_sub(2).max(1);
        let max = c.len.saturating_sub(c.rows);
        match k.code {
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => c.show = false,
            KeyCode::Down | KeyCode::Char('j') => c.scroll = (c.scroll + 1).min(max),
            KeyCode::Up | KeyCode::Char('k') => c.scroll = c.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => c.scroll = (c.scroll + page).min(max),
            KeyCode::PageUp => c.scroll = c.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => c.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => c.scroll = max,
            _ => {}
        }
        return Flow::Continue;
    }

    // While the help popup is open, swallow every key: the first press just
    // dismisses it (^c still quits, so you're never trapped).
    if app.show_help {
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            return Flow::Quit;
        }
        app.show_help = false;
        return Flow::Continue;
    }

    let Some(ch) = keymap::chord_of(&k) else {
        return Flow::Continue;
    };

    // Normal-mode leader: `␣` arms it, the next key picks a leader action. An
    // unbound follow-up just disarms — the leader never traps you.
    if app.mode == keymap::Mode::Normal {
        if app.leader_pending {
            app.leader_pending = false;
            if let Some(action) = app.keymap.leader_action(ch) {
                return apply_action(app, action);
            }
            return Flow::Continue;
        }
        if ch == app.keymap.leader_chord {
            app.leader_pending = true;
            return Flow::Continue;
        }
    }

    if let Some(action) = app.keymap.action(app.mode, ch) {
        return apply_action(app, action);
    }
    // Unbound: in Insert mode a plain printable key types into the query. In
    // Normal mode an unbound key does nothing — the list is driven by commands.
    if app.mode == keymap::Mode::Insert && !ch.ctrl && !ch.alt {
        if let keymap::Key::Char(c) = ch.key {
            app.picker.query.push(c);
            app.picker.recompute();
        }
    }
    Flow::Continue
}

/// Wake cadence while a preview render is in flight — short enough that the
/// result appears promptly, long enough to cost nothing.
const PREVIEW_TICK: Duration = Duration::from_millis(16);
/// Wake cadence with nothing in flight; the loop is just parked on the keyboard.
const IDLE_TICK: Duration = Duration::from_secs(1);
/// How long a render may take before the placeholder replaces the stale preview.
const PLACEHOLDER_GRACE: Duration = Duration::from_millis(90);
/// Placeholder animation frame length.
const PLACEHOLDER_FRAME: Duration = Duration::from_millis(80);

/// Blocks until there is something to draw: a key, a finished preview, or the
/// next placeholder frame.
fn wait_for_work(app: &mut App) -> Result<()> {
    let entered_on = app.preview.placeholder_frame();
    loop {
        let tick = if app.preview.pending() {
            PREVIEW_TICK
        } else {
            IDLE_TICK
        };
        if event::poll(tick)? {
            return Ok(()); // a key is waiting: it takes priority
        }
        if app.preview.absorb() {
            return Ok(());
        }
        // Redraw on a frame change only — polling at 16ms must not drag the
        // 80ms animation up to a 60fps repaint.
        if app.preview.placeholder_frame() != entered_on {
            return Ok(());
        }
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> Result<Option<(Option<Entry>, Accept)>> {
    // Trace-only bookkeeping (see `trace`): both stay untouched when tracing is off.
    let mut first_list_drawn = false;
    let mut key_at: Option<Instant> = None;
    loop {
        // Draw first: it publishes the preview pane's width, which the request
        // below needs to clip the card to. The first pass draws an empty pane
        // for one frame, which is what the placeholder is for anyway.
        terminal.draw(|f| ui::draw(f, app))?;
        if trace::enabled() {
            // The keystroke budget is "key in, pixels out": close the span on the
            // draw that follows the key, not on the handler returning.
            if let Some(at) = key_at.take() {
                trace::span("key.to_frame", at);
            }
            if !first_list_drawn && !app.picker.entries.is_empty() {
                first_list_drawn = true;
                trace::mark("frame.first_list");
            }
        }
        app.request_preview();
        wait_for_work(app)?;
        if !event::poll(Duration::ZERO)? {
            continue; // woke for a finished preview, not a key: redraw it
        }
        match event::read()? {
            Event::Mouse(m) => {
                let at = Position::new(m.column, m.row);
                match m.kind {
                    MouseEventKind::ScrollDown => {
                        app.on_wheel(at, 1);
                    }
                    MouseEventKind::ScrollUp => {
                        app.on_wheel(at, -1);
                    }
                    MouseEventKind::Down(MouseButton::Left) => match app.on_click(at) {
                        Flow::Continue => {}
                        Flow::Quit => return Ok(None),
                        Flow::Accept(a) => {
                            return Ok(Some((app.picker.selected_entry().cloned(), a)))
                        }
                    },
                    // Releases and drags: nothing here acts on them, and
                    // redrawing for them would be churn.
                    _ => continue,
                }
            }
            Event::Key(k) => {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if trace::enabled() {
                    key_at = Some(Instant::now());
                }
                match handle_key(app, k) {
                    Flow::Continue => {}
                    Flow::Quit => return Ok(None),
                    Flow::Accept(a) => return Ok(Some((app.picker.selected_entry().cloned(), a))),
                }
            }
            _ => {}
        }
    }
}

/// Wheel reporting, on and off.
///
/// Not crossterm's `EnableMouseCapture`: that also turns on any-event tracking
/// (`?1003h`), which reports every pointer *move*. The loop would wake and
/// redraw hundreds of times a second for events it discards. `?1000h` reports
/// buttons only — the wheel among them — and `?1006h` asks for SGR encoding, so
/// coordinates past column 223 survive.
pub(crate) const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
pub(crate) const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";

/// Claim the terminal, then the wheel. Also chains the mouse teardown ahead of
/// the panic hook `ratatui::init` installs — that hook restores the screen but
/// knows nothing about the wheel, and a panic must not leave the terminal
/// reporting clicks at a shell.
pub(crate) fn init_terminal() -> ratatui::DefaultTerminal {
    let terminal = ratatui::init();
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        print!("{MOUSE_OFF}");
        let _ = io::stdout().flush();
        restore(info);
    }));
    print!("{MOUSE_ON}");
    let _ = io::stdout().flush();
    terminal
}

pub(crate) fn restore_terminal() {
    print!("{MOUSE_OFF}");
    let _ = io::stdout().flush();
    ratatui::restore();
}

/// `herdr-switchboard open --target T --path P --origin O --label L` — the
/// clone flow (`bin/get.sh`) delegates here so the herdr open verbs live only in
/// Rust rather than being mirrored in bash.
fn cli_open(args: &[String]) -> Result<()> {
    let (mut target, mut path, mut origin, mut label) =
        (String::new(), String::new(), String::new(), String::new());
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let val = it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--target" => target = val,
            "--path" => path = val,
            "--origin" => origin = val,
            "--label" => label = val,
            _ => {}
        }
    }
    let cfg = Config::try_load()?;
    action::open_target(&runner::SystemRunner, &target, &path, &origin, &label, &cfg)
}

/// `herdr-switchboard config get KEY [DEFAULT]` — the scalar config reader,
/// so bash reads a setting through the same parser the TUI uses.
fn cli_config(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("get") => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            let default = args.get(2).map(String::as_str).unwrap_or("");
            println!("{}", Config::try_load()?.get(key, default));
            Ok(())
        }
        _ => Err(anyhow::anyhow!("usage: config get <key> [default]")),
    }
}

#[derive(Debug)]
struct NonUtf8Argument {
    index: usize,
}

impl fmt::Display for NonUtf8Argument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "command argument {} is not valid UTF-8",
            self.index
        )
    }
}

impl std::error::Error for NonUtf8Argument {}

fn collect_startup_args(argv: impl IntoIterator<Item = OsString>) -> Result<Vec<String>> {
    argv.into_iter()
        .skip(1)
        .enumerate()
        .map(|(offset, argument)| {
            argument
                .into_string()
                .map_err(|_| NonUtf8Argument { index: offset + 1 }.into())
        })
        .collect()
}

fn main() -> Result<()> {
    // First statement on purpose: it fixes the zero point every other trace mark
    // is measured from. Inert unless SWITCHBOARD_TRACE is set.
    trace::init();
    // One binary, many modes. bin/changelog.sh execs us with --changelog for the
    // standalone changelog pane; the clone flow execs `open`/`config` so the herdr
    // verbs and the scalar config reader live only here, not mirrored in bash.
    let args = collect_startup_args(env::args_os())?;
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("herdr-switchboard {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--changelog") => return changelog::main(),
        Some("--update-check") => return update::main(),
        Some("--git") => return git::main(),
        Some("--menu") => return menu::main(Config::try_load()?, Theme::load()),
        Some("--agent-launch") => {
            return agents::launch_worker(&args[1..], &Config::try_load()?);
        }
        Some("--agents") => return agents::main(Config::try_load()?, Theme::load()),
        Some("--commands") => return commands::main(Config::try_load()?, Theme::load()),
        Some("--ports") => return ports::main(Config::try_load()?, Theme::load()),
        Some("--settings") => return settings::main(Config::try_load()?, Theme::load()),
        Some("--usage") => return usage::main(Config::try_load()?, Theme::load()),
        Some("--zen") => return zen::main(Config::try_load()?, Theme::load()),
        Some("open") => return cli_open(&args[1..]),
        Some("config") => return cli_config(&args[1..]),
        Some("zen") => return zen::cli(&runner::SystemRunner, &args[1..], &Config::try_load()?),
        Some("notify") => return notify::cli(&args[1..], &Config::try_load()?),
        _ => {}
    }

    let runner = runner::SystemRunner;
    let cfg = Config::try_load()?;
    let theme = Theme::load();
    let script_dir = env::var("HERDR_PLUGIN_ROOT")
        .map(|r| format!("{r}/bin"))
        .unwrap_or_else(|_| ".".into());
    let origin = env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default();

    // Hands the network to a detached child and returns immediately; the badge it
    // enables shows up on a later launch. Nothing below waits on it.
    update::spawn_refresh_if_stale(&cfg);

    trace::mark("config+theme.loaded");

    // Loading is synchronous, and it happens *before* the terminal is claimed.
    // The source commands cost ~35ms together, which is well under the frame the
    // old animation spent covering them — and this way an empty result hands off
    // to the clone flow without ever having taken the screen and given it back.
    let root = data::ghq_root(&runner);
    let entries = source::load_all(
        &cfg,
        &source::LoadCtx {
            runner: &runner,
            theme: &theme,
            root: &root,
        },
    );
    if trace::enabled() {
        trace::mark_with("sources.ready", &entries.len().to_string());
    }
    if entries.is_empty() {
        // Nothing to switch to yet — hand the pane to the clone flow.
        let err = Command::new("bash")
            .arg(format!("{script_dir}/get.sh"))
            .exec();
        return Err(err.into());
    }

    let mut app = App::new(entries, theme, cfg, script_dir.clone());
    let mut terminal = init_terminal();
    trace::mark("terminal.claimed");
    let outcome = run(&mut terminal, &mut app);
    restore_terminal();

    if let Some((entry, accept)) = outcome? {
        let id = entry.as_ref().map(|e| e.id.clone());

        // Resolve where Enter lands a repo from the (possibly just-applied) config, so a
        // `default_target` change made in the settings overlay is honoured this session.
        let default_target = action::resolve_default_target(
            action::forced_target().as_deref(),
            &app.cfg.get("default_target", "workspace"),
        );
        action::dispatch(
            &runner,
            entry,
            accept,
            &origin,
            &app.cfg,
            &script_dir,
            &default_target,
        )?;
        // Record recency only for successful opens (dispatch returned Ok above).
        if let Some(id) = id {
            match accept {
                Accept::Default
                | Accept::Workspace
                | Accept::Tab
                | Accept::Split
                | Accept::Pane => history::touch(&id),
                Accept::Remove => history::forget(&id),
                // Clone / UpdatePlugin exec away and never come back here.
                Accept::Update | Accept::Clone | Accept::UpdatePlugin => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::*;
    use data::Kind;
    use ratatui::style::Color;

    fn entry(kind: Kind, id: &str, primary: &str) -> Entry {
        Entry {
            kind,
            id: id.into(),
            dir: None,
            label: primary.into(),
            icon: String::new(),
            icon_color: Color::Reset,
            primary: primary.into(),
            secondary: String::new(),
            search: primary.into(),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry(Kind::Repo, "gh/zeta", "zeta"),
            entry(Kind::Agent, "term-1", "alpha"),
            entry(Kind::Repo, "gh/mid", "mid"),
            entry(Kind::Workspace, "ws-1", "work"),
        ]
    }

    #[test]
    fn startup_args_skip_non_utf8_executable_name_before_conversion() {
        let argv = vec![
            OsString::from_vec(vec![0xff]),
            OsString::from("--agent-launch"),
            OsString::from("--kind"),
            OsString::from("claude"),
        ];

        let args = collect_startup_args(argv).unwrap();

        assert_eq!(args, ["--agent-launch", "--kind", "claude"]);
    }

    #[test]
    fn startup_args_return_typed_error_for_non_utf8_command_argument() {
        let argv = vec![
            OsString::from("herdr-switchboard"),
            OsString::from("--title"),
            OsString::from_vec(vec![0xff]),
        ];

        let error = collect_startup_args(argv).unwrap_err();

        assert_eq!(error.downcast_ref::<NonUtf8Argument>().unwrap().index, 2);
    }

    #[test]
    fn recent_sort_puts_latest_opened_first() {
        let e = sample();
        let mut recent = HashMap::new();
        recent.insert("gh/mid".to_string(), 100u64);
        recent.insert("term-1".to_string(), 200u64);
        let order = browse_order(&e, &recent, GroupFilter::All, SortMode::Recent);
        // term-1 (200) then gh/mid (100), then the untouched two in load order.
        assert_eq!(order, vec![1, 2, 0, 3]);
    }

    #[test]
    fn name_sort_is_alphabetical() {
        let e = sample();
        let order = browse_order(&e, &HashMap::new(), GroupFilter::All, SortMode::Name);
        // alpha, mid, work, zeta
        assert_eq!(order, vec![1, 2, 3, 0]);
    }

    #[test]
    fn kind_sort_groups_agents_workspaces_repos_then_worktrees() {
        let mut e = sample();
        e.push(entry(Kind::Worktree, "/tmp/zeta.feature", "zeta feature"));
        let order = browse_order(&e, &HashMap::new(), GroupFilter::All, SortMode::Kind);
        // agent(1), workspace(3), repos in load order(0,2), worktree(4)
        assert_eq!(order, vec![1, 3, 0, 2, 4]);
    }

    /// An app whose preview pane sits at 0,0 and shows `rows` of a `len`-row card.
    /// The pane is two rows and two columns taller/wider than its interior: the border.
    fn app_with_preview(len: u16, rows: u16) -> App {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        app.preview.area = Some(Rect::new(0, 0, 40, rows + 2));
        app.preview.len = len;
        app
    }

    #[test]
    fn preview_scroll_stops_at_the_last_screenful() {
        let mut app = app_with_preview(60, 20);
        app.preview.scroll_by(1000);
        // The end of the scroll is the last full screen, not the last line:
        // scrolling past it would leave the pane showing blanks.
        assert_eq!(app.preview.scroll, 40);
    }

    #[test]
    fn preview_scroll_stops_at_the_top() {
        let mut app = app_with_preview(60, 20);
        app.preview.scroll_by(-5);
        assert_eq!(app.preview.scroll, 0);
    }

    #[test]
    fn preview_that_fits_does_not_scroll() {
        let mut app = app_with_preview(5, 20);
        app.preview.scroll_by(3);
        assert_eq!(app.preview.scroll, 0);
    }

    /// An app laid out the way a draw would leave it: a list at 0,0 and a
    /// command bar on row 30 carrying one `open` pill spanning columns 1..8.
    fn app_with_layout() -> App {
        let mut app = app_with_preview(60, 20);
        app.zones.list_area = Rect::new(0, 10, 40, 12);
        app.zones.footer_row = 30;
        app.zones.footer_zones = vec![(1, 8, keymap::Action::Accept(Accept::Default))];
        app.zones.tab_zones = vec![
            (1, 6, GroupFilter::All),
            (7, 15, GroupFilter::Only(Kind::Repo)),
        ];
        app
    }

    fn is_accept(flow: Flow) -> bool {
        matches!(flow, Flow::Accept(_))
    }

    /// Render the whole UI into a buffer and hand back what it says, row by row.
    fn rendered(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The same render as [`rendered`], handing back the cells themselves.
    fn rendered_buffer(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The switcher's own panels are transparent, the way `picker.rs` pins its
    /// mode pickers down. `panel_bg` belongs on chips and on the popups that
    /// have to occlude the picker; a panel that fills itself with it blacks out
    /// the terminal behind a pane that is meant to show through.
    #[test]
    fn no_switcher_panel_paints_an_opaque_background() {
        let mut app = app_with_layout();
        app.theme = Theme::from_slots(&[("panel_bg", "#101214")]);
        let fill = Color::Rgb(0x10, 0x12, 0x14);
        let buffer = rendered_buffer(&mut app, 80, 20);
        for y in 0..20 {
            for x in 0..80 {
                assert_ne!(buffer[(x, y)].bg, fill, "cell ({x},{y}) filled the panel");
            }
        }
    }

    /// A click outside the settings card closes it — and must discard, the way
    /// `esc` does. Closing without discarding left a staged edit alive behind a
    /// form that looked shut, and the next open showed it as unsaved.
    #[test]
    fn a_click_outside_the_settings_card_discards_the_draft_and_closes() {
        let mut app = app_with_layout();
        app.settings
            .redirect(std::env::temp_dir().join("switchboard-never-written.toml"));
        app.settings.open();
        let _ = rendered(&mut app, 120, 40);
        app.settings
            .on_key(crossterm::event::KeyEvent::from(KeyCode::Enter));
        assert!(app.settings.dirty(), "the draft is staged");

        app.on_click(Position::new(0, 0));
        assert!(!app.settings.show);
        assert!(!app.settings.dirty(), "the close rolled the draft back");
    }

    #[test]
    fn the_help_popup_says_what_each_key_does_in_full() {
        let mut app = app_with_layout();
        app.show_help = true;
        // A description too wide for the column is cut with no ellipsis to warn
        // anyone — `wheel  Scroll whatever is under it` reached a screenshot as
        // `Scroll whatever is`. `row`'s debug_assert fires here if it recurs.
        let screen = rendered(&mut app, 120, 40);
        assert!(screen.contains("Scroll that pane"), "{screen}");
        assert!(screen.contains("Select or run it"), "{screen}");
    }

    #[test]
    fn the_first_frame_is_the_loaded_list_not_a_splash() {
        // Sources load before `App` exists, so there is no state in which the
        // picker draws anything but its own list.
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        let screen = rendered(&mut app, 80, 24);
        assert!(screen.contains("Search"), "{screen}");
        assert!(screen.contains("zeta"), "{screen}");
    }

    #[test]
    fn settings_is_offered_in_the_bar_and_floats_over_the_picker() {
        let mut app = app_with_layout();
        // The command bar advertises settings alongside the other verbs.
        let bar = rendered(&mut app, 120, 40);
        assert!(
            bar.lines().last().unwrap().contains("settings"),
            "the footer must offer settings: {:?}",
            bar.lines().last().unwrap()
        );
        // ⌥, opens it, and the card draws *over* the list rather than replacing it —
        // the picker's Search box is still framed behind the overlay.
        handle_key(&mut app, key(KeyCode::Char(','), KeyModifiers::ALT));
        assert!(app.settings.show);
        let screen = rendered(&mut app, 120, 40);
        assert!(screen.contains("Switchboard Settings"), "{screen}");
        assert!(screen.contains("default_target"), "{screen}");
        assert!(
            screen.contains("Search"),
            "the picker must stay behind the overlay: {screen}"
        );
    }

    #[test]
    fn the_command_bar_pills_are_where_their_zones_say() {
        let mut app = app_with_layout();
        let screen = rendered(&mut app, 120, 40);
        let bar = screen.lines().last().unwrap().to_string();
        // The zones the draw published must land on the pills the draw drew.
        for &(a, b, _) in &app.zones.footer_zones {
            let pill: String = bar
                .chars()
                .skip(a as usize)
                .take((b - a) as usize)
                .collect();
            assert!(
                !pill.trim().is_empty(),
                "zone {a}..{b} covers blank bar: {bar:?}"
            );
        }
        let (a, b, _) = app.zones.footer_zones[0];
        let first: String = bar
            .chars()
            .skip(a as usize)
            .take((b - a) as usize)
            .collect();
        assert_eq!(first.trim(), "↵ open");
    }

    #[test]
    fn clicking_a_row_selects_that_entry() {
        let mut app = app_with_layout();
        // Row 10 is the top border (the tab strip); the list starts at 11.
        app.on_click(Position::new(5, 13));
        assert_eq!(app.picker.selected, 2);
    }

    #[test]
    fn clicking_a_row_reads_through_the_scroll_offset() {
        let mut app = app_with_layout();
        // Scrolled down: the first visible row is entry 1, not entry 0. Getting
        // this wrong selects a different entry than the one under the pointer.
        *app.zones.list_state.offset_mut() = 1;
        app.on_click(Position::new(5, 11));
        assert_eq!(app.picker.selected, 1);
    }

    #[test]
    fn clicking_past_the_last_entry_selects_nothing() {
        let mut app = app_with_layout();
        app.picker.selected = 2;
        // The list is 12 rows tall but holds 4 entries; this is empty space.
        app.on_click(Position::new(5, 20));
        assert_eq!(
            app.picker.selected, 2,
            "the selection must survive a click on nothing"
        );
    }

    #[test]
    fn clicking_a_pill_runs_its_command() {
        let mut app = app_with_layout();
        assert!(is_accept(app.on_click(Position::new(3, 30))));
    }

    #[test]
    fn clicking_beside_the_pills_does_nothing() {
        let mut app = app_with_layout();
        assert!(!is_accept(app.on_click(Position::new(60, 30))));
    }

    #[test]
    fn clicking_a_tab_switches_the_group() {
        let mut app = app_with_layout();
        // The strip rides the list's top border, row 10.
        app.on_click(Position::new(8, 10));
        assert_eq!(app.picker.group, GroupFilter::Only(Kind::Repo));
    }

    #[test]
    fn a_click_dismisses_the_help_popup_and_nothing_else() {
        let mut app = app_with_layout();
        app.show_help = true;
        // Aimed straight at a pill: the popup is modal, so it must swallow this.
        assert!(!is_accept(app.on_click(Position::new(3, 30))));
        assert!(!app.show_help);
    }

    #[test]
    fn wheel_over_the_preview_scrolls_the_card() {
        let mut app = app_with_preview(60, 20);
        app.on_wheel(Position::new(5, 5), 1);
        // Three rows a notch.
        assert_eq!(app.preview.scroll, 3);
        assert_eq!(
            app.picker.selected, 0,
            "the selection must not move with the card"
        );
    }

    #[test]
    fn wheel_outside_the_preview_walks_the_list() {
        let mut app = app_with_preview(60, 20);
        // The pane is 40 wide; this is past its right edge.
        app.on_wheel(Position::new(80, 5), 1);
        assert_eq!(app.picker.selected, 1);
        assert_eq!(
            app.preview.scroll, 0,
            "the card must not move with the list"
        );
    }

    #[test]
    fn wheel_over_a_hidden_preview_walks_the_list() {
        let mut app = app_with_preview(60, 20);
        // ⌥p hides the pane; its rect is stale, so the pointer being "inside" it
        // means nothing and the wheel belongs to the list.
        app.preview.enabled = false;
        app.on_wheel(Position::new(5, 5), 1);
        assert_eq!(app.picker.selected, 1);
    }

    #[test]
    fn group_filter_narrows_to_one_kind() {
        let e = sample();
        let order = browse_order(
            &e,
            &HashMap::new(),
            GroupFilter::Only(Kind::Repo),
            SortMode::Name,
        );
        // only the two repos, alphabetical: mid(2), zeta(0)
        assert_eq!(order, vec![2, 0]);
    }

    #[test]
    fn configured_default_tab_selects_a_present_group_or_all() {
        let repos = Config::from_pairs(&[("default_tab", "repos")]);
        let app = App::new(sample(), Theme::default(), repos, ".".into());
        assert_eq!(app.picker.group, GroupFilter::Only(Kind::Repo));

        let missing = Config::from_pairs(&[("default_tab", "worktrees")]);
        let app = App::new(sample(), Theme::default(), missing, ".".into());
        assert_eq!(app.picker.group, GroupFilter::All);

        let mut entries = sample();
        entries.push(entry(Kind::Worktree, "/tmp/repo.feature", "repo"));
        let worktrees = Config::from_pairs(&[("default_tab", "worktrees")]);
        let app = App::new(entries, Theme::default(), worktrees, ".".into());
        assert_eq!(app.picker.group, GroupFilter::Only(Kind::Worktree));
    }

    #[test]
    fn worktree_selection_disables_repo_update_and_remove() {
        let app = App::new(
            vec![entry(Kind::Worktree, "/tmp/repo.feature", "repo")],
            Theme::default(),
            Config::default(),
            ".".into(),
        );
        assert!(!app.action_available(keymap::Action::Accept(Accept::Update)));
        assert!(!app.action_available(keymap::Action::Accept(Accept::Remove)));
        assert!(app.action_available(keymap::Action::Accept(Accept::Tab)));
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, mods)
    }

    #[test]
    fn typing_a_letter_in_insert_mode_filters() {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        handle_key(&mut app, key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.picker.query, "z");
    }

    #[test]
    fn ctrl_t_accepts_into_a_tab_through_the_keymap() {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        let flow = handle_key(&mut app, key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(matches!(flow, Flow::Accept(Accept::Tab)));
    }

    #[test]
    fn question_mark_opens_help_rather_than_typing() {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        handle_key(&mut app, key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(app.show_help);
        assert_eq!(app.picker.query, "", "? must not land in the query");
    }

    #[test]
    fn modal_mode_navigates_bare_and_i_returns_to_insert() {
        let cfg = Config::from_pairs(&[("keymode", "modal")]);
        let mut app = App::new(sample(), Theme::default(), cfg, ".".into());
        assert_eq!(app.mode, keymap::Mode::Normal);

        // Bare `j` walks the list and does not type.
        handle_key(&mut app, key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.picker.selected, 1);
        assert_eq!(app.picker.query, "");

        // `i` enters Insert, where letters filter again.
        handle_key(&mut app, key(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(app.mode, keymap::Mode::Insert);
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.picker.query, "x");

        // Esc returns to Normal (modal), not quit.
        assert!(matches!(
            handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE)),
            Flow::Continue
        ));
        assert_eq!(app.mode, keymap::Mode::Normal);
    }
}
