//! The picker's keymap: an ordered `chord → action` table per mode, built from
//! defaults and overridden by the Projects keymap, with a LazyVim-flavoured modal
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
use crate::config::{Config, KeyMode};

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

    /// Crossterm representation used by the shared picker engine. Keeping this
    /// conversion beside the parser prevents each surface from inventing a
    /// slightly different chord grammar.
    pub fn event_parts(self) -> (KeyCode, KeyModifiers) {
        let mut modifiers = KeyModifiers::NONE;
        if self.ctrl {
            modifiers.insert(KeyModifiers::CONTROL);
        }
        if self.alt {
            modifiers.insert(KeyModifiers::ALT);
        }
        let code = match self.key {
            Key::Char(c) => KeyCode::Char(c),
            Key::Enter => KeyCode::Enter,
            Key::Esc => KeyCode::Esc,
            Key::Tab => KeyCode::Tab,
            Key::BackTab => {
                modifiers.insert(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            Key::Backspace => KeyCode::Backspace,
            Key::Up => KeyCode::Up,
            Key::Down => KeyCode::Down,
            Key::PageUp => KeyCode::PageUp,
            Key::PageDown => KeyCode::PageDown,
            Key::Home => KeyCode::Home,
            Key::End => KeyCode::End,
        };
        (code, modifiers)
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
    CopyPath,
    SendToAgent,
    ToggleStar,
    Backspace,
    ClearQuery,
    DeleteWord,
    EnterInsert,
    EnterNormal,
    Accept(AcceptKind),
}

impl Action {
    pub fn needs_selection(&self) -> bool {
        matches!(
            self,
            Action::Accept(
                AcceptKind::Default
                    | AcceptKind::Workspace
                    | AcceptKind::Tab
                    | AcceptKind::Split
                    | AcceptKind::Pane
                    | AcceptKind::Update
                    | AcceptKind::Remove
            ) | Action::CopyPath
                | Action::SendToAgent
                | Action::ToggleStar
        )
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
    ("copy_path", Action::CopyPath),
    ("send_to_agent", Action::SendToAgent),
    ("star", Action::ToggleStar),
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

/// The ordered chord tables for both modes. Every modified chord appears in
/// *both*, with the same action — see [`default_insert`] for why.
pub struct Keymap {
    insert: Vec<(Chord, Action)>,
    normal: Vec<(Chord, Action)>,
    start: Mode,
}

impl Keymap {
    /// Build the defaults, then apply `keys.*` overrides and `keymode`.
    pub fn load(cfg: &Config) -> Self {
        let start = match cfg.common.keymode {
            KeyMode::Normal => Mode::Normal,
            KeyMode::Insert => Mode::Insert,
        };
        let mut km = Keymap {
            insert: default_insert(),
            normal: default_normal(),
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
        list.iter()
            .find(|(_, a)| *a == action)
            .map(|(c, _)| c.label())
    }

    /// Rebind actions the config names. `keys.<action> = "chord[,chord…]"` clears
    /// that action's chords in every table and binds the listed ones (first = the
    /// one shown). An unparseable chord is skipped, so a typo cannot silently
    /// unbind an action.
    fn apply_overrides(&mut self, cfg: &Config) {
        for (name, act) in NAMES {
            let Some(spec) = cfg.keys.get("projects").and_then(|keys| keys.get(*name)) else {
                continue;
            };
            let chords: Vec<Chord> = spec.split(',').filter_map(parse_chord).collect();
            if chords.is_empty() {
                continue;
            }
            self.insert.retain(|(_, a)| a != act);
            self.normal.retain(|(_, a)| a != act);
            // A configured chord owns its slot. This also lets an existing
            // remap such as `tab = "ctrl-y"` override a newly introduced default
            // without leaving two footer pills that advertise the same key.
            for chord in &chords {
                self.insert.retain(|(bound, _)| bound != chord);
                self.normal.retain(|(bound, _)| bound != chord);
            }
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

/// The prefix says what *kind* of thing a key does. It never says which mode you
/// are in, which is why [`default_normal`] repeats every modified chord below
/// verbatim rather than respelling it:
///
/// - `↵` runs the selected row's primary action; `^↵`/`⌥↵` are its variants on
///   the surfaces that have them (Commands, Ports, the fnm manager).
/// - **`^<letter>` acts on the selected row** — open it, update it, remove it,
///   copy it, send it, star it.
/// - **`⌥<letter>` changes the view or the app** — preview, sort, clone,
///   changelog, settings, plugin update. It touches no row. The one named
///   exception is that `⌥<letter>` is also the *heavier* form of the `^<letter>`
///   verb on the same letter (Ports `^x` TERM → `⌥x` KILL), the same way `⌥↵`
///   is a variant of `↵`.
///
/// A `␣` leader used to hold the manage verbs, and it is gone: space can only be
/// a leader in Normal — in Insert it is a character the user is typing — so any
/// group living there was forced to change prefix with the mode, which is the
/// exact inconsistency this layout exists to remove. Normal instead adds *bare*
/// aliases on the same letter as the `^` chord (`t`/`v`/`o`/`w`, `p`).
///
/// Two carve-outs, both deliberate: motion follows the idiom of its mode
/// (readline `^j`/`^n`/`^k`/`^p` here, Vim `j`/`k`/`g`/`G`/`^d`/`^u` there), and
/// query editing exists only where there is a query — `^u` clears it and `⌥⌫`
/// deletes a word. `^u` and `^c` are reserved everywhere; no picker action may
/// take them.
fn default_insert() -> Vec<(Chord, Action)> {
    use Action::*;
    vec![
        // Motion, in this mode's idiom.
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
        // Act on the selected row.
        (chord(Key::Enter), Accept(AcceptKind::Default)),
        (ctrl(Key::Char('t')), Accept(AcceptKind::Tab)),
        (ctrl(Key::Char('v')), Accept(AcceptKind::Split)),
        (ctrl(Key::Char('o')), Accept(AcceptKind::Pane)),
        (ctrl(Key::Char('w')), Accept(AcceptKind::Workspace)),
        (ctrl(Key::Char('r')), Accept(AcceptKind::Update)),
        (ctrl(Key::Char('x')), Accept(AcceptKind::Remove)),
        (ctrl(Key::Char('y')), CopyPath),
        (ctrl(Key::Char('a')), SendToAgent),
        (ctrl(Key::Char('s')), ToggleStar),
        // Change the view or the app.
        (alt(Key::Char('p')), TogglePreview),
        (alt(Key::Char('j')), PreviewDown),
        (alt(Key::Char('k')), PreviewUp),
        (alt(Key::Char('s')), CycleSort),
        (alt(Key::Char('l')), Accept(AcceptKind::Clone)),
        (alt(Key::Char('h')), Changelog),
        (alt(Key::Char('u')), Accept(AcceptKind::UpdatePlugin)),
        (alt(Key::Char(',')), Settings),
        // Query editing, which only this mode has.
        (ctrl(Key::Char('u')), ClearQuery),
        (alt(Key::Backspace), DeleteWord),
        (chord(Key::Backspace), Backspace),
        (chord(Key::Char('?')), Help),
        (ctrl(Key::Char('c')), Quit),
        (chord(Key::Esc), EnterNormal),
    ]
}

/// Normal (Vim). Every modified chord here is the one [`default_insert`] binds,
/// spelled identically — the bare letters are *additions*, not replacements, and
/// each one carries the same letter as the `^` chord it shadows. `q`/`Esc` close
/// alongside the `^c` that closes in both modes. The one thing Normal does not
/// repeat is query editing, because there is nothing to edit until you are
/// typing: `^u` is Vim's half-page up here and clears the query there.
fn default_normal() -> Vec<(Chord, Action)> {
    use Action::*;
    vec![
        // Motion, in this mode's idiom.
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
        // Act on the selected row.
        (chord(Key::Enter), Accept(AcceptKind::Default)),
        (ctrl(Key::Char('t')), Accept(AcceptKind::Tab)),
        (ctrl(Key::Char('v')), Accept(AcceptKind::Split)),
        (ctrl(Key::Char('o')), Accept(AcceptKind::Pane)),
        (ctrl(Key::Char('w')), Accept(AcceptKind::Workspace)),
        (ctrl(Key::Char('r')), Accept(AcceptKind::Update)),
        (ctrl(Key::Char('x')), Accept(AcceptKind::Remove)),
        (ctrl(Key::Char('y')), CopyPath),
        (ctrl(Key::Char('a')), SendToAgent),
        (ctrl(Key::Char('s')), ToggleStar),
        // Change the view or the app.
        (alt(Key::Char('p')), TogglePreview),
        (alt(Key::Char('j')), PreviewDown),
        (alt(Key::Char('k')), PreviewUp),
        (alt(Key::Char('s')), CycleSort),
        (alt(Key::Char('l')), Accept(AcceptKind::Clone)),
        (alt(Key::Char('h')), Changelog),
        (alt(Key::Char('u')), Accept(AcceptKind::UpdatePlugin)),
        (alt(Key::Char(',')), Settings),
        // Bare aliases: the same letters, one keystroke shorter.
        (chord(Key::Char('t')), Accept(AcceptKind::Tab)),
        (chord(Key::Char('v')), Accept(AcceptKind::Split)),
        (chord(Key::Char('o')), Accept(AcceptKind::Pane)),
        (chord(Key::Char('w')), Accept(AcceptKind::Workspace)),
        (chord(Key::Char('p')), TogglePreview),
        (chord(Key::Char('?')), Help),
        (ctrl(Key::Char('c')), Quit),
        (chord(Key::Char('q')), Quit),
        (chord(Key::Esc), Quit),
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
    fn insert_mode_is_lean_and_frees_readline() {
        let mut cfg = Config::default();
        cfg.common.keymode = KeyMode::Insert;
        let km = Keymap::load(&cfg);
        assert_eq!(km.start_mode(), Mode::Insert);
        assert_eq!(
            km.action(Mode::Insert, chord(Key::Enter)),
            Some(Action::Accept(AcceptKind::Default))
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('v'))),
            Some(Action::Accept(AcceptKind::Split))
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('s'))),
            Some(Action::ToggleStar)
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('a'))),
            Some(Action::SendToAgent)
        );
        // Query editing is Insert-only, and `^w` is not part of it: that chord
        // opens the row in a workspace, so deleting a word is `⌥⌫`.
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('u'))),
            Some(Action::ClearQuery)
        );
        assert_eq!(
            km.action(Mode::Insert, alt(Key::Backspace)),
            Some(Action::DeleteWord)
        );
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('w'))),
            Some(Action::Accept(AcceptKind::Workspace))
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
    fn normal_adds_bare_aliases_without_respelling_a_single_chord() {
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
        // The manage verbs used to hide behind a `␣` leader. They are now the
        // same chords Insert uses, and space is an ordinary unbound key.
        assert_eq!(km.action(Mode::Normal, chord(Key::Char(' '))), None);
        assert_eq!(
            km.action(Mode::Normal, ctrl(Key::Char('r'))),
            Some(Action::Accept(AcceptKind::Update))
        );
        assert_eq!(
            km.action(Mode::Normal, ctrl(Key::Char('x'))),
            Some(Action::Accept(AcceptKind::Remove))
        );
        assert_eq!(
            km.action(Mode::Normal, alt(Key::Char('s'))),
            Some(Action::CycleSort)
        );
        assert_eq!(
            km.label_for(Mode::Normal, Action::CycleSort).as_deref(),
            Some("⌥s")
        );
        // A bare alias never displaces the chord: `t` and `^t` both open a tab,
        // and the footer shows the one Insert would show.
        assert_eq!(
            km.action(Mode::Normal, ctrl(Key::Char('t'))),
            Some(Action::Accept(AcceptKind::Tab))
        );
        assert_eq!(
            km.label_for(Mode::Normal, Action::Accept(AcceptKind::Tab))
                .as_deref(),
            Some("^t")
        );
        assert_eq!(
            km.action(Mode::Normal, ctrl(Key::Char('y'))),
            Some(Action::CopyPath)
        );
        assert_eq!(
            km.action(Mode::Normal, ctrl(Key::Char('a'))),
            Some(Action::SendToAgent)
        );
        assert_eq!(
            km.action(Mode::Normal, ctrl(Key::Char('s'))),
            Some(Action::ToggleStar)
        );
        assert_eq!(
            km.label_for(Mode::Normal, Action::CopyPath).as_deref(),
            Some("^y")
        );
        assert_eq!(
            km.label_for(Mode::Normal, Action::ToggleStar).as_deref(),
            Some("^s")
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
        // Normal: the same chord, not a respelling of it.
        assert_eq!(
            km.action(Mode::Normal, alt(Key::Char(','))),
            Some(Action::Settings)
        );
        assert_eq!(
            km.label_for(Mode::Normal, Action::Settings).as_deref(),
            Some("⌥,")
        );
    }

    /// The two vocabularies the prefix rule deliberately does not govern:
    /// motion follows the idiom of its mode, query editing exists only where
    /// there is a query, and the session keys (`esc`, `i`, `/`, `q`, `^c`) name
    /// a way in or out rather than a thing to do to a row.
    fn mode_idiomatic(action: Action) -> bool {
        matches!(
            action,
            Action::Down
                | Action::Up
                | Action::PageDown
                | Action::PageUp
                | Action::Top
                | Action::Bottom
                | Action::NextGroup
                | Action::PrevGroup
                | Action::ClearQuery
                | Action::DeleteWord
                | Action::Backspace
                | Action::EnterInsert
                | Action::EnterNormal
                | Action::Quit
        )
    }

    /// The concept, asserted rather than described: a chord that carries a
    /// modifier means the same thing in both modes. Insert is the reference —
    /// Normal may *add* (bare aliases, Vim motion) but may never respell.
    ///
    /// Motion and query editing are the two carve-outs, and they are named here
    /// rather than inferred: navigation follows the idiom of its mode, and there
    /// is nothing to edit until you are typing.
    #[test]
    fn every_modified_chord_means_the_same_thing_in_both_modes() {
        let km = Keymap::load(&Config::default());
        for (ch, action) in km.insert.iter().filter(|(c, _)| c.ctrl || c.alt) {
            if mode_idiomatic(*action) {
                continue;
            }
            assert_eq!(
                km.action(Mode::Normal, *ch),
                Some(*action),
                "{} is {action:?} in Insert but not in Normal",
                ch.label()
            );
        }
        for (ch, action) in km.normal.iter().filter(|(c, _)| c.ctrl || c.alt) {
            if mode_idiomatic(*action) {
                continue;
            }
            assert_eq!(
                km.action(Mode::Insert, *ch),
                Some(*action),
                "{} is {action:?} in Normal but not in Insert",
                ch.label()
            );
        }
    }

    /// `^` acts on the selected row and `⌥` changes the view or the app. An
    /// action that appears in both families has no single answer to "which kind
    /// of thing is this", which is how a keymap starts drifting again.
    #[test]
    fn no_action_straddles_the_ctrl_and_alt_families() {
        let km = Keymap::load(&Config::default());
        for table in [&km.insert, &km.normal] {
            for (chord, action) in table {
                if !chord.ctrl {
                    continue;
                }
                assert!(
                    !table
                        .iter()
                        .any(|(other, a)| other.alt && !other.ctrl && a == action),
                    "{action:?} is bound with both ^ and ⌥ in one mode"
                );
            }
        }
    }

    /// A Normal-mode bare letter is a shorthand for a chord, never a rename of
    /// one: `t` opens a tab because `^t` does. A bare letter carrying a
    /// different letter than its chord is the drift this layout removed.
    #[test]
    fn a_bare_alias_carries_the_same_letter_as_its_chord() {
        let km = Keymap::load(&Config::default());
        for (bare, action) in km.normal.iter().filter(|(c, _)| !c.ctrl && !c.alt) {
            let Key::Char(letter) = bare.key else {
                continue;
            };
            if mode_idiomatic(*action) {
                continue;
            }
            let Some(chord) = km
                .normal
                .iter()
                .find(|(c, a)| (c.ctrl || c.alt) && a == action)
                .map(|(c, _)| *c)
            else {
                continue; // motion and the mode keys have no chord form
            };
            assert_eq!(
                chord.key,
                Key::Char(letter),
                "bare {letter} and {} are the same action on different letters",
                chord.label()
            );
        }
    }

    #[test]
    fn keymode_normal_starts_in_normal() {
        let km = Keymap::load(&Config::default());
        assert_eq!(km.start_mode(), Mode::Normal);
    }

    #[test]
    fn an_override_rebinds_and_becomes_the_shown_chord() {
        let mut cfg = Config::default();
        cfg.keys
            .entry("projects".into())
            .or_default()
            .insert("tab".into(), "ctrl-y".into());
        let km = Keymap::load(&cfg);
        assert_eq!(
            km.action(Mode::Insert, ctrl(Key::Char('y'))),
            Some(Action::Accept(AcceptKind::Tab))
        );
        // The default ^t is gone, and the footer would now show ^y.
        assert_eq!(km.action(Mode::Insert, ctrl(Key::Char('t'))), None);
        assert_eq!(km.label_for(Mode::Insert, Action::CopyPath), None);
        assert_eq!(
            km.label_for(Mode::Insert, Action::Accept(AcceptKind::Tab))
                .as_deref(),
            Some("^y")
        );
    }

    #[test]
    fn a_project_star_override_replaces_both_default_mode_bindings() {
        let mut cfg = Config::default();
        cfg.keys
            .entry("projects".into())
            .or_default()
            .insert("star".into(), "alt-f".into());
        let km = Keymap::load(&cfg);

        for mode in [Mode::Insert, Mode::Normal] {
            assert_eq!(
                km.action(mode, alt(Key::Char('f'))),
                Some(Action::ToggleStar)
            );
            assert_eq!(
                km.label_for(mode, Action::ToggleStar).as_deref(),
                Some("⌥f")
            );
        }
        for mode in [Mode::Insert, Mode::Normal] {
            assert_eq!(km.action(mode, ctrl(Key::Char('s'))), None);
        }
    }
}
