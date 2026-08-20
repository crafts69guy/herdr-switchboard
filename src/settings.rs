//! Settings form: the switcher's TUI vocabulary applied to the plugin's flat
//! `config.toml`.
//!
//! This was an fzf list, which made a fixed form behave like a search: a fuzzy prompt
//! and a match counter. You do not *find* `sort` in this list, you walk to it — so it
//! is a form now, in the picker's colours and command-bar pills.
//!
//! Like the `⌥c` changelog and the remove confirm, it lives **inside** the picker: a
//! centred, rounded, ink-filled floating card — the `?` cheatsheet's shape — drawn over
//! the list rather than a separate herdr pane, so opening it never costs you your place.
//! The settings sit in two columns; the hint for the selected row is spelled out along
//! the bottom, above the command-bar pills, since the narrow columns have no room for a
//! per-row hint.
//!
//! Edits are **drafts**: cycling a value stages it (a peach dot marks a changed row) but
//! writes nothing. `a` applies the whole draft to `config.toml` at once; `esc` discards
//! it. `on_key`/`apply` return `true` on a successful apply so the picker re-reads the
//! config and updates its live state (see `App::reload_config`) — an applied change takes
//! effect in the running session, no relaunch or server reload needed.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::data::{Config, Theme};
use crate::tui::{self, Pill};

/// Standalone settings action. Projects also embeds this same form, so both
/// entry points preserve the draft/apply behavior and namespaced writer.
pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let title = theme
        .resolve(&cfg.common.title_color)
        .unwrap_or_else(|| theme.or("peach", Color::Yellow));
    let mut settings = Settings::new(&cfg);
    settings.open();
    let mut terminal = crate::init_terminal();
    let outcome: Result<()> = (|| {
        while settings.show {
            terminal.draw(|frame| draw(frame, frame.area(), &theme, title, &mut settings))?;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    settings.on_key(key);
                }
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown => settings.on_wheel(1),
                    MouseEventKind::ScrollUp => settings.on_wheel(-1),
                    MouseEventKind::Down(MouseButton::Left) => {
                        settings.on_click(Position::new(m.column, m.row));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    })();
    crate::restore_terminal();
    outcome
}

/// How Enter changes a setting.
enum Cycle {
    /// Step through a fixed ring. An unrecognised current value lands on the first
    /// entry, matching the `*)` fallback each `cycle()` case in the old bash form had.
    Ring(&'static [&'static str]),
    /// Free text, typed in place. Only `split_ratio` wants this.
    Prompt,
}

struct Setting {
    /// The section this setting sits under; a new value starts a new heading,
    /// so the array's order is the display order (like the `?` cheatsheet).
    group: &'static str,
    key: &'static str,
    default: &'static str,
    hint: &'static str,
    cycle: Cycle,
}

const BOOL: &[&str] = &["true", "false"];

