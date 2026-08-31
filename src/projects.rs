//! Projects Picker model, reduction, terminal hosting, and restored effects.

mod effect;
mod handoff;
mod preview;
mod stars;
mod view;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::env;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher, Utf32Str};
use ratatui::layout::{Position, Rect};
use ratatui::text::Text;
use ratatui::widgets::ListState;

use crate::action::Accept;
#[cfg(test)]
use crate::agent_handoff::AgentTarget;
use crate::agent_handoff::{discover_targets, TargetResolution, TargetScope};
use crate::data::{Config, Entry, GroupFilter, Kind, SortMode, Theme};
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::surface::{Surface as HostedSurface, Transition as SurfaceTransition};
use crate::{
    action, changelog, history, keymap, markdown, runner, settings, source, surface, trace, update,
};
use effect::CatalogWorker;
use handoff::{HandoffAction, HandoffRequest, HandoffState, ItemContext};
use view as ui;

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
    /// Durable local favourites, private to the Projects feature.
    stars: stars::Stars,
    /// How many entries each tab holds, in [`Picker::tabs`] order.
    ///
    /// Cached rather than counted on demand because the Context panel asks for
    /// every tab's count on every frame, and each answer is a full scan of the
    /// catalogue. The counts depend only on the entries and the stars, never on
    /// the query, group, or sort — so they are rebuilt by [`Picker::recount`] at
    /// the three points those two change, not by `recompute`.
    group_counts: Vec<(GroupFilter, usize)>,
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
    pub tab_zones: Vec<(Rect, GroupFilter)>,
    /// The command bar's pills, each carrying the action it runs when clicked.
    pub footer_zones: Vec<(u16, u16, keymap::Action)>,
    /// The command bar's row — it is one row tall, so this plus a zone's `x`
    /// span is the whole hit test.
    pub footer_row: u16,
}

