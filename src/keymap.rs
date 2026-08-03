//! The picker's keymap: an ordered `chord → action` table per mode, built from
//! defaults and overridden by the flat config, with a LazyVim-flavoured modal
//! layer and a `␣` leader for the manage verbs.
//!
//! Shape follows a Telescope/LazyVim picker: you open **typing** (Insert), and
//! `Esc` drops to **Normal** where bare `hjkl`/`gg`/`G` move, `i`/`/` return to
//! Insert, the frequent opens sit on unshifted keys, and `␣` leads the rest.
//! Insert keeps lean `^`-chords for the opens and frees `^u`/`^w` for readline.
//!
//! Tables are ordered `Vec`s, not maps: the first chord bound to an action is the
//! one the footer and cheatsheet show, so display is deterministic and follows
//! the author's preference. `keys.<action> = "chord[,chord…]"` rebinds; the whole
//! surface — footer, cheatsheet, both modes — re-renders from these tables.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Accept as AcceptKind;
use crate::data::Config;

/// A key press reduced to what the keymap distinguishes: a base key plus the two
/// modifiers the picker uses. Shift is folded into the char case and [`Key::BackTab`].
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
}

impl Chord {
    /// How the chord reads in the footer and cheatsheet: `^t`, `⌥p`, `⇧⇥`, `g`, `↵`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push('^');
        }
        if self.alt {
            s.push('⌥');
        }
        s.push_str(&key_label(self.key));
        s
    }
}

/// The base keys a chord can carry.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

fn key_label(k: Key) -> String {
    match k {
        Key::Char(' ') => "␣".into(),
        Key::Char(c) => c.to_string(),
        Key::Enter => "↵".into(),
        Key::Esc => "esc".into(),
        Key::Tab => "⇥".into(),
        Key::BackTab => "⇧⇥".into(),
        Key::Backspace => "⌫".into(),
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::PageUp => "PgUp".into(),
        Key::PageDown => "PgDn".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
    }
}

/// Which keymap is live.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Mode {
    /// Type-to-filter: printable keys land in the query.
    Insert,
    /// Vim Normal: bare keys are commands, `i`/`/` return to Insert, `␣` leads.
    Normal,
}

/// What a chord does. `Accept` carries the terminal action the picker returns to
/// the caller; the rest mutate picker state in place.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Quit,
    Help,
    Changelog,
    Settings,
    NextGroup,
    PrevGroup,
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    TogglePreview,
    PreviewDown,
    PreviewUp,
    CycleSort,
    Backspace,
    ClearQuery,
    DeleteWord,
    EnterInsert,
    EnterNormal,
    Accept(AcceptKind),
}

impl Action {
    /// An accept returns out of the TUI; everything else stays in it.
    pub fn is_accept(&self) -> bool {
        matches!(self, Action::Accept(_))
    }
}

/// The action vocabulary, paired with the config name that rebinds it. One table
/// drives the override lookup and the docs — a new action is one row here.
const NAMES: &[(&str, Action)] = &[
    ("quit", Action::Quit),
    ("help", Action::Help),
    ("changelog", Action::Changelog),
    ("settings", Action::Settings),
    ("next_group", Action::NextGroup),
    ("prev_group", Action::PrevGroup),
    ("down", Action::Down),
    ("up", Action::Up),
    ("page_down", Action::PageDown),
    ("page_up", Action::PageUp),
    ("top", Action::Top),
    ("bottom", Action::Bottom),
    ("toggle_preview", Action::TogglePreview),
    ("preview_down", Action::PreviewDown),
    ("preview_up", Action::PreviewUp),
    ("cycle_sort", Action::CycleSort),
    ("clear_query", Action::ClearQuery),
    ("delete_word", Action::DeleteWord),
    ("insert_mode", Action::EnterInsert),
    ("normal_mode", Action::EnterNormal),
    ("open", Action::Accept(AcceptKind::Default)),
    ("clone", Action::Accept(AcceptKind::Clone)),
    ("update_plugin", Action::Accept(AcceptKind::UpdatePlugin)),
    ("workspace", Action::Accept(AcceptKind::Workspace)),
    ("tab", Action::Accept(AcceptKind::Tab)),
    ("split", Action::Accept(AcceptKind::Split)),
    ("pane", Action::Accept(AcceptKind::Pane)),
    ("update", Action::Accept(AcceptKind::Update)),
    ("remove", Action::Accept(AcceptKind::Remove)),
];

/// The ordered chord tables for both modes plus the Normal-mode `␣` leader group.
pub struct Keymap {
    insert: Vec<(Chord, Action)>,
    normal: Vec<(Chord, Action)>,
    /// Reached in Normal after the leader key; holds the manage/meta verbs.
    leader: Vec<(Chord, Action)>,
    /// The Normal-mode leader (default `␣`).
    pub leader_chord: Chord,
    start: Mode,
}