/// The settings, in display order, grouped into sections. `write_setting` is
/// keyed by `key`, so the order here is free to read well.
const SETTINGS: &[Setting] = &[
    Setting {
        group: "Open",
        key: "default_target",
        default: "workspace",
        hint: "where Enter opens a repo",
        cycle: Cycle::Ring(&["workspace", "tab", "split", "pane"]),
    },
    Setting {
        group: "Open",
        key: "split_direction",
        default: "right",
        hint: "split growth direction",
        cycle: Cycle::Ring(&["right", "down"]),
    },
    Setting {
        group: "Open",
        key: "split_ratio",
        default: "0.5",
        hint: "split size (0.1-0.9)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Open",
        key: "label",
        default: "repo",
        hint: "workspace/tab label style",
        cycle: Cycle::Ring(&["repo", "owner-repo", "path"]),
    },
    Setting {
        group: "Sources",
        key: "include_agents",
        default: "true",
        hint: "list running agents in the switcher",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Sources",
        key: "include_workspaces",
        default: "true",
        hint: "list open workspaces in the switcher",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Sources",
        key: "include_worktrees",
        default: "true",
        hint: "list linked Git worktrees in the switcher",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Sources",
        key: "default_tab",
        default: "all",
        hint: "active tab at startup and after apply",
        cycle: Cycle::Ring(&["all", "agents", "workspaces", "repos", "worktrees"]),
    },
    Setting {
        group: "Sources",
        key: "sort",
        default: "recent",
        hint: "resting list order (recent/name/kind)",
        cycle: Cycle::Ring(&["recent", "name", "kind"]),
    },
    Setting {
        group: "Keys",
        key: "keymode",
        default: "insert",
        hint: "start mode: insert (type-to-filter) or normal (Vim)",
        cycle: Cycle::Ring(&["insert", "normal"]),
    },
    Setting {
        group: "Preview",
        key: "preview",
        default: "enabled",
        hint: "show the preview pane",
        cycle: Cycle::Ring(&["enabled", "disabled"]),
    },
    Setting {
        group: "Preview",
        key: "preview_position",
        default: "down",
        hint: "which side the preview sits on",
        cycle: Cycle::Ring(&["right", "down", "up", "left"]),
    },
    Setting {
        group: "Preview",
        key: "preview_size",
        default: "60%",
        hint: "preview share of the body",
        cycle: Cycle::Ring(&["40%", "50%", "60%", "70%", "80%"]),
    },
    Setting {
        group: "Preview",
        key: "preview_readme",
        default: "true",
        hint: "include README in the preview",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Appearance",
        key: "title_color",
        default: "peach",
        hint: "box title colour (theme slot or #hex)",
        cycle: Cycle::Ring(&["peach", "mauve", "teal", "blue", "accent"]),
    },
    Setting {
        group: "Appearance",
        key: "transparency",
        default: "auto",
        hint: "popup background transparency",
        cycle: Cycle::Ring(&["auto", "enabled", "disabled"]),
    },
    Setting {
        group: "Clone",
        key: "clone_source",
        default: "clipboard",
        hint: "seed clone input from clipboard",
        cycle: Cycle::Ring(&["clipboard", "prompt"]),
    },
    Setting {
        group: "Clone",
        key: "open_after_clone",
        default: "true",
        hint: "open a repo right after cloning",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Updates",
        key: "update_check",
        default: "true",
        hint: "check GitHub daily for a newer version",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Notifications",
        key: "notifications",
        default: "true",
        hint: "show herdr notifications",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Notifications",
        key: "notification_position",
        default: "top-right",
        hint: "notification corner",
        cycle: Cycle::Ring(&["top-right", "top-left", "bottom-left", "bottom-right"]),
    },
    Setting {
        group: "Notifications",
        key: "notification_sound",
        default: "auto",
        hint: "toast sound: auto per-event, or force one",
        cycle: Cycle::Ring(&["auto", "none", "done", "request"]),
    },
    Setting {
        group: "Git",
        key: "base_branch",
        default: "",
        hint: "base for review branch (blank = auto-detect)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Git",
        key: "all_files_warn",
        default: "1500",
        hint: "confirm all-files over N tracked files (0 = never)",
        cycle: Cycle::Ring(&["1500", "0", "500", "5000"]),
    },
    Setting {
        group: "Catalog",
        key: "history_limit",
        default: "5000",
        hint: "maximum imported command records",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Catalog",
        key: "command_sort",
        default: "frecency",
        hint: "command resting order",
        cycle: Cycle::Ring(&["frecency", "recent", "frequency", "alphabetical"]),
    },
    Setting {
        group: "Monitor",
        key: "refresh_interval_ms",
        default: "2000",
        hint: "listener refresh interval (minimum 250ms)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Zen",
        key: "zen_width",
        default: "70",
        hint: "zen'd pane's share of the tab (20-95%)",
        cycle: Cycle::Ring(&["60", "70", "80", "90"]),
    },
    Setting {
        group: "Zen",
        key: "zen_scrim",
        default: "true",
        hint: "dim the zen gutters",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Usage",
        key: "usage_warn_percent",
        default: "60",
        hint: "ungraded bar level that turns yellow",
        cycle: Cycle::Ring(&["50", "60", "70", "80"]),
    },
    Setting {
        group: "Usage",
        key: "usage_alert_percent",
        default: "85",
        hint: "ungraded bar level that turns red",
        cycle: Cycle::Ring(&["75", "85", "90", "95"]),
    },
    Setting {
        group: "Zen",
        key: "zen_scrim_color",
        default: "#11111b",
        hint: "gutter colour (#rrggbb; herdr paints it opaque)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Zen",
        key: "zen_chrome",
        default: "off",
        hint: "hide herdr chrome while zen'd (panes: borders; full: +tabs/sidebar)",
        cycle: Cycle::Ring(&["off", "panes", "full"]),
    },
];