pub struct App {
    pub theme: Theme,
    pub background: crate::tui::SurfaceBackground,
    pub title_color: ratatui::style::Color,
    pub cfg: Config,
    pub script_dir: String,
    catalog: CatalogState,
    overlay: Overlay,
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
    handoff: HandoffState,
    /// The settings form used when [`Overlay::Settings`] owns input.
    pub settings: settings::Settings,
    pub zones: HitZones,
    /// A non-sensitive inline failure that replaces the ordinary result count.
    pub feedback: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Overlay {
    #[default]
    None,
    Help,
    Changelog,
    Settings,
    Handoff,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CatalogState {
    Loading,
    Ready,
    Refreshing,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CatalogIntent {
    Initial,
    Refresh {
        requested_group: GroupFilter,
        preserve_selection: bool,
    },
}

enum Flow {
    Continue,
    Quit,
    Accept(Accept),
    CopyPath(Entry),
    DiscoverTargets(Entry),
    Deliver(HandoffRequest),
    SetStar(Entry, bool),
    ReloadCatalog(CatalogIntent),
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
            stars: stars::Stars::load(),
            group_counts: Vec::new(),
        };
        picker.recount();
        // Apply the initial sort (Recent by default) to the resting list.
        picker.recompute();
        picker
    }

    /// Rebuild the per-tab counts. One pass over the catalogue for all tabs
    /// rather than one pass per tab, and the only place `stars` is consulted for
    /// counting.
    fn recount(&mut self) {
        let mut counts: Vec<(GroupFilter, usize)> =
            self.tabs().into_iter().map(|group| (group, 0)).collect();
        for entry in &self.entries {
            let starred = self.stars.contains(entry);
            for (group, count) in &mut counts {
                if group.matches(entry.kind, starred) {
                    *count += 1;
                }
            }
        }
        self.group_counts = counts;
    }

    fn recompute(&mut self) {
        let group = self.group;
        if self.query.is_empty() {
            // Browse mode: filter by group, then order by the active sort.
            self.filtered =
                browse_order(&self.entries, &self.recent, &self.stars, group, self.sort);
        } else {
            // Search mode: fuzzy score wins; group still narrows the candidates.
            let pat = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = Vec::new();
            for (i, e) in self.entries.iter().enumerate() {
                if !group.matches(e.kind, self.stars.contains(e)) {
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

    /// The tab strip in order: All, each present kind, then Starred.
    pub fn tabs(&self) -> Vec<GroupFilter> {
        let mut v = vec![GroupFilter::All];
        v.extend(self.present_kinds.iter().map(|&k| GroupFilter::Only(k)));
        v.push(GroupFilter::Starred);
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

    pub fn is_starred(&self, entry: &Entry) -> bool {
        self.stars.contains(entry)
    }

    pub fn group_count(&self, group: GroupFilter) -> usize {
        self.group_counts
            .iter()
            .find(|(candidate, _)| *candidate == group)
            .map_or(0, |(_, count)| *count)
    }

    /// Install a persisted star snapshot without making a toggle jump the
    /// Navigator back to its first row. Unstarring inside Starred selects the
    /// row that moved into the removed row's place, or the previous last row.
    fn replace_stars(&mut self, stars: stars::Stars) {
        let selected_id = self.selected_entry().map(|entry| entry.id.clone());
        let selected_rank = self.selected;
        self.stars = stars;
        self.recount();
        self.recompute();
        if let Some(id) = selected_id {
            if let Some(position) = self
                .filtered
                .iter()
                .position(|&index| self.entries[index].id == id)
            {
                self.selected = position;
                return;
            }
        }
        self.selected = selected_rank.min(self.filtered.len().saturating_sub(1));
    }

    fn replace_entries(
        &mut self,
        entries: Vec<Entry>,
        requested_group: GroupFilter,
        preserve_selection: bool,
    ) {
        let selected_id = preserve_selection
            .then(|| self.selected_entry().map(|entry| entry.id.clone()))
            .flatten();
        self.entries = entries;
        self.present_kinds = source::kinds()
            .into_iter()
            .filter(|&kind| self.entries.iter().any(|entry| entry.kind == kind))
            .collect();
        // After `present_kinds`, because the tab list it counts is derived from it.
        self.recount();
        self.select_group_or_all(requested_group);
        if let Some(id) = selected_id {
            if let Some(position) = self
                .filtered
                .iter()
                .position(|&index| self.entries[index].id == id)
            {
                self.selected = position;
            }
        }
    }
}

impl PreviewState {
    fn new(cfg: &Config, theme: &Theme, script_dir: &str) -> Self {
        let enabled = cfg.projects.preview != "disabled";
        let position = cfg.projects.preview_position.clone();
        let pct = cfg
            .projects
            .preview_size
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
        let background = crate::tui::SurfaceBackground::resolve(&theme, cfg.common.transparency);
        let title_color = theme
            .resolve(&cfg.common.title_color)
            .unwrap_or_else(|| theme.or("accent", ratatui::style::Color::Cyan));
        let sort = SortMode::parse(&cfg.projects.sort);
        // Read before `cfg` moves into the struct.
        let update = update::available(&cfg);
        let recent = history::load();
        let preview = PreviewState::new(&cfg, &theme, &script_dir);
        let mut picker = Picker::new(entries, sort, recent);
        picker.select_group_or_all(GroupFilter::parse(&cfg.projects.default_tab));
        let keymap = keymap::Keymap::load(&cfg);
        let mode = keymap.start_mode();
        // Seed the settings form from the same cfg before it moves into the struct.
        let settings = settings::Settings::new(&cfg);
        App {
            theme,
            background,
            title_color,
            cfg,
            script_dir,
            catalog: CatalogState::Ready,
            overlay: Overlay::None,
            update,
            keymap,
            mode,
            leader_pending: false,
            picker,
            preview,
            changelog: ChangelogState::new(),
            handoff: HandoffState::new(),
            settings,
            zones: HitZones::new(),
            feedback: None,
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

    fn install_catalog(&mut self, entries: Vec<Entry>, intent: CatalogIntent) {
        let (requested_group, preserve_selection) = match intent {
            CatalogIntent::Initial => (GroupFilter::parse(&self.cfg.projects.default_tab), false),
            CatalogIntent::Refresh {
                requested_group,
                preserve_selection,
            } => (requested_group, preserve_selection),
        };
        self.picker
            .replace_entries(entries, requested_group, preserve_selection);
        self.catalog = CatalogState::Ready;
        self.preview.id.clear();
    }

    /// Re-read `config.toml` and re-derive the runtime state that depends on it, so a
    /// setting applied in the overlay takes effect in this session rather than on the
    /// next launch. Called after `Settings::apply` reports it wrote something.
    ///
    /// The entry list is reloaded too, so the source toggles and label style update
    /// live; that resettles the selection at the top the way a `sort` change reorders
    /// it anyway. `mode` is left as the user has it — `keymode` only picks the *start*
    /// mode.
    fn reconfigure(&mut self) -> CatalogIntent {
        let cfg = Config::load();
        let default_tab_changed = self.cfg.projects.default_tab != cfg.projects.default_tab;

        self.title_color = self
            .theme
            .resolve(&cfg.common.title_color)
            .unwrap_or_else(|| self.theme.or("accent", ratatui::style::Color::Cyan));
        self.background =
            crate::tui::SurfaceBackground::resolve(&self.theme, cfg.common.transparency);
        self.picker.sort = SortMode::parse(&cfg.projects.sort);
        self.keymap = keymap::Keymap::load(&cfg);

        // Preview geometry is read straight from these fields at draw time; the readme
        // toggle lives in the worker's config, so respawn it and force a re-render.
        self.preview.enabled = cfg.projects.preview != "disabled";
        self.preview.position = cfg.projects.preview_position.clone();
        self.preview.pct = cfg
            .projects
            .preview_size
            .trim_end_matches('%')
            .parse::<u16>()
            .unwrap_or(52)
            .clamp(20, 80);
        self.preview.worker =
            preview::Worker::spawn(self.script_dir.clone(), cfg.clone(), self.theme.clone());
        self.preview.id.clear();

        let requested = if default_tab_changed {
            GroupFilter::parse(&cfg.projects.default_tab)
        } else {
            self.picker.group
        };
        self.cfg = cfg;
        self.catalog = CatalogState::Refreshing;
        CatalogIntent::Refresh {
            requested_group: requested,
            preserve_selection: !default_tab_changed,
        }
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
        if matches!(self.overlay, Overlay::Help | Overlay::Changelog) {
            self.overlay = Overlay::None;
            return Flow::Continue;
        }
        if self.overlay == Overlay::Handoff {
            return self.on_handoff_click(at);
        }
        // The settings form is modal: inside the card the pointer picks a tab, a
        // row or a pill; outside it the click dismisses. Dismissing goes through
        // `close_discarding` so a staged edit cannot survive the close — `esc`
        // discards, and a click that closed without discarding left the draft
        // alive behind a form that looked shut.
        if self.overlay == Overlay::Settings {
            let mut reload = None;
            if self.settings.hit(at) {
                if self.settings.on_click(at) {
                    reload = Some(self.reconfigure());
                }
            } else {
                self.settings.close_discarding();
                self.overlay = Overlay::None;
            }
            if !self.settings.show {
                self.overlay = Overlay::None;
            }
            return reload.map(Flow::ReloadCatalog).unwrap_or(Flow::Continue);
        }
        // The command bar: one row, so the x span is the whole test. A pill runs
        // its action, the same as its key would.
        if let Some(action) =
            crate::tui::zone_at(&self.zones.footer_zones, self.zones.footer_row, at)
        {
            // Accepting on nothing would be a no-op with a confirmation prompt.
            if action.needs_selection() && self.picker.selected_entry().is_none() {
                return Flow::Continue;
            }
            return apply_action(self, action);
        }
        if let Some(&(_, group)) = self
            .zones
            .tab_zones
            .iter()
            .find(|(zone, _)| zone.contains(at))
        {
            if group != self.picker.group {
                self.picker.group = group;
                self.picker.recompute();
            }
            return Flow::Continue;
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
        if self.overlay == Overlay::Settings {
            self.settings.on_wheel(delta as isize);
            return true;
        }
        if self.overlay == Overlay::Handoff {
            self.handoff.move_selection(delta.signum());
            return true;
        }
        if self.overlay == Overlay::Changelog {
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

    fn on_handoff_click(&mut self, at: Position) -> Flow {
        if let Some(action) =
            crate::tui::zone_at(&self.handoff.footer_zones, self.handoff.footer_row, at)
        {
            return match action {
                HandoffAction::Back => {
                    self.overlay = Overlay::None;
                    Flow::Continue
                }
                HandoffAction::Send => self
                    .handoff
                    .selected_target()
                    .and_then(|target| self.handoff.begin_delivery(target))
                    .map(Flow::Deliver)
                    .unwrap_or(Flow::Continue),
            };
        }
        if !self.handoff.list_area.contains(at) {
            return Flow::Continue;
        }
        let first = self.handoff.list_area.y + 1;
        let Some(row) = at.y.checked_sub(first).map(usize::from) else {
            return Flow::Continue;
        };
        let selected = self.handoff.list_state.offset() + row;
        if selected >= self.handoff.filtered.len() {
            return Flow::Continue;
        }
        if selected == self.handoff.selected {
            return self
                .handoff
                .selected_target()
                .and_then(|target| self.handoff.begin_delivery(target))
                .map(Flow::Deliver)
                .unwrap_or(Flow::Continue);
        }
        self.handoff.selected = selected;
        Flow::Continue
    }

    /// Worktrees are openable and reviewable, but repository update/removal has
    /// different semantics and is intentionally unavailable for them.
    pub fn action_available(&self, action: keymap::Action) -> bool {
        if action.needs_selection() && self.picker.selected_entry().is_none() {
            return false;
        }
        if self.catalog == CatalogState::Refreshing && action.needs_selection() {
            return false;
        }
        let unsupported = matches!(
            action,
            keymap::Action::Accept(Accept::Update | Accept::Remove)
        );
        if unsupported
            && self
                .picker
                .selected_entry()
                .is_some_and(|entry| entry.kind == Kind::Worktree)
        {
            return false;
        }
        if matches!(
            action,
            keymap::Action::CopyPath | keymap::Action::SendToAgent
        ) {
            return self.picker.selected_entry().is_some_and(|entry| {
                entry.kind != Kind::Workspace
                    && entry
                        .dir
                        .as_deref()
                        .is_some_and(|path| std::path::Path::new(path).is_absolute())
            });
        }
        if action == keymap::Action::ToggleStar {
            return self
                .picker
                .selected_entry()
                .is_some_and(stars::Stars::supports);
        }
        true
    }
}

/// The no-query browse order: entries passing `group`, ordered by `sort`.
/// Ties break on original load order so the list is stable.
fn browse_order(
    entries: &[Entry],
    recent: &HashMap<String, u64>,
    stars: &stars::Stars,
    group: GroupFilter,
    sort: SortMode,
) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len())
        .filter(|&i| group.matches(entries[i].kind, stars.contains(&entries[i])))
        .collect();
    // `sort_by_cached_key` rather than `sort_by`: each key is derived once per
    // entry instead of once per comparison. The Name order was lowercasing both
    // sides inside the comparator, which is two String allocations per compare
    // and O(n log n) of them; Recent was re-hashing an id into the recency map
    // just as often. Every key ends in the entry's load index, so ties still
    // break on load order exactly as before.
    match sort {
        SortMode::Recent => idx.sort_by_cached_key(|&i| {
            (Reverse(recent.get(&entries[i].id).copied().unwrap_or(0)), i)
        }),
        SortMode::Name => idx.sort_by_cached_key(|&i| (entries[i].primary.to_lowercase(), i)),
        SortMode::Kind => idx.sort_by_cached_key(|&i| (entries[i].kind.order(), i)),
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
    app.feedback = None;
    if !app.action_available(action) {
        return Flow::Continue;
    }
    match action {
        Action::Quit => return Flow::Quit,
        Action::Help => app.overlay = Overlay::Help,
        Action::Changelog => {
            app.changelog.open();
            app.overlay = Overlay::Changelog;
        }
        Action::Settings => {
            app.settings.open();
            app.overlay = Overlay::Settings;
        }
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
        Action::CopyPath => {
            if let Some(entry) = app.picker.selected_entry().cloned() {
                return Flow::CopyPath(entry);
            }
        }
        Action::SendToAgent => {
            if let Some(entry) = app.picker.selected_entry().cloned() {
                app.handoff.finding();
                app.overlay = Overlay::Handoff;
                return Flow::DiscoverTargets(entry);
            }
        }
        Action::ToggleStar => {
            if let Some(entry) = app.picker.selected_entry().cloned() {
                let starred = !app.picker.is_starred(&entry);
                return Flow::SetStar(entry, starred);
            }
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

    if app.overlay == Overlay::Handoff {
        match k.code {
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Esc => {
                if app.handoff.query.is_empty() {
                    app.overlay = Overlay::None;
                } else {
                    app.handoff.query.clear();
                    app.handoff.refilter();
                }
            }
            KeyCode::Down | KeyCode::Char('n') if ctrl => app.handoff.move_selection(1),
            KeyCode::Up | KeyCode::Char('p') if ctrl => app.handoff.move_selection(-1),
            KeyCode::Down => app.handoff.move_selection(1),
            KeyCode::Up => app.handoff.move_selection(-1),
            KeyCode::Home => app.handoff.selected = 0,
            KeyCode::End => app.handoff.selected = app.handoff.filtered.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(request) = app
                    .handoff
                    .selected_target()
                    .and_then(|target| app.handoff.begin_delivery(target))
                {
                    return Flow::Deliver(request);
                }
            }
            KeyCode::Backspace => {
                app.handoff.query.pop();
                app.handoff.refilter();
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                app.handoff.query.push(c);
                app.handoff.refilter();
            }
            _ => {}
        }
        return Flow::Continue;
    }

    // The settings overlay is a form: while open it owns navigation, Enter (cycle),
    // and the in-place split_ratio edit, so route every key to it. `esc`/`q` close it
    // from inside `on_key`; `^c` still quits the picker so you are never trapped.
    if app.overlay == Overlay::Settings {
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            return Flow::Quit;
        }
        // An apply persisted a change: re-read config.toml and re-derive the live
        // state so the new value takes effect now, not on the next launch.
        if app.settings.on_key(k) {
            let intent = app.reconfigure();
            if !app.settings.show {
                app.overlay = Overlay::None;
            }
            return Flow::ReloadCatalog(intent);
        }
        if !app.settings.show {
            app.overlay = Overlay::None;
        }
        return Flow::Continue;
    }

    // The changelog popup scrolls, so it cannot dismiss on any key the way the help
    // cheatsheet does; esc/q closes it and the movement keys drive it.
    if app.overlay == Overlay::Changelog {
        let c = &mut app.changelog;
        let page = c.rows.saturating_sub(2).max(1);
        let max = c.len.saturating_sub(c.rows);
        match k.code {
            KeyCode::Char('c') if ctrl => return Flow::Quit,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => app.overlay = Overlay::None,
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
    if app.overlay == Overlay::Help {
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            return Flow::Quit;
        }
        app.overlay = Overlay::None;
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

struct ProjectsSurface<'a> {
    app: &'a mut App,
    origin_pane: String,
    notifier: Notifier,
    effect: Option<Receiver<ProjectEffect>>,
    catalog_worker: CatalogWorker,
    catalog_generation: u64,
    catalog_intent: CatalogIntent,
    catalog_pending: bool,
    first_loading_drawn: bool,
    first_list_drawn: bool,
    key_at: Option<Instant>,
    placeholder_frame: Option<usize>,
}

enum ProjectOutcome {
    Accept(Option<Entry>, Accept),
    CopyPath(Entry),
}

enum ProjectEffect {
    Targets(Result<(ItemContext, TargetResolution), String>),
    Delivered(Result<(), String>),
    Stars(Result<stars::Stars, String>),
}

impl HostedSurface for ProjectsSurface<'_> {
    type Output = Option<ProjectOutcome>;

    fn terminal_claimed(&mut self) {
        trace::mark("terminal.claimed");
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        // Draw first: it publishes the preview pane's width, which the request
        // below needs to clip the card to. The first pass draws an empty pane
        // for one frame, which is what the placeholder is for anyway.
        ui::draw(frame, self.app);
        if trace::enabled() {
            if !self.first_loading_drawn && self.app.catalog == CatalogState::Loading {
                self.first_loading_drawn = true;
                trace::mark("frame.loading");
            }
            // The keystroke budget is "key in, pixels out": close the span on the
            // draw that follows the key, not on the handler returning.
            if let Some(at) = self.key_at.take() {
                trace::span("key.to_frame", at);
            }
            if !self.first_list_drawn && !self.app.picker.entries.is_empty() {
                self.first_list_drawn = true;
                trace::mark("frame.first_list");
            }
        }
    }

    fn after_draw(&mut self) -> Result<()> {
        self.app.request_preview();
        self.placeholder_frame = self.app.preview.placeholder_frame();
        Ok(())
    }

    fn tick_rate(&self) -> Duration {
        if self.app.preview.pending() || self.effect.is_some() || self.catalog_pending {
            PREVIEW_TICK
        } else {
            IDLE_TICK
        }
    }

    fn on_tick(&mut self) -> Result<SurfaceTransition<Self::Output>> {
        if self.catalog_pending {
            match self.catalog_worker.poll() {
                effect::Poll::Pending => {}
                effect::Poll::Ready(completion) => {
                    if let Some(transition) = self.accept_catalog_completion(completion) {
                        return Ok(transition);
                    }
                }
                effect::Poll::Disconnected => {
                    self.catalog_pending = false;
                    self.app.catalog = CatalogState::Failed(
                        "Project discovery stopped unexpectedly. Close and reopen to retry.".into(),
                    );
                    return Ok(SurfaceTransition::Redraw);
                }
            }
        }
        if let Some(receiver) = &self.effect {
            let effect = match receiver.try_recv() {
                Ok(effect) => effect,
                Err(TryRecvError::Empty) => return Ok(SurfaceTransition::Wait),
                Err(TryRecvError::Disconnected) => {
                    self.effect = None;
                    self.app.handoff.delivery_failed("");
                    self.app.feedback = Some("Background action stopped unexpectedly.".into());
                    return Ok(SurfaceTransition::Redraw);
                }
            };
            self.effect = None;
            return Ok(match effect {
                ProjectEffect::Targets(Ok((item, targets))) => {
                    if let Some(request) = self.app.handoff.show_targets(item, targets) {
                        self.apply_flow(Flow::Deliver(request))
                    } else {
                        SurfaceTransition::Redraw
                    }
                }
                ProjectEffect::Targets(Err(error)) => {
                    self.app.handoff.status = None;
                    self.app.handoff.error = Some(error);
                    SurfaceTransition::Redraw
                }
                ProjectEffect::Delivered(Ok(())) => {
                    self.notifier.send(NotifyEvent::PathHandoffSucceeded, None);
                    SurfaceTransition::Exit(None)
                }
                ProjectEffect::Delivered(Err(error)) => {
                    self.app.handoff.delivery_failed(&error);
                    SurfaceTransition::Redraw
                }
                ProjectEffect::Stars(Ok(stars)) => {
                    self.app.picker.replace_stars(stars);
                    SurfaceTransition::Redraw
                }
                ProjectEffect::Stars(Err(_error)) => {
                    self.app.feedback = Some("Could not update star.".into());
                    SurfaceTransition::Redraw
                }
            });
        }
        // Poll at 16ms while work is pending, but repaint only for a completed
        // preview or an actual 80ms placeholder-frame change.
        let absorbed = self.app.preview.absorb();
        let frame_changed = self.app.preview.placeholder_frame() != self.placeholder_frame;
        Ok(if absorbed || frame_changed {
            SurfaceTransition::Redraw
        } else {
            SurfaceTransition::Wait
        })
    }

    fn on_event(&mut self, event: Event) -> Result<SurfaceTransition<Self::Output>> {
        if matches!(
            self.app.catalog,
            CatalogState::Loading | CatalogState::Failed(_)
        ) {
            if let Event::Key(k) = event {
                if k.kind == KeyEventKind::Press {
                    if let Some(chord) = keymap::chord_of(&k) {
                        if self.app.keymap.action(self.app.mode, chord)
                            == Some(keymap::Action::Quit)
                        {
                            return Ok(SurfaceTransition::Exit(None));
                        }
                    }
                }
            }
            if let Event::Mouse(m) = event {
                if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    let at = Position::new(m.column, m.row);
                    if crate::tui::zone_at(
                        &self.app.zones.footer_zones,
                        self.app.zones.footer_row,
                        at,
                    ) == Some(keymap::Action::Quit)
                    {
                        return Ok(SurfaceTransition::Exit(None));
                    }
                }
            }
            return Ok(SurfaceTransition::Wait);
        }
        if self.effect.is_some() {
            if let Event::Key(k) = event {
                if k.kind == KeyEventKind::Press
                    && k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c'))
                {
                    return Ok(SurfaceTransition::Exit(None));
                }
            }
            return Ok(SurfaceTransition::Wait);
        }
        match event {
            Event::Mouse(m) => {
                let at = Position::new(m.column, m.row);
                match m.kind {
                    MouseEventKind::ScrollDown => {
                        self.app.on_wheel(at, 1);
                    }
                    MouseEventKind::ScrollUp => {
                        self.app.on_wheel(at, -1);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        let flow = self.app.on_click(at);
                        return Ok(self.apply_flow(flow));
                    }
                    // Releases and drags: nothing here acts on them, and
                    // redrawing for them would be churn.
                    _ => return Ok(SurfaceTransition::Wait),
                }
                Ok(SurfaceTransition::Redraw)
            }
            Event::Key(k) => {
                if k.kind != KeyEventKind::Press {
                    return Ok(SurfaceTransition::Wait);
                }
                if trace::enabled() {
                    self.key_at = Some(Instant::now());
                }
                let flow = handle_key(self.app, k);
                Ok(self.apply_flow(flow))
            }
            _ => Ok(SurfaceTransition::Wait),
        }
    }
}

impl ProjectsSurface<'_> {
    fn accept_catalog_completion(
        &mut self,
        completion: effect::CatalogCompletion,
    ) -> Option<SurfaceTransition<Option<ProjectOutcome>>> {
        if completion.generation != self.catalog_generation {
            return None;
        }
        self.catalog_pending = false;
        if self.catalog_intent == CatalogIntent::Initial && completion.entries.is_empty() {
            return Some(SurfaceTransition::Exit(Some(ProjectOutcome::Accept(
                None,
                Accept::Clone,
            ))));
        }
        self.app
            .install_catalog(completion.entries, self.catalog_intent);
        Some(SurfaceTransition::Redraw)
    }

    fn start_catalog(
        &mut self,
        intent: CatalogIntent,
    ) -> SurfaceTransition<Option<ProjectOutcome>> {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.catalog_intent = intent;
        self.catalog_pending = true;
        self.app.catalog = match intent {
            CatalogIntent::Initial => CatalogState::Loading,
            CatalogIntent::Refresh { .. } => CatalogState::Refreshing,
        };
        if !self.catalog_worker.request(
            self.catalog_generation,
            self.app.cfg.clone(),
            self.app.theme.clone(),
        ) {
            self.catalog_pending = false;
            self.app.catalog = CatalogState::Failed(
                "Project discovery could not start. Close and reopen to retry.".into(),
            );
        }
        SurfaceTransition::Redraw
    }

    fn apply_flow(&mut self, flow: Flow) -> SurfaceTransition<Option<ProjectOutcome>> {
        match flow {
            Flow::Continue => SurfaceTransition::Redraw,
            Flow::Quit => SurfaceTransition::Exit(None),
            Flow::Accept(accept) => SurfaceTransition::Exit(Some(ProjectOutcome::Accept(
                self.app.picker.selected_entry().cloned(),
                accept,
            ))),
            Flow::CopyPath(entry) => SurfaceTransition::Exit(Some(ProjectOutcome::CopyPath(entry))),
            Flow::DiscoverTargets(entry) => {
                let (sender, receiver) = mpsc::channel();
                let origin_pane = self.origin_pane.clone();
                std::thread::spawn(move || {
                    let runner = runner::SystemRunner;
                    let result = handoff::resolve_item(&runner, &entry)
                        .ok_or_else(|| "Selected item has no absolute path.".to_string())
                        .map(|item| {
                            let targets =
                                discover_targets(&runner, &item.absolute_path, &origin_pane);
                            (item, targets)
                        });
                    let _ = sender.send(ProjectEffect::Targets(result));
                });
                self.effect = Some(receiver);
                SurfaceTransition::Redraw
            }
            Flow::Deliver(request) => {
                let (sender, receiver) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = handoff::deliver(&runner::SystemRunner, &request)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(ProjectEffect::Delivered(result));
                });
                self.effect = Some(receiver);
                SurfaceTransition::Redraw
            }
            Flow::SetStar(entry, starred) => {
                let (sender, receiver) = mpsc::channel();
                let stars = self.app.picker.stars.clone();
                std::thread::spawn(move || {
                    let result = stars
                        .set(&entry, starred)
                        .map_err(|error| error.to_string());
                    let _ = sender.send(ProjectEffect::Stars(result));
                });
                self.effect = Some(receiver);
                SurfaceTransition::Redraw
            }
            Flow::ReloadCatalog(intent) => self.start_catalog(intent),
        }
    }
}

/// Run the Projects Picker after the composition root has selected this mode.
pub(crate) fn main(cfg: Config, theme: Theme) -> Result<()> {
    let runner = runner::SystemRunner;
    let script_dir = env::var("HERDR_PLUGIN_ROOT")
        .map(|r| format!("{r}/bin"))
        .unwrap_or_else(|_| ".".into());
    let origin = env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default();

    // Hands the network to a detached child and returns immediately; the badge it
    // enables shows up on a later launch. Nothing below waits on it.
    update::spawn_refresh_if_stale(&cfg);

    trace::mark("config+theme.loaded");

    let mut app = App::new(Vec::new(), theme, cfg, script_dir.clone());
    app.catalog = CatalogState::Loading;
    let notifier = Notifier::new(&app.cfg);
    let mut surface = ProjectsSurface {
        app: &mut app,
        origin_pane: origin.clone(),
        notifier,
        effect: None,
        catalog_worker: CatalogWorker::spawn(),
        catalog_generation: 0,
        catalog_intent: CatalogIntent::Initial,
        catalog_pending: false,
        first_loading_drawn: false,
        first_list_drawn: false,
        key_at: None,
        placeholder_frame: None,
    };
    surface.start_catalog(CatalogIntent::Initial);
    let outcome = surface::run(&mut surface);

    match outcome? {
        Some(ProjectOutcome::CopyPath(entry)) => {
            let item = handoff::resolve_item(&runner, &entry)
                .ok_or_else(|| anyhow::anyhow!("selected item has no absolute path"))?;
            crate::clipboard::copy_text(&item.absolute_path)?;
        }
        Some(ProjectOutcome::Accept(entry, accept)) => {
            let id = entry.as_ref().map(|e| e.id.clone());
            let removed_star = (accept == Accept::Remove)
                .then(|| entry.as_ref().cloned())
                .flatten()
                .filter(|entry| app.picker.is_starred(entry));

            // Resolve where Enter lands a repo from the (possibly just-applied) config, so a
            // `default_target` change made in the settings overlay is honoured this session.
            let default_target = action::resolve_default_target(
                action::forced_target().as_deref(),
                &app.cfg.projects.default_target,
            );
            let dispatch_outcome = action::dispatch(
                &runner,
                entry,
                accept,
                &origin,
                &app.cfg,
                &script_dir,
                &default_target,
            )?;
            if dispatch_outcome == action::DispatchOutcome::Aborted {
                return Ok(());
            }
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
            if let Some(entry) = removed_star {
                if app.picker.stars.clone().set(&entry, false).is_err() {
                    Notifier::new(&app.cfg).send_message(
                        "Repository was removed, but its local star could not be cleared.",
                        "request",
                    );
                }
            }
        }
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Kind;
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
    fn recent_sort_puts_latest_opened_first() {
        let e = sample();
        let mut recent = HashMap::new();
        recent.insert("gh/mid".to_string(), 100u64);
        recent.insert("term-1".to_string(), 200u64);
        let order = browse_order(
            &e,
            &recent,
            &stars::Stars::default(),
            GroupFilter::All,
            SortMode::Recent,
        );
        // term-1 (200) then gh/mid (100), then the untouched two in load order.
        assert_eq!(order, vec![1, 2, 0, 3]);
    }

    #[test]
    fn name_sort_is_alphabetical() {
        let e = sample();
        let order = browse_order(
            &e,
            &HashMap::new(),
            &stars::Stars::default(),
            GroupFilter::All,
            SortMode::Name,
        );
        // alpha, mid, work, zeta
        assert_eq!(order, vec![1, 2, 3, 0]);
    }

    #[test]
    fn kind_sort_groups_agents_workspaces_repos_then_worktrees() {
        let mut e = sample();
        e.push(entry(Kind::Worktree, "/tmp/zeta.feature", "zeta feature"));
        let order = browse_order(
            &e,
            &HashMap::new(),
            &stars::Stars::default(),
            GroupFilter::All,
            SortMode::Kind,
        );
        // agent(1), workspace(3), repos in load order(0,2), worktree(4)
        assert_eq!(order, vec![1, 3, 0, 2, 4]);
    }

    /// An app whose preview pane sits at 0,0 and shows `rows` of a `len`-row card.
    /// The pane is two rows and two columns taller/wider than its interior: the border.
    fn app_with_preview(len: u16, rows: u16) -> App {
        let mut cfg = Config::default();
        cfg.common.keymode = crate::config::KeyMode::Insert;
        let mut app = App::new(sample(), Theme::default(), cfg, ".".into());
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
            (Rect::new(1, 10, 5, 1), GroupFilter::All),
            (Rect::new(7, 10, 8, 1), GroupFilter::Only(Kind::Repo)),
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

    /// The column `needle` starts at in a rendered row. Every fixture below uses
    /// ASCII markers, so a byte offset into the joined row is its column.
    fn column_of(row: &str, needle: &str) -> usize {
        row.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is not in `{row}`"))
    }

    /// One row per (primary, secondary) pair, with a marker icon.
    fn marked_rows(rows: &[(&str, &str)]) -> Vec<Entry> {
        rows.iter()
            .enumerate()
            .map(|(index, (primary, secondary))| Entry {
                icon: "@".into(),
                secondary: (*secondary).into(),
                ..entry(Kind::Repo, &format!("gh/{index}"), primary)
            })
            .collect()
    }

    /// The Navigator's columns are fixed: icon, star, a `PRIMARY_WIDTH` primary,
    /// then the secondary one column further on.
    ///
    /// The primary is padded by a second blank span rather than by growing a
    /// clone of the entry's own text, so this pins that the pair still paints
    /// one identical row. A drift here would not error — it would silently shift
    /// the whole secondary column, and only on rows short enough to need padding.
    #[test]
    fn a_navigator_row_pads_its_primary_column_to_a_fixed_width() {
        // One primary well short of the column and one that overruns it, so both
        // the padded and the unpadded branch are covered.
        let long = "L".repeat(ui::PRIMARY_WIDTH + 4);
        let entries = marked_rows(&[("ALPHA", "SEC"), (&long, "TAIL")]);
        let mut app = App::new(entries, Theme::default(), Config::default(), ".".into());
        app.catalog = CatalogState::Ready;
        // Give the Navigator the whole body: a preview pane would narrow it to
        // less than the fixed column and clip the answer away.
        app.preview.enabled = false;

        for width in [140u16, 100, 80] {
            let screen = rendered(&mut app, width, 24);
            let short_row = screen
                .lines()
                .find(|line| line.contains("ALPHA"))
                .unwrap_or_else(|| panic!("no ALPHA row at width {width}:\n{screen}"));
            let icon = column_of(short_row, "@");
            let primary = column_of(short_row, "ALPHA");
            // icon, space, star, space — then the primary column starts.
            assert_eq!(primary, icon + 4, "width {width}: {short_row}");
            assert_eq!(
                column_of(short_row, "SEC"),
                primary + ui::PRIMARY_WIDTH + 1,
                "width {width}: a padded primary must reach the fixed column\n{short_row}"
            );

            let long_row = screen
                .lines()
                .find(|line| line.contains("TAIL"))
                .unwrap_or_else(|| panic!("no overlong row at width {width}:\n{screen}"));
            // An overlong primary is not truncated and not padded: the secondary
            // simply follows it one column later.
            assert_eq!(
                column_of(long_row, "TAIL"),
                column_of(long_row, &long) + long.chars().count() + 1,
                "width {width}: an overlong primary must not be padded\n{long_row}"
            );
        }
    }

    /// The same render as [`rendered`], handing back the cells themselves.
    fn rendered_buffer(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Transparent mode leaves every ordinary cell at the terminal default.
    /// Deliberately coloured pills and selection rows use other theme slots;
    /// `panel_bg` remains their foreground ink, never an accidental fill.
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

    #[test]
    fn every_projects_overlay_obeys_the_transparent_background() {
        let fill = Color::Rgb(0x10, 0x12, 0x14);
        for overlay in [
            Overlay::Help,
            Overlay::Changelog,
            Overlay::Settings,
            Overlay::Handoff,
        ] {
            let mut app = app_with_layout();
            app.theme = Theme::from_slots(&[("panel_bg", "#101214")]);
            app.background = crate::tui::SurfaceBackground::resolve(
                &app.theme,
                crate::config::Transparency::Transparent,
            );
            app.overlay = overlay;
            app.settings.open();
            let buffer = rendered_buffer(&mut app, 120, 40);
            assert!(
                buffer.content.iter().all(|cell| cell.bg != fill),
                "{overlay:?} painted panel_bg in transparent mode"
            );
        }
    }

    #[test]
    fn opaque_projects_and_overlays_leave_no_transparent_holes() {
        for overlay in [
            Overlay::None,
            Overlay::Help,
            Overlay::Changelog,
            Overlay::Settings,
            Overlay::Handoff,
        ] {
            let mut app = app_with_layout();
            app.theme = Theme::from_slots(&[("panel_bg", "#101214")]);
            app.background = crate::tui::SurfaceBackground::resolve(
                &app.theme,
                crate::config::Transparency::Opaque,
            );
            app.overlay = overlay;
            app.settings.open();
            let buffer = rendered_buffer(&mut app, 120, 40);
            assert!(
                buffer.content.iter().all(|cell| cell.bg != Color::Reset),
                "{overlay:?} left a transparent cell in opaque mode"
            );
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
        app.overlay = Overlay::Settings;
        let _ = rendered(&mut app, 120, 40);
        app.settings
            .on_key(crossterm::event::KeyEvent::from(KeyCode::Enter));
        assert!(app.settings.dirty(), "the draft is staged");

        app.on_click(Position::new(0, 0));
        assert_eq!(app.overlay, Overlay::None);
        assert!(!app.settings.show);
        assert!(!app.settings.dirty(), "the close rolled the draft back");
    }

    #[test]
    fn the_help_popup_says_what_each_key_does_in_full() {
        let mut app = app_with_layout();
        app.overlay = Overlay::Help;
        // A description too wide for the column is cut with no ellipsis to warn
        // anyone — `wheel  Scroll whatever is under it` reached a screenshot as
        // `Scroll whatever is`. `row`'s debug_assert fires here if it recurs.
        let screen = rendered(&mut app, 120, 40);
        assert!(screen.contains("Scroll that pane"), "{screen}");
        assert!(screen.contains("Select or run it"), "{screen}");
    }

    #[test]
    fn the_first_frame_is_stable_projects_chrome_with_a_standing_by_state() {
        for (width, expected) in [
            (140, &["Search", "Context", "Navigator", "Preview"][..]),
            (100, &["Search", "Navigator", "Preview"][..]),
            (79, &["Search", "Navigator"][..]),
        ] {
            let mut app = App::new(Vec::new(), Theme::default(), Config::default(), ".".into());
            app.catalog = CatalogState::Loading;
            let screen = rendered(&mut app, width, 24);
            for label in expected {
                assert!(screen.contains(label), "missing {label}: {screen}");
            }
            assert!(screen.contains("Standing by…"), "{screen}");
            assert!(!screen.contains("zeta"), "{screen}");
        }
    }

    #[test]
    fn startup_loading_accepts_close_and_ignores_row_input() {
        let mut app = App::new(Vec::new(), Theme::default(), Config::default(), ".".into());
        app.catalog = CatalogState::Loading;
        let notifier = Notifier::new(&app.cfg);
        let mut surface = ProjectsSurface {
            app: &mut app,
            origin_pane: String::new(),
            notifier,
            effect: None,
            catalog_worker: CatalogWorker::spawn(),
            catalog_generation: 0,
            catalog_intent: CatalogIntent::Initial,
            catalog_pending: false,
            first_loading_drawn: false,
            first_list_drawn: false,
            key_at: None,
            placeholder_frame: None,
        };

        let enter = Event::Key(crossterm::event::KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            surface.on_event(enter).unwrap(),
            SurfaceTransition::Wait
        ));
        let close = Event::Key(crossterm::event::KeyEvent::from(KeyCode::Char('q')));
        assert!(matches!(
            surface.on_event(close).unwrap(),
            SurfaceTransition::Exit(None)
        ));
    }

    #[test]
    fn an_empty_initial_catalog_returns_the_typed_clone_outcome() {
        let mut app = App::new(Vec::new(), Theme::default(), Config::default(), ".".into());
        app.catalog = CatalogState::Loading;
        let notifier = Notifier::new(&app.cfg);
        let mut surface = ProjectsSurface {
            app: &mut app,
            origin_pane: String::new(),
            notifier,
            effect: None,
            catalog_worker: CatalogWorker::spawn(),
            catalog_generation: 7,
            catalog_intent: CatalogIntent::Initial,
            catalog_pending: true,
            first_loading_drawn: false,
            first_list_drawn: false,
            key_at: None,
            placeholder_frame: None,
        };

        let transition = surface
            .accept_catalog_completion(effect::CatalogCompletion {
                generation: 7,
                entries: Vec::new(),
            })
            .expect("current completion must be applied");
        assert!(matches!(
            transition,
            SurfaceTransition::Exit(Some(ProjectOutcome::Accept(None, Accept::Clone)))
        ));
    }

    #[test]
    fn catalog_completion_preserves_query_and_installs_the_configured_group() {
        let mut cfg = Config::default();
        cfg.projects.default_tab = "repos".into();
        let mut app = App::new(Vec::new(), Theme::default(), cfg, ".".into());
        app.catalog = CatalogState::Loading;
        app.picker.query = "mid".into();

        app.install_catalog(sample(), CatalogIntent::Initial);

        assert_eq!(app.catalog, CatalogState::Ready);
        assert_eq!(app.picker.group, GroupFilter::Only(Kind::Repo));
        assert_eq!(app.picker.query, "mid");
        assert_eq!(app.picker.selected_entry().unwrap().id, "gh/mid");
    }

    #[test]
    fn refresh_keeps_the_old_list_then_restores_selection_by_entry_id() {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        app.picker.selected = 2;
        let selected_id = app.picker.selected_entry().unwrap().id.clone();
        app.catalog = CatalogState::Refreshing;
        let refreshing = rendered(&mut app, 120, 32);
        assert!(refreshing.contains("Refreshing…"), "{refreshing}");
        assert!(refreshing.contains("zeta"), "{refreshing}");
        assert!(!app.action_available(keymap::Action::Accept(Accept::Default)));

        let mut reordered = sample();
        reordered.reverse();
        app.install_catalog(
            reordered,
            CatalogIntent::Refresh {
                requested_group: GroupFilter::All,
                preserve_selection: true,
            },
        );

        assert_eq!(app.catalog, CatalogState::Ready);
        assert_eq!(app.picker.selected_entry().unwrap().id, selected_id);
    }

    #[test]
    fn wide_dashboard_has_context_navigator_and_inspector() {
        let mut app = app_with_layout();
        let screen = rendered(&mut app, 140, 32);
        assert!(screen.contains("Context"), "{screen}");
        assert!(screen.contains("Navigator"), "{screen}");
        assert!(screen.contains("Preview"), "{screen}");
        assert!(app.zones.tab_zones.iter().all(|(zone, _)| zone.height == 1));
    }

    #[test]
    fn compact_dashboard_prioritizes_the_navigator() {
        let mut app = app_with_layout();
        let screen = rendered(&mut app, 79, 24);
        assert!(!screen.contains("Context"), "{screen}");
        assert!(!screen.contains("Preview"), "{screen}");
        assert!(app.preview.area.is_none());
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
        assert_eq!(app.overlay, Overlay::Settings);
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
    fn a_path_selection_offers_copy_and_send_in_the_footer_and_help() {
        let mut app = App::new(
            vec![path_entry(Kind::Repo, "/repo")],
            Theme::default(),
            Config::default(),
            ".".into(),
        );
        let screen = rendered(&mut app, 180, 40);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("^y copy"), "{footer}");
        assert!(footer.contains("^s send"), "{footer}");
        assert!(footer.contains("␣ b star"), "{footer}");

        app.overlay = Overlay::Help;
        let help = rendered(&mut app, 120, 40);
        assert!(help.contains("Copy path"), "{help}");
        assert!(help.contains("Send to agent"), "{help}");
        assert!(help.contains("Star / unstar"), "{help}");
    }

    #[test]
    fn a_star_keeps_its_peach_colour_when_the_row_is_selected() {
        let entries = vec![entry(Kind::Repo, "gh/repo", "repo")];
        let mut app = App::new(
            entries.clone(),
            Theme::from_slots(&[("peach", "#ffaa00")]),
            Config::default(),
            ".".into(),
        );
        app.picker.replace_stars(stars::Stars::memory(&entries));

        let buffer = rendered_buffer(&mut app, 79, 24);
        let marker_x = (0..79)
            .find(|&x| buffer[(x, 4)].symbol() == "★")
            .expect("star marker on the first list row");
        assert_eq!(buffer[(marker_x, 4)].fg, Color::Rgb(0xff, 0xaa, 0x00));
    }

    #[test]
    fn an_empty_starred_filter_explains_what_is_missing() {
        let mut app = app_with_layout();
        app.picker.group = GroupFilter::Starred;
        app.picker.recompute();

        let screen = rendered(&mut app, 100, 28);
        assert!(
            screen.contains("No starred repos or worktrees yet"),
            "{screen}"
        );
    }

    #[test]
    fn an_empty_starred_filter_disables_selection_actions() {
        let mut app = app_with_layout();
        app.picker.group = GroupFilter::Starred;
        app.picker.recompute();

        for action in [
            keymap::Action::Accept(Accept::Default),
            keymap::Action::Accept(Accept::Tab),
            keymap::Action::Accept(Accept::Update),
            keymap::Action::Accept(Accept::Remove),
            keymap::Action::ToggleStar,
        ] {
            assert!(!app.action_available(action), "{action:?} needs a row");
        }
        assert!(app.action_available(keymap::Action::Accept(Accept::Clone)));
        assert!(app.action_available(keymap::Action::Accept(Accept::UpdatePlugin)));

        let enter = handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(enter, Flow::Continue));

        let screen = rendered(&mut app, 100, 28);
        let footer = screen.lines().last().unwrap();
        assert!(!footer.contains("open"), "{footer}");
        assert!(!footer.contains("update"), "{footer}");
        assert!(!footer.contains("remove"), "{footer}");
        assert!(footer.contains("clone"), "{footer}");
    }

    #[test]
    fn a_star_write_failure_is_visible_without_exposing_a_path() {
        let mut app = app_with_layout();
        app.feedback = Some("Could not update star.".into());
        let screen = rendered(&mut app, 100, 28);
        assert!(screen.contains("Could not update star."), "{screen}");
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
    fn clicking_the_rendered_starred_tab_opens_the_starred_filter() {
        let mut app = app_with_layout();
        let _ = rendered(&mut app, 140, 32);
        let zone = app
            .zones
            .tab_zones
            .iter()
            .find(|(_, group)| *group == GroupFilter::Starred)
            .map(|(zone, _)| *zone)
            .expect("starred tab zone");

        app.on_click(Position::new(zone.x, zone.y));
        assert_eq!(app.picker.group, GroupFilter::Starred);
    }

    #[test]
    fn every_medium_layout_keeps_the_starred_tab_visible_and_inside_the_navigator() {
        for (width, preview_size) in [(80, "60%"), (80, "80%"), (100, "60%"), (119, "60%")] {
            let mut entries = sample();
            entries.push(entry(Kind::Worktree, "/tmp/repo.feature", "repo feature"));
            let mut config = Config::default();
            config.projects.preview_size = preview_size.into();
            let mut app = App::new(entries, Theme::default(), config, ".".into());
            let buffer = rendered_buffer(&mut app, width, 28);
            let zone = app
                .zones
                .tab_zones
                .iter()
                .find(|(_, group)| *group == GroupFilter::Starred)
                .map(|(zone, _)| *zone)
                .expect("starred tab zone");

            assert!(
                zone.right() <= app.zones.list_area.right(),
                "width {width}: {zone:?} escaped {:?}",
                app.zones.list_area
            );
            let star_x = (zone.x..zone.right())
                .find(|&x| buffer[(x, zone.y)].symbol() == "★")
                .unwrap_or_else(|| {
                    panic!(
                        "width {width}, preview {preview_size}: Starred was not rendered where its zone points"
                    )
                });

            app.on_click(Position::new(star_x, zone.y));
            assert_eq!(app.picker.group, GroupFilter::Starred, "width {width}");
        }
    }

    #[test]
    fn a_click_dismisses_the_help_popup_and_nothing_else() {
        let mut app = app_with_layout();
        app.overlay = Overlay::Help;
        // Aimed straight at a pill: the popup is modal, so it must swallow this.
        assert!(!is_accept(app.on_click(Position::new(3, 30))));
        assert_eq!(app.overlay, Overlay::None);
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
            &stars::Stars::default(),
            GroupFilter::Only(Kind::Repo),
            SortMode::Name,
        );
        // only the two repos, alphabetical: mid(2), zeta(0)
        assert_eq!(order, vec![2, 0]);
    }

    #[test]
    fn starred_filter_uses_the_existing_sort_and_search_rules() {
        let entries = sample();
        let stars = stars::Stars::memory(&[entries[0].clone(), entries[2].clone()]);
        let mut picker = Picker::new(entries, SortMode::Name, HashMap::new());
        picker.replace_stars(stars);
        picker.group = GroupFilter::Starred;
        picker.recompute();

        assert_eq!(
            picker
                .filtered
                .iter()
                .map(|&index| picker.entries[index].id.as_str())
                .collect::<Vec<_>>(),
            vec!["gh/mid", "gh/zeta"]
        );

        picker.query = "zet".into();
        picker.recompute();
        assert_eq!(picker.filtered.len(), 1);
        assert_eq!(picker.selected_entry().unwrap().id, "gh/zeta");
    }

    #[test]
    fn starred_is_always_the_last_tab_and_counts_only_loaded_stars() {
        let entries = sample();
        let mut picker = Picker::new(entries.clone(), SortMode::Recent, HashMap::new());
        picker.replace_stars(stars::Stars::memory(&[entries[0].clone()]));

        assert_eq!(picker.tabs().last(), Some(&GroupFilter::Starred));
        assert_eq!(picker.group_count(GroupFilter::Starred), 1);
    }

    /// The tab counts are cached, because the Context panel asks for every one
    /// of them on every frame and each answer used to be a full scan. A cache
    /// fails by going stale rather than by erroring, so this walks the three
    /// points that can invalidate it — construction, a star change, and a whole
    /// new catalogue — and checks each count against a fresh scan.
    #[test]
    fn tab_counts_are_rebuilt_whenever_the_entries_or_the_stars_change() {
        fn scan(picker: &Picker, group: GroupFilter) -> usize {
            picker
                .entries
                .iter()
                .filter(|entry| group.matches(entry.kind, picker.is_starred(entry)))
                .count()
        }
        fn agrees(picker: &Picker) {
            for group in picker.tabs() {
                assert_eq!(
                    picker.group_count(group),
                    scan(picker, group),
                    "cached count for {group:?} went stale"
                );
            }
        }

        let entries = sample();
        let mut picker = Picker::new(entries.clone(), SortMode::Recent, HashMap::new());
        agrees(&picker);
        assert_eq!(picker.group_count(GroupFilter::All), entries.len());
        assert_eq!(picker.group_count(GroupFilter::Starred), 0);

        picker.replace_stars(stars::Stars::memory(&[
            entries[0].clone(),
            entries[2].clone(),
        ]));
        agrees(&picker);
        assert_eq!(picker.group_count(GroupFilter::Starred), 2);

        // A refresh that drops a whole kind must drop its tab and its count with
        // it, not leave the previous catalogue's number behind.
        picker.replace_entries(
            vec![entry(Kind::Repo, "gh/only", "only")],
            GroupFilter::All,
            false,
        );
        agrees(&picker);
        assert_eq!(picker.group_count(GroupFilter::All), 1);
        assert_eq!(picker.group_count(GroupFilter::Only(Kind::Agent)), 0);
        assert!(!picker.tabs().contains(&GroupFilter::Only(Kind::Agent)));
    }

    #[test]
    fn star_updates_preserve_selection_and_unstar_selects_the_nearest_row() {
        let entries = sample();
        let mut picker = Picker::new(entries.clone(), SortMode::Recent, HashMap::new());
        picker.selected = 2;
        let selected = picker.selected_entry().unwrap().clone();

        picker.replace_stars(stars::Stars::memory(std::slice::from_ref(&selected)));
        assert_eq!(picker.selected_entry().unwrap().id, selected.id);

        picker.group = GroupFilter::Starred;
        picker.recompute();
        assert_eq!(picker.selected_entry().unwrap().id, selected.id);
        picker.replace_stars(stars::Stars::default());
        assert!(picker.filtered.is_empty());
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn configured_default_tab_selects_a_present_group_or_all() {
        let mut repos = Config::default();
        repos.projects.default_tab = "repos".into();
        let app = App::new(sample(), Theme::default(), repos, ".".into());
        assert_eq!(app.picker.group, GroupFilter::Only(Kind::Repo));

        let mut missing = Config::default();
        missing.projects.default_tab = "worktrees".into();
        let app = App::new(sample(), Theme::default(), missing, ".".into());
        assert_eq!(app.picker.group, GroupFilter::All);

        let mut entries = sample();
        entries.push(entry(Kind::Worktree, "/tmp/repo.feature", "repo"));
        let mut worktrees = Config::default();
        worktrees.projects.default_tab = "worktrees".into();
        let app = App::new(entries, Theme::default(), worktrees, ".".into());
        assert_eq!(app.picker.group, GroupFilter::Only(Kind::Worktree));

        let mut starred = Config::default();
        starred.projects.default_tab = "starred".into();
        let app = App::new(sample(), Theme::default(), starred, ".".into());
        assert_eq!(app.picker.group, GroupFilter::Starred);
        assert!(app.picker.filtered.is_empty());
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
        assert!(app.action_available(keymap::Action::ToggleStar));
    }

    #[test]
    fn only_repo_and_worktree_selections_offer_star() {
        for kind in [Kind::Repo, Kind::Worktree] {
            let app = App::new(
                vec![entry(kind, "durable", "durable")],
                Theme::default(),
                Config::default(),
                ".".into(),
            );
            assert!(app.action_available(keymap::Action::ToggleStar));
        }
        for kind in [Kind::Agent, Kind::Workspace] {
            let app = App::new(
                vec![entry(kind, "live", "live")],
                Theme::default(),
                Config::default(),
                ".".into(),
            );
            assert!(!app.action_available(keymap::Action::ToggleStar));
        }
    }

    fn path_entry(kind: Kind, path: &str) -> Entry {
        let mut selected = entry(kind, "id", "selected");
        selected.dir = Some(path.into());
        selected
    }

    #[test]
    fn path_actions_require_an_absolute_non_workspace_path() {
        let action = keymap::Action::CopyPath;
        let send = keymap::Action::SendToAgent;
        for kind in [Kind::Agent, Kind::Repo, Kind::Worktree] {
            let app = App::new(
                vec![path_entry(kind, "/absolute/path")],
                Theme::default(),
                Config::default(),
                ".".into(),
            );
            assert!(app.action_available(action));
            assert!(app.action_available(send));
        }
        let workspace = App::new(
            vec![path_entry(Kind::Workspace, "/ambiguous/path")],
            Theme::default(),
            Config::default(),
            ".".into(),
        );
        assert!(!workspace.action_available(action));
        assert!(!workspace.action_available(send));
        let relative = App::new(
            vec![path_entry(Kind::Repo, "relative")],
            Theme::default(),
            Config::default(),
            ".".into(),
        );
        assert!(!relative.action_available(action));
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, mods)
    }

    #[test]
    fn typing_a_letter_in_insert_mode_filters() {
        let mut app = app_with_preview(0, 0);
        handle_key(&mut app, key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.picker.query, "z");
    }

    #[test]
    fn ctrl_t_accepts_into_a_tab_through_the_keymap() {
        let mut app = app_with_preview(0, 0);
        let flow = handle_key(&mut app, key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(matches!(flow, Flow::Accept(Accept::Tab)));
    }

    #[test]
    fn ctrl_y_returns_a_typed_copy_outcome_and_ctrl_s_opens_handoff() {
        let mut cfg = Config::default();
        cfg.common.keymode = crate::config::KeyMode::Insert;
        let mut app = App::new(
            vec![path_entry(Kind::Repo, "/repo")],
            Theme::default(),
            cfg,
            ".".into(),
        );
        let copy = handle_key(&mut app, key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert!(matches!(copy, Flow::CopyPath(entry) if entry.dir.as_deref() == Some("/repo")));

        let send = handle_key(&mut app, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(send, Flow::DiscoverTargets(_)));
        assert_eq!(app.overlay, Overlay::Handoff);
        assert_eq!(app.handoff.status.as_deref(), Some("Finding agents…"));
    }

    #[test]
    fn ctrl_b_requests_a_repo_star_without_changing_ctrl_s() {
        let mut cfg = Config::default();
        cfg.common.keymode = crate::config::KeyMode::Insert;
        let mut app = App::new(
            vec![path_entry(Kind::Repo, "/repo")],
            Theme::default(),
            cfg,
            ".".into(),
        );

        let star = handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(matches!(star, Flow::SetStar(entry, true) if entry.id == "id"));
        assert!(app.picker.query.is_empty());

        let send = handle_key(&mut app, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(send, Flow::DiscoverTargets(_)));
    }

    fn target(pane_id: &str, cwd: &str) -> AgentTarget {
        AgentTarget {
            pane_id: pane_id.into(),
            agent: "codex".into(),
            status: "idle".into(),
            cwd: cwd.into(),
        }
    }

    fn item() -> ItemContext {
        ItemContext {
            kind: "repository",
            label: "repo".into(),
            absolute_path: "/repo".into(),
        }
    }

    #[test]
    fn exact_origin_bypasses_the_picker_and_other_targets_remain_filterable() {
        let mut handoff = HandoffState::new();
        let direct = handoff
            .show_targets(
                item(),
                TargetResolution {
                    origin: Some(target("origin", "/repo")),
                    choices: Vec::new(),
                    scope: TargetScope::SameWorktree,
                },
            )
            .unwrap();
        assert_eq!(direct.target.pane_id, "origin");

        assert!(handoff
            .show_targets(
                item(),
                TargetResolution {
                    origin: None,
                    choices: vec![target("one", "/repo"), target("two", "/other")],
                    scope: TargetScope::AllAgents,
                },
            )
            .is_none());
        handoff.query = "other".into();
        handoff.refilter();
        assert_eq!(handoff.selected_target().unwrap().pane_id, "two");
    }

    #[test]
    fn handoff_overlay_renders_scope_empty_state_and_live_hit_zones() {
        let mut app = App::new(
            vec![path_entry(Kind::Repo, "/repo")],
            Theme::default(),
            Config::default(),
            ".".into(),
        );
        app.overlay = Overlay::Handoff;
        app.handoff.item = Some(item());
        app.handoff.scope = Some(TargetScope::AllAgents);
        let screen = rendered(&mut app, 120, 40);
        assert!(screen.contains("Send path to agent"), "{screen}");
        assert!(screen.contains("no promptable agents"), "{screen}");
        assert_eq!(app.handoff.footer_zones.len(), 2);
    }

    #[test]
    fn question_mark_opens_help_rather_than_typing() {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        handle_key(&mut app, key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(app.overlay, Overlay::Help);
        assert_eq!(app.picker.query, "", "? must not land in the query");
    }

    #[test]
    fn opening_an_overlay_replaces_the_previous_modal_owner() {
        let mut app = App::new(sample(), Theme::default(), Config::default(), ".".into());
        apply_action(&mut app, keymap::Action::Help);
        assert_eq!(app.overlay, Overlay::Help);

        apply_action(&mut app, keymap::Action::Settings);
        assert_eq!(app.overlay, Overlay::Settings);
        assert!(app.settings.show);
    }

    #[test]
    fn normal_mode_navigates_bare_and_i_returns_to_insert() {
        let cfg = Config::default();
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