impl Keymap {
    /// Build the defaults, then apply `keys.*` overrides and `keymode`.
    pub fn load(cfg: &Config) -> Self {
        let start = match cfg.get("keymode", "insert").as_str() {
            "normal" | "modal" => Mode::Normal,
            _ => Mode::Insert,
        };
        let mut km = Keymap {
            insert: default_insert(),
            normal: default_normal(),
            leader: default_leader(),
            leader_chord: chord(Key::Char(' ')),
            start,
        };
        km.apply_overrides(cfg);
        km
    }

    /// The action a chord triggers in `mode`, if any.
    pub fn action(&self, mode: Mode, ch: Chord) -> Option<Action> {
        let list = match mode {
            Mode::Insert => &self.insert,
            Mode::Normal => &self.normal,
        };
        list.iter().find(|(c, _)| *c == ch).map(|(_, a)| *a)
    }

    /// The action a chord triggers after the Normal-mode leader, if any.
    pub fn leader_action(&self, ch: Chord) -> Option<Action> {
        self.leader.iter().find(|(c, _)| *c == ch).map(|(_, a)| *a)
    }

    /// The mode the picker starts in.
    pub fn start_mode(&self) -> Mode {
        self.start
    }

    /// How `action` reads in `mode`, for the footer and cheatsheet. Manage verbs
    /// that live behind the leader in Normal render as `␣g`, `␣x`, and so on.
    pub fn label_for(&self, mode: Mode, action: Action) -> Option<String> {
        let list = match mode {
            Mode::Insert => &self.insert,
            Mode::Normal => &self.normal,
        };
        if let Some((c, _)) = list.iter().find(|(_, a)| *a == action) {
            return Some(c.label());
        }
        if mode == Mode::Normal {
            if let Some((c, _)) = self.leader.iter().find(|(_, a)| *a == action) {
                // A space between the leader glyph and the key: `␣ g`, not `␣g`,
                // which reads as one smudged symbol at terminal cell spacing.
                return Some(format!("{} {}", self.leader_chord.label(), c.label()));
            }
        }
        None
    }

    /// Rebind actions the config names. `keys.<action> = "chord[,chord…]"` clears
    /// that action's chords in every table and binds the listed ones (first = the
    /// one shown). An unparseable chord is skipped, so a typo cannot silently
    /// unbind an action.
    fn apply_overrides(&mut self, cfg: &Config) {
        for (name, act) in NAMES {
            let spec = cfg.get(&format!("keys.{name}"), "");
            if spec.is_empty() {
                continue;
            }
            let chords: Vec<Chord> = spec.split(',').filter_map(parse_chord).collect();
            if chords.is_empty() {
                continue;
            }
            self.insert.retain(|(_, a)| a != act);
            self.normal.retain(|(_, a)| a != act);
            self.leader.retain(|(_, a)| a != act);
            // Prepend so the override wins as the displayed chord.
            for ch in chords.into_iter().rev() {
                self.insert.insert(0, (ch, *act));
                self.normal.insert(0, (ch, *act));
            }
        }
    }
}

fn chord(key: Key) -> Chord {
    Chord {
        key,
        ctrl: false,
        alt: false,
    }
}

fn ctrl(key: Key) -> Chord {
    Chord {
        key,
        ctrl: true,
        alt: false,
    }
}

fn alt(key: Key) -> Chord {
    Chord {
        key,
        ctrl: false,
        alt: true,
    }
}

/// Insert (type-to-filter): lean `^`-chords for the opens, `⌥` for view/meta,
/// and `^u`/`^w` left to readline. `Esc` drops to Normal; `^c` closes.
fn default_insert() -> Vec<(Chord, Action)> {
    use Action::*;
    vec![
        (chord(Key::Enter), Accept(AcceptKind::Default)),
        (alt(Key::Enter), Accept(AcceptKind::Clone)),
        (ctrl(Key::Char('j')), Down),
        (ctrl(Key::Char('n')), Down),
        (ctrl(Key::Char('k')), Up),
        (ctrl(Key::Char('p')), Up),
        (chord(Key::Down), Down),
        (chord(Key::Up), Up),
        (chord(Key::PageDown), PageDown),
        (chord(Key::PageUp), PageUp),
        (chord(Key::Tab), NextGroup),
        (chord(Key::BackTab), PrevGroup),
        (ctrl(Key::Char('t')), Accept(AcceptKind::Tab)),
        (ctrl(Key::Char('v')), Accept(AcceptKind::Split)),
        (ctrl(Key::Char('o')), Accept(AcceptKind::Pane)),
        (alt(Key::Char('w')), Accept(AcceptKind::Workspace)),
        (ctrl(Key::Char('r')), Accept(AcceptKind::Update)),
        (ctrl(Key::Char('x')), Accept(AcceptKind::Remove)),
        (alt(Key::Char('p')), TogglePreview),
        (alt(Key::Char('j')), PreviewDown),
        (alt(Key::Char('k')), PreviewUp),
        (alt(Key::Char('s')), CycleSort),
        (alt(Key::Char('c')), Changelog),
        (alt(Key::Char('u')), Accept(AcceptKind::UpdatePlugin)),
        (alt(Key::Char(',')), Settings),
        (ctrl(Key::Char('u')), ClearQuery),
        (ctrl(Key::Char('w')), DeleteWord),
        (chord(Key::Backspace), Backspace),
        (chord(Key::Char('?')), Help),
        (ctrl(Key::Char('c')), Quit),
        (chord(Key::Esc), EnterNormal),
    ]
}