/// The next value in a ring. An unknown current value restarts at the first.
fn next_in(ring: &[&str], current: &str) -> String {
    let i = ring.iter().position(|v| *v == current);
    match i {
        Some(i) => ring[(i + 1) % ring.len()].to_string(),
        None => ring[0].to_string(),
    }
}

/// Replace a typed setting in its namespaced table while preserving comments and
/// unknown keys through `toml_edit`.
#[cfg(test)]
fn write_setting(path: &PathBuf, key: &str, value: &str) -> Result<()> {
    let mut doc = load_document(path)?;
    set_document_value(&mut doc, key, value)?;
    write_document(path, doc)
}

fn load_document(path: &PathBuf) -> Result<toml_edit::DocumentMut> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    Ok(existing.parse::<toml_edit::DocumentMut>()?)
}

fn set_document_value(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<()> {
    let (section, field) = setting_path(key);
    if doc.get(section).is_none() {
        doc[section] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if is_bool_setting(key) {
        doc[section][field] = toml_edit::value(value == "true");
    } else if matches!(
        key,
        "history_limit"
            | "refresh_interval_ms"
            | "zen_width"
            | "usage_warn_percent"
            | "usage_alert_percent"
            | "all_files_warn"
    ) {
        doc[section][field] = toml_edit::value(value.parse::<i64>()?);
    } else {
        doc[section][field] = toml_edit::value(value);
    }
    Ok(())
}

fn write_document(path: &PathBuf, doc: toml_edit::DocumentMut) -> Result<()> {
    let out = doc.to_string();
    Config::parse(&out)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn setting_path(key: &str) -> (&'static str, &str) {
    match key {
        "keymode"
        | "notifications"
        | "notification_position"
        | "notification_sound"
        | "title_color"
        | "transparency"
        | "update_check" => ("common", key),
        "clone_source" => ("clone", "source"),
        "open_after_clone" => ("clone", "open_after"),
        "base_branch" | "all_files_warn" => ("git", key),
        "history_limit" => ("commands", key),
        "command_sort" => ("commands", "sort"),
        "refresh_interval_ms" => ("ports", key),
        "usage_warn_percent" => ("usage", "warn_percent"),
        "usage_alert_percent" => ("usage", "alert_percent"),
        "zen_width" => ("zen", "width"),
        "zen_scrim" => ("zen", "scrim"),
        "zen_scrim_color" => ("zen", "scrim_color"),
        "zen_chrome" => ("zen", "chrome"),
        _ => ("projects", key),
    }
}

fn is_bool_setting(key: &str) -> bool {
    matches!(
        key,
        "notifications"
            | "update_check"
            | "include_agents"
            | "include_workspaces"
            | "include_worktrees"
            | "preview_readme"
            | "open_after_clone"
            | "zen_scrim"
    )
}

fn config_path() -> PathBuf {
    crate::config::config_path()
}

/// The settings form, embedded in the picker as a floating overlay (like the `⌥c`
/// changelog).
///
/// Edits are **drafts**: cycling a value or typing a `split_ratio` only changes
/// `values` in memory. Nothing touches `config.toml` until you **apply** (`a`); `esc`
/// discards every unsaved change and closes. `saved` is the last-applied snapshot —
/// the baseline both `dirty` and `discard` compare against — so opening, editing, and
/// leaving without applying is a no-op on disk.
pub struct Settings {
    pub show: bool,
    /// The working draft, one per `SETTINGS` entry.
    values: Vec<String>,
    /// What is on disk (== the last applied draft). `values` differing from this is
    /// the unsaved state.
    saved: Vec<String>,
    sel: usize,
    tab: usize,
    /// `Some` while typing a `Cycle::Prompt` value.
    editing: Option<String>,
    path: PathBuf,
    /// Shown in the command bar when an apply fails; the form stays usable.
    error: Option<String>,
    /// Where the last draw put everything a pointer can land on. Written by
    /// [`draw`], read by [`Settings::on_click`] — the card is recentred every
    /// frame, so nothing else knows where it landed.
    zones: Zones,
}

/// The form's click targets, each measured by the loop that lays its row out.
#[derive(Default)]
struct Zones {
    card: Rect,
    tab_row: u16,
    tab_zones: Vec<(u16, u16, usize)>,
    /// The two settings columns and, per screen row, the `SETTINGS` index it
    /// draws — `None` for a group heading or a spacer. That interleaving is why
    /// a screen row is not a settings index and why the map is built by
    /// [`column`] itself.
    cols: [Rect; 2],
    rows: [Vec<Option<usize>>; 2],
    bar_row: u16,
    bar_zones: Vec<(u16, u16, KeyCode)>,
}

impl Settings {
    /// Seed the values from the picker's already-loaded config, so no second read
    /// of `config.toml` is needed.
    pub fn new(cfg: &Config) -> Self {
        let values: Vec<String> = SETTINGS.iter().map(|s| cfg.get(s.key, s.default)).collect();
        Settings {
            show: false,
            saved: values.clone(),
            values,
            sel: 0,
            tab: 1,
            editing: None,
            path: config_path(),
            error: None,
            zones: Zones::default(),
        }
    }

    /// Open the overlay at the top of the form. Values already match `saved` (a close
    /// applies or discards), so there is nothing to reset but the cursor.
    pub fn open(&mut self) {
        self.select_first_in_tab();
        self.editing = None;
        self.error = None;
        self.show = true;
    }

    /// True when the draft has unsaved changes.
    pub(crate) fn dirty(&self) -> bool {
        self.values != self.saved
    }

    /// Stage a value on the selected row. Draft only — no disk write.
    fn set_draft(&mut self, value: String) {
        self.values[self.sel] = value;
    }

    /// Write every changed row to `config.toml`, then adopt the draft as the new
    /// baseline. A failed write leaves the form dirty with the error shown, so it can
    /// be retried. Comments and hand-added keys survive (see `write_setting`).
    ///
    /// Returns `true` when it actually persisted a change, so the picker can re-read
    /// the config and apply it live rather than waiting for the next launch.
    fn apply(&mut self) -> bool {
        if !self.dirty() {
            return false;
        }
        let result = (|| -> Result<()> {
            let mut doc = load_document(&self.path)?;
            for (i, setting) in SETTINGS.iter().enumerate() {
                if self.values[i] != self.saved[i] {
                    set_document_value(&mut doc, setting.key, &self.values[i])?;
                }
            }
            write_document(&self.path, doc)
        })();
        if let Err(error) = result {
            self.error = Some(format!("could not save settings: {error}"));
            return false;
        }
        self.saved = self.values.clone();
        self.error = None;
        true
    }

    /// Drop every unsaved change back to the last-applied baseline.
    fn discard(&mut self) {
        self.values = self.saved.clone();
        self.editing = None;
        self.error = None;
    }

    /// Handle a key while the overlay is open. `a` applies the draft; `esc`/`q`
    /// (outside an edit) discard it and close. The caller keeps `^c` as the picker's
    /// quit. Returns `true` when a key applied a change, so the caller reloads config.
    /// Point the writer at a scratch file so a test can stage and discard drafts
    /// without ever touching the real `config.toml`.
    #[cfg(test)]
    pub(crate) fn redirect(&mut self, path: PathBuf) {
        self.path = path;
    }

    /// A wheel turn walks the form the way `j`/`k` do.
    pub fn on_wheel(&mut self, delta: isize) {
        if self.editing.is_none() {
            self.move_in_tab(delta);
        }
    }

    /// Whether the pointer is over the card at all. A click outside it is a
    /// dismiss, which only the embedded overlay has to care about.
    pub fn hit(&self, at: Position) -> bool {
        self.zones.card.contains(at)
    }

    /// Discard the draft and close, the way `esc` does. Public so the picker's
    /// click-outside path cannot close over a staged edit and leave it alive.
    pub fn close_discarding(&mut self) {
        self.discard();
        self.editing = None;
        self.show = false;
    }

    /// A left click, resolved against the zones the last draw published. Returns
    /// what [`Settings::on_key`] returns: true when config was written.
    ///
    /// Same rule as everywhere else — a click selects, and a click on the row
    /// already selected does what Enter would, so no stray click ever writes.
    pub fn on_click(&mut self, at: Position) -> bool {
        if let Some(code) = crate::tui::zone_at(&self.zones.bar_zones, self.zones.bar_row, at) {
            return self.on_key(KeyEvent::from(code));
        }
        if self.editing.is_some() || !self.zones.card.contains(at) {
            return false;
        }
        if at.y == self.zones.tab_row {
            if let Some(tab) = self
                .zones
                .tab_zones
                .iter()
                .find(|&&(a, b, _)| at.x >= a && at.x < b)
                .map(|&(_, _, i)| i)
            {
                if tab != self.tab {
                    self.tab = tab;
                    self.select_first_in_tab();
                }
                return false;
            }
        }
        for (col, map) in self.zones.cols.iter().zip(self.zones.rows.iter()) {
            if !col.contains(at) {
                continue;
            }
            let row = (at.y - col.y) as usize;
            // A heading or a spacer is `None`: the interleaving is why a screen
            // row is not a settings index.
            if let Some(Some(index)) = map.get(row).copied() {
                if index == self.sel {
                    return self.on_key(KeyEvent::from(KeyCode::Enter));
                }
                self.sel = index;
            }
        }
        false
    }

    pub fn on_key(&mut self, k: KeyEvent) -> bool {
        if let Some(buf) = self.editing.as_mut() {
            match k.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Enter => {
                    let v = buf.trim().to_string();
                    self.editing = None;
                    if !v.is_empty() {
                        self.set_draft(v);
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return false;
        }

        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.discard();
                self.show = false;
            }
            KeyCode::Char('a') => return self.apply(),
            KeyCode::Tab => {
                self.tab = (self.tab + 1) % TABS.len();
                self.select_first_in_tab();
            }
            KeyCode::BackTab => {
                self.tab = (self.tab + TABS.len() - 1) % TABS.len();
                self.select_first_in_tab();
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_in_tab(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_in_tab(-1),
            KeyCode::Home => self.select_first_in_tab(),
            KeyCode::End => {
                if let Some(index) = self.indices_in_tab().last().copied() {
                    self.sel = index;
                }
            }
            KeyCode::Enter => match &SETTINGS[self.sel].cycle {
                Cycle::Ring(ring) => {
                    let next = next_in(ring, &self.values[self.sel]);
                    self.set_draft(next);
                }
                Cycle::Prompt => self.editing = Some(self.values[self.sel].clone()),
            },
            _ => {}
        }
        false
    }

    fn indices_in_tab(&self) -> Vec<usize> {
        SETTINGS
            .iter()
            .enumerate()
            .filter_map(|(index, setting)| (setting_tab(setting.key) == self.tab).then_some(index))
            .collect()
    }

    fn select_first_in_tab(&mut self) {
        if let Some(index) = self.indices_in_tab().first().copied() {
            self.sel = index;
        }
    }

    fn move_in_tab(&mut self, delta: isize) {
        let indices = self.indices_in_tab();
        if indices.is_empty() {
            return;
        }
        let position = indices
            .iter()
            .position(|index| *index == self.sel)
            .unwrap_or(0);
        self.sel = indices[(position as isize + delta).rem_euclid(indices.len() as isize) as usize];
    }
}

const TABS: [&str; 4] = ["Common", "Projects", "Commands", "Ports"];

fn setting_tab(key: &str) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_cycles_and_wraps() {
        let ring = &["workspace", "tab", "split", "pane"];
        assert_eq!(next_in(ring, "workspace"), "tab");
        assert_eq!(next_in(ring, "pane"), "workspace");
    }

    #[test]
    fn draw_renders_grouped_rows_with_heading_value_and_hint() {
        let mut settings = Settings::new(&Config::default());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, &mut settings))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = (0..24)
            .map(|y| {
                (0..90)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Both columns' headings, the first setting's value, and the selected row's
        // hint all render inside the floating card.
        assert!(screen.contains("Open"), "{screen}");
        assert!(screen.contains("Projects"), "{screen}"); // active package tab
        assert!(screen.contains("Clone"), "{screen}"); // the right column
        assert!(screen.contains("default_target"), "{screen}");
        assert!(screen.contains("workspace"), "{screen}");
        assert!(screen.contains("where Enter opens a repo"), "{screen}");
        // The selected row (row 0) carries the ▌ marker, and the card is boxed.
        assert!(screen.contains('▌'), "{screen}");
        assert!(screen.contains('╭'), "{screen}");
    }

    #[test]
    fn unknown_value_restarts_the_ring() {
        // The `*)` fallback: a hand-edited or empty value lands on the first.
        assert_eq!(next_in(&["true", "false"], ""), "true");
        assert_eq!(next_in(&["true", "false"], "yes"), "true");
    }

    #[test]
    fn write_replaces_namespaced_value_and_keeps_comments_and_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("switchboard-set-{}", std::process::id()));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            "# a comment\n[projects]\nsort = \"name\"\nunknown_key = \"keep\"\n",
        )
        .unwrap();

        write_setting(&path, "sort", "recent").unwrap();
        let text = fs::read_to_string(&path).unwrap();

        assert!(text.contains("# a comment"));
        assert!(text.contains("sort = \"recent\""));
        assert!(text.contains("unknown_key = \"keep\""));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_appends_a_missing_key() {
        let dir = std::env::temp_dir().join(format!("switchboard-app-{}", std::process::id()));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "[projects]\nsort = \"name\"\n").unwrap();

        write_setting(&path, "label", "path").unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("sort = \"name\""));
        assert!(text.contains("label = \"path\""));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_numeric_setting_is_rejected_without_changing_file() {
        let dir = std::env::temp_dir().join(format!("switchboard-invalid-{}", std::process::id()));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        let original = toml::to_string_pretty(&Config::default()).unwrap();
        fs::write(&path, &original).unwrap();

        assert!(write_setting(&path, "refresh_interval_ms", "1").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_dir_all(dir).ok();
    }

    /// A number written as a string parses as TOML but fails `Config` — and
    /// `Config::try_load` failing takes down every surface in the plugin, not
    /// just the one whose setting was edited. So every value this form writes
    /// has to survive a round trip back through the typed config.
    #[test]
    fn every_setting_this_form_writes_still_loads_as_config() {
        let dir =
            std::env::temp_dir().join(format!("switchboard-roundtrip-{}", std::process::id()));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, toml::to_string_pretty(&Config::default()).unwrap()).unwrap();

        for setting in SETTINGS {
            let value = match &setting.cycle {
                Cycle::Ring(ring) => ring[ring.len() - 1],
                Cycle::Prompt => continue,
            };
            write_setting(&path, setting.key, value)
                .unwrap_or_else(|e| panic!("writing {} = {value:?}: {e}", setting.key));
            let text = fs::read_to_string(&path).unwrap();
            toml::from_str::<Config>(&text)
                .unwrap_or_else(|e| panic!("{} = {value:?} broke the config: {e}", setting.key));
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn every_setting_has_a_default_its_ring_accepts() {
        // A default outside its own ring would make the first Enter appear to do nothing.
        for s in SETTINGS {
            if let Cycle::Ring(ring) = &s.cycle {
                assert!(
                    ring.contains(&s.default),
                    "{} default {:?} is not in its ring",
                    s.key,
                    s.default
                );
            }
        }
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn enter_drafts_a_value_without_touching_disk() {
        let dir = std::env::temp_dir().join(format!("switchboard-draft-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut s = Settings::new(&Config::default());
        s.path = path.clone();
        s.open();
        // Enter on default_target (a ring) advances it, in memory only.
        s.on_key(key(KeyCode::Enter));
        assert_eq!(s.values[0], "tab");
        assert!(s.dirty(), "a staged change must read as unsaved");
        assert!(!path.exists(), "a draft must not write config.toml");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_writes_the_draft_and_clears_dirty() {
        let dir = std::env::temp_dir().join(format!("switchboard-apply-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut s = Settings::new(&Config::default());
        s.path = path.clone();
        s.open();
        s.on_key(key(KeyCode::Enter)); // draft default_target = "tab"
        let applied = s.on_key(key(KeyCode::Char('a'))); // apply
        assert!(
            applied,
            "a successful apply must report it, so the picker reloads"
        );
        assert!(!s.dirty(), "apply must adopt the draft as the baseline");
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[projects]") && text.contains("default_target = \"tab\""),
            "apply must persist the change: {text:?}"
        );
        // Applying again with nothing staged writes nothing and asks for no reload.
        assert!(
            !s.on_key(key(KeyCode::Char('a'))),
            "a no-op apply must not reload"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Zones only exist after a draw — the card is recentred every frame.
    fn drawn(s: &mut Settings) {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &Theme::default(), Color::Yellow, s))
            .unwrap();
    }

    #[test]
    fn clicking_a_settings_tab_selects_its_first_row() {
        let mut s = Settings::new(&Config::default());
        s.path = std::env::temp_dir().join("switchboard-never-written.toml");
        s.open();
        drawn(&mut s);

        let (a, _, index) = s.zones.tab_zones[0];
        assert_eq!(index, 0);
        assert!(!s.on_click(Position::new(a, s.zones.tab_row)));
        assert_eq!(s.tab, 0);
        assert_eq!(s.sel, s.indices_in_tab()[0]);
    }

    /// The one rule: a click selects, and a click on the row already selected
    /// does what Enter would — so a stray click can never write a setting.
    #[test]
    fn clicking_a_row_selects_it_and_clicking_it_again_cycles_it() {
        let mut s = Settings::new(&Config::default());
        s.path = std::env::temp_dir().join("switchboard-never-written.toml");
        s.open();
        drawn(&mut s);

        // Find a row in the left column that is not the selected one.
        let col = s.zones.cols[0];
        let (row, index) = s.zones.rows[0]
            .iter()
            .enumerate()
            .find_map(|(row, slot)| slot.filter(|&i| i != s.sel).map(|i| (row, i)))
            .expect("the left column draws more than one setting");
        let at = Position::new(col.x + 2, col.y + row as u16);

        assert!(!s.on_click(at));
        assert_eq!(s.sel, index, "the first click only moves the cursor");
        assert!(!s.dirty(), "and it changes nothing");

        let before = s.values[index].clone();
        drawn(&mut s);
        assert!(!s.on_click(at), "cycling stages a draft, it does not apply");
        assert_ne!(s.values[index], before, "the second click cycles the value");
        assert_eq!(s.saved[index], before, "nothing reached disk");
    }

    #[test]
    fn esc_discards_the_draft_and_closes() {
        let mut s = Settings::new(&Config::default());
        // Point away from the real config; esc must not write regardless.
        s.path = std::env::temp_dir().join("switchboard-never-written.toml");
        s.open();
        s.on_key(key(KeyCode::Enter)); // draft
        assert_eq!(s.values[0], "tab");
        s.on_key(key(KeyCode::Esc)); // discard + close
        assert!(!s.show);
        assert_eq!(s.values[0], "workspace", "esc must roll the draft back");
        assert!(!s.dirty());
    }
}
