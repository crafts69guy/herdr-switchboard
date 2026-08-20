//! Settings form: the switcher's TUI vocabulary applied to its typed,
//! namespaced `config.toml`.
//!
//! Settings are navigated as a fixed form in the picker's colours and
//! command-bar pills.
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
//! config and updates its live state (see `projects::App::reload_config`) — an applied change takes
//! effect in the running session, no relaunch or server reload needed.

use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use ratatui::Frame;

use crate::data::{Config, Theme};
use crate::surface::{Surface, Transition};
mod catalog;
mod document;
mod view;

use catalog::{next_in, Cycle, SETTINGS};
use document::{config_path, load_document, set_document_value, write_document};
pub use view::draw;
use view::{setting_tab, TABS};

#[cfg(test)]
use document::write_setting;
#[cfg(test)]
use std::fs;

/// Standalone settings action. Projects also embeds this same form, so both
/// entry points preserve the draft/apply behavior and namespaced writer.
pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let title = theme
        .resolve(&cfg.common.title_color)
        .unwrap_or_else(|| theme.or("peach", Color::Yellow));
    let mut surface = StandaloneSettings {
        settings: Settings::new(&cfg),
        background: crate::tui::SurfaceBackground::resolve(&theme, cfg.common.transparency),
        theme,
        title,
    };
    surface.settings.open();
    crate::surface::run(&mut surface)
}

struct StandaloneSettings {
    settings: Settings,
    background: crate::tui::SurfaceBackground,
    theme: Theme,
    title: Color,
}

impl Surface for StandaloneSettings {
    type Output = ();

    fn draw(&mut self, frame: &mut Frame) {
        self.background.paint(frame, frame.area());
        draw(
            frame,
            frame.area(),
            &self.theme,
            self.background,
            self.title,
            &mut self.settings,
        );
    }

    fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.settings.on_key(key);
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => self.settings.on_wheel(1),
                MouseEventKind::ScrollUp => self.settings.on_wheel(-1),
                MouseEventKind::Down(MouseButton::Left) => {
                    self.settings
                        .on_click(Position::new(mouse.column, mouse.row));
                }
                _ => return Ok(Transition::Wait),
            },
            _ => return Ok(Transition::Wait),
        }
        Ok(if self.settings.show {
            Transition::Redraw
        } else {
            Transition::Exit(())
        })
    }
}

/// How Enter changes a setting.
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
        let values: Vec<String> = SETTINGS
            .iter()
            .map(|setting| {
                cfg.value_for_cli(setting.key)
                    .unwrap_or_else(|| setting.default.into())
            })
            .collect();
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
        let theme = Theme::default();
        let background = crate::tui::SurfaceBackground::resolve(
            &theme,
            crate::config::Transparency::Transparent,
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| {
            draw(
                f,
                f.area(),
                &theme,
                background,
                Color::Yellow,
                &mut settings,
            )
        })
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
    fn opaque_standalone_settings_fills_the_whole_pane() {
        let theme = Theme::from_slots(&[("panel_bg", "#101214")]);
        let mut surface = StandaloneSettings {
            settings: Settings::new(&Config::default()),
            background: crate::tui::SurfaceBackground::resolve(
                &theme,
                crate::config::Transparency::Opaque,
            ),
            theme,
            title: Color::Yellow,
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| surface.draw(frame)).unwrap();
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg != Color::Reset));
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
        let theme = Theme::default();
        let background = crate::tui::SurfaceBackground::resolve(
            &theme,
            crate::config::Transparency::Transparent,
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &theme, background, Color::Yellow, s))
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