/// Normal (Vim): bare motion, unshifted opens, `i`/`/` to filter, `␣` for the
/// rest. `q`/`Esc` close.
fn default_normal() -> Vec<(Chord, Action)> {
    use Action::*;
    vec![
        (chord(Key::Char('j')), Down),
        (chord(Key::Char('k')), Up),
        (chord(Key::Down), Down),
        (chord(Key::Up), Up),
        (chord(Key::Char('g')), Top),
        (chord(Key::Char('G')), Bottom),
        (ctrl(Key::Char('d')), PageDown),
        (ctrl(Key::Char('u')), PageUp),
        (chord(Key::PageDown), PageDown),
        (chord(Key::PageUp), PageUp),
        (chord(Key::Char('L')), NextGroup),
        (chord(Key::Char('H')), PrevGroup),
        (chord(Key::Tab), NextGroup),
        (chord(Key::BackTab), PrevGroup),
        (chord(Key::Char('i')), EnterInsert),
        (chord(Key::Char('/')), EnterInsert),
        (chord(Key::Enter), Accept(AcceptKind::Default)),
        (chord(Key::Char('t')), Accept(AcceptKind::Tab)),
        (chord(Key::Char('v')), Accept(AcceptKind::Split)),
        (chord(Key::Char('o')), Accept(AcceptKind::Pane)),
        (chord(Key::Char('w')), Accept(AcceptKind::Workspace)),
        (chord(Key::Char('p')), TogglePreview),
        (alt(Key::Char('j')), PreviewDown),
        (alt(Key::Char('k')), PreviewUp),
        (chord(Key::Char('?')), Help),
        (chord(Key::Char('q')), Quit),
        (chord(Key::Esc), Quit),
    ]
}

/// The Normal-mode `␣` leader group: the manage + meta verbs, so Normal keeps
/// its bare letters for motion and the frequent opens.
fn default_leader() -> Vec<(Chord, Action)> {
    use Action::*;
    vec![
        (chord(Key::Char('u')), Accept(AcceptKind::Update)),
        (chord(Key::Char('x')), Accept(AcceptKind::Remove)),
        (chord(Key::Char('c')), Accept(AcceptKind::Clone)),
        (chord(Key::Char('s')), CycleSort),
        (chord(Key::Char('l')), Changelog),
        (chord(Key::Char(',')), Settings),
        (chord(Key::Char('U')), Accept(AcceptKind::UpdatePlugin)),
    ]
}

/// Reduce a crossterm key event to a [`Chord`], or `None` for keys the picker
/// does not model. Shift is baked into the char, so it is not tracked except as
/// [`Key::BackTab`].
pub fn chord_of(k: &KeyEvent) -> Option<Chord> {
    let key = match k.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    };
    Some(Chord {
        key,
        ctrl: k.modifiers.contains(KeyModifiers::CONTROL),
        alt: k.modifiers.contains(KeyModifiers::ALT),
    })
}

/// Parse a config chord spec like `ctrl-j`, `alt-p`, `shift-tab`, `enter`, `g`.
/// Modifiers precede the key, `-`-separated; the last segment is the key.
pub fn parse_chord(spec: &str) -> Option<Chord> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let parts: Vec<&str> = spec.split('-').collect();
    let (mods, key_part) = parts.split_at(parts.len() - 1);
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "c" | "^" => ctrl = true,
            "alt" | "opt" | "meta" | "a" | "m" => alt = true,
            "shift" => shift = true,
            _ => return None,
        }
    }
    let key = parse_key(key_part[0], shift)?;
    Some(Chord { key, ctrl, alt })
}

fn parse_key(name: &str, shift: bool) -> Option<Key> {
    let key = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" | "cr" => Key::Enter,
        "esc" | "escape" => Key::Esc,
        "tab" if shift => Key::BackTab,
        "tab" => Key::Tab,
        "backtab" => Key::BackTab,
        "backspace" | "bs" => Key::Backspace,
        "up" => Key::Up,
        "down" => Key::Down,
        "pgup" | "pageup" => Key::PageUp,
        "pgdn" | "pagedown" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "space" => Key::Char(' '),
        s if s.chars().count() == 1 => {
            let c = name.chars().next().unwrap();
            return Some(Key::Char(if shift { c.to_ascii_uppercase() } else { c }));
        }
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chord_reads_modifiers_and_named_keys() {
        assert_eq!(
            parse_chord("ctrl-j"),
            Some(Chord {
                key: Key::Char('j'),
                ctrl: true,
                alt: false
            })
        );
        assert_eq!(parse_chord("shift-tab"), Some(chord(Key::BackTab)));
        assert_eq!(parse_chord("enter"), Some(chord(Key::Enter)));
        assert_eq!(parse_chord("space"), Some(chord(Key::Char(' '))));
        assert!(parse_chord("bogusmod-j").is_none());
        assert!(parse_chord("").is_none());
    }

    #[test]
    fn chord_labels_read_the_way_the_footer_shows_them() {
        assert_eq!(ctrl(Key::Char('t')).label(), "^t");
        assert_eq!(alt(Key::Char('p')).label(), "⌥p");
        assert_eq!(chord(Key::Enter).label(), "↵");
        assert_eq!(chord(Key::BackTab).label(), "⇧⇥");
        assert_eq!(chord(Key::Char(' ')).label(), "␣");
    }

    #[test]
    fn insert_is_the_lean_default_and_frees_readline() {
        let km = Keymap::load(&Config::default());
        assert_eq!(km.start_mode(), Mode::Insert);
        assert_eq!(
            km.action(Mode::Insert, chord(Key::Enter)),
            Some(Action::Accept(AcceptKind::Default))
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('v'))),
            Some(Action::Accept(AcceptKind::Split))
        );
        // ^u/^w are readline editing, not actions.
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('u'))),
            Some(Action::ClearQuery)
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('w'))),
            Some(Action::DeleteWord)
        );
        // Esc drops to Normal rather than quitting; ^c quits.
        assert_eq!(
            km.action(Mode::Insert, chord(Key::Esc)),
            Some(Action::EnterNormal)
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('c'))),
            Some(Action::Quit)
        );
        // A bare letter falls through to the query.
        assert_eq!(km.action(Mode::Insert, chord(Key::Char('t'))), None);
    }

    #[test]
    fn normal_has_bare_motion_and_a_leader_for_manage_verbs() {
        let km = Keymap::load(&Config::default());
        assert_eq!(
            km.action(Mode::Normal, chord(Key::Char('j'))),
            Some(Action::Down)
        );
        assert_eq!(
            km.action(Mode::Normal, chord(Key::Char('g'))),
            Some(Action::Top)
        );
        assert_eq!(
            km.action(Mode::Normal, chord(Key::Char('i'))),
            Some(Action::EnterInsert)
        );
        assert_eq!(
            km.action(Mode::Normal, chord(Key::Char('t'))),
            Some(Action::Accept(AcceptKind::Tab))
        );
        // the manage verbs live behind the leader, not on bare Normal keys…
        assert_eq!(
            km.leader_action(chord(Key::Char('u'))),
            Some(Action::Accept(AcceptKind::Update))
        );
        // …and their labels read as the two-key sequence.
        assert_eq!(
            km.label_for(Mode::Normal, Action::Accept(AcceptKind::Update))
                .as_deref(),
            Some("␣ u")
        );
    }

    #[test]
    fn settings_is_reachable_in_both_modes() {
        let km = Keymap::load(&Config::default());
        // Insert: ⌥, opens the in-picker settings overlay.
        assert_eq!(
            km.action(Mode::Insert, alt(Key::Char(','))),
            Some(Action::Settings)
        );
        // Normal: behind the leader, shown as `␣ ,`.
        assert_eq!(
            km.leader_action(chord(Key::Char(','))),
            Some(Action::Settings)
        );
        assert_eq!(
            km.label_for(Mode::Normal, Action::Settings).as_deref(),
            Some("␣ ,")
        );
    }

    #[test]
    fn keymode_normal_starts_in_normal() {
        let km = Keymap::load(&Config::from_pairs(&[("keymode", "normal")]));
        assert_eq!(km.start_mode(), Mode::Normal);
    }

    #[test]
    fn an_override_rebinds_and_becomes_the_shown_chord() {
        let km = Keymap::load(&Config::from_pairs(&[("keys.tab", "ctrl-y")]));
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('y'))),
            Some(Action::Accept(AcceptKind::Tab))
        );
        // The default ^t is gone, and the footer would now show ^y.
        assert_eq!(km.action(Mode::Insert, ctrl(Key::Char('t'))), None);
        assert_eq!(
            km.label_for(Mode::Insert, Action::Accept(AcceptKind::Tab))
                .as_deref(),
            Some("^y")
        );
    }
}
