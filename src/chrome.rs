//! herdr's own chrome, borrowed for the length of a zen session.
//!
//! Zen can centre a pane and dim its gutters, but the sidebar rail, the tab row,
//! the pane borders and the scrollbar column are herdr's, and **none of them has
//! a CLI verb or a socket method**: `herdr pane|tab|server|config --help` expose
//! nothing UI-shaped, there is no `ui.*` in the socket API, and `herdr config`
//! has no `set`. They are global `[ui]` keys in herdr's own `config.toml`, and
//! `herdr server reload-config` is the only runtime lever.
//!
//! So this module — the one place in the plugin that writes a file it does not
//! own — rewrites those keys on the way into zen and puts them back on the way
//! out. Everything is built around *restore is never lost*:
//!
//! - the prior value of every key is captured **before** the write and returned
//!   to the caller, which persists it in its own state file — one that outlives
//!   the session, because a restore herdr refuses must still be retryable;
//!   `prior == None` means the key was absent and must be *removed* again, not
//!   written back as a default,
//! - a key already at the value zen wants is not in the plan at all, so a
//!   double-apply can never snapshot zen's own writes over the user's, and a user
//!   who keeps `pane_borders = false` permanently never sees it move,
//! - [`Override`] round-trips through the TOML literal, so `"compact"` comes back
//!   as `"compact"` and not as some normalised re-render,
//! - the untouched original is copied once to the plugin's state dir, so there is
//!   a human-recoverable file even if the snapshot is lost,
//! - the edit goes through `toml_edit`, so comments, ordering and every unrelated
//!   key survive, and it lands atomically (temp + rename).
//!
//! Every entry point fails soft. An unreadable config, a refused write or a dead
//! server leaves zen working exactly as it did before this module existed.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Value};

use crate::runner::CommandRunner;
use crate::state;

/// The table every key below lives in.
const UI: &str = "ui";

/// Where the untouched config is copied the first time zen rewrites it.
pub const BACKUP: &str = "herdr-config.backup.toml";

/// The paint-only keys: borders, the gaps between panes, and the scrollbar
/// column. These are what turn the gutters from two visible boxes into darkness.
const PANES: &[(&str, &str)] = &[
    ("pane_borders", "false"),
    ("pane_gaps", "false"),
    ("pane_scrollbars", "false"),
];

/// The structural keys, on top of [`PANES`]: the tab row (which hides only while
/// the zen tab is alone in its workspace — the usual case) and the sidebar.
///
/// herdr documents `sidebar_start_collapsed` as taking effect on the next launch,
/// so the sidebar half of this may not apply live; zen measures the result rather
/// than assuming it, and says so when it did not move.
const FULL: &[(&str, &str)] = &[
    ("hide_tab_bar_when_single_tab", "true"),
    ("sidebar_start_collapsed", "true"),
    ("sidebar_collapsed_mode", "\"hidden\""),
];

/// How much of herdr's chrome zen suppresses, from the `zen_chrome` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Off,
    Panes,
    Full,
}

impl Level {
    /// An unknown value degrades to [`Level::Off`] rather than erroring: the
    /// setting decides how a session *looks*, and a typo must not stop zen.
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "panes" => Level::Panes,
            "full" => Level::Full,
            _ => Level::Off,
        }
    }

    fn wants(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Level::Off => Vec::new(),
            Level::Panes => PANES.to_vec(),
            Level::Full => PANES.iter().chain(FULL).copied().collect(),
        }
    }
}

/// One `[ui]` key zen changed, and what it has to become again.
///
/// `want` and `prior` are TOML literals (`false`, `"hidden"`) rather than typed
/// values, because that is what round-trips: the literal is what was in the file
/// and the literal is what goes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    pub key: String,
    pub want: String,
    /// `None` when the key was absent, which restores as a removal.
    pub prior: Option<String>,
}

/// The keys that actually need changing for `level`, each carrying the value it
/// has right now. Keys already at the wanted value are left out entirely.
pub fn plan_overrides(doc: &DocumentMut, level: Level) -> Vec<Override> {
    level
        .wants()
        .into_iter()
        .filter_map(|(key, want)| {
            let prior = doc
                .get(UI)
                .and_then(|ui| ui.get(key))
                .and_then(Item::as_value)
                .map(render);
            (prior.as_deref() != Some(want)).then(|| Override {
                key: key.to_string(),
                want: want.to_string(),
                prior,
            })
        })
        .collect()
}

/// A value as it should appear in the file — `to_string` keeps the decoration
/// (the leading space of ` = false`), which would compound on every round trip.
fn render(value: &Value) -> String {
    value.to_string().trim().to_string()
}

fn literal(raw: &str) -> Option<Value> {
    raw.parse::<Value>().ok()
}

pub fn apply(doc: &mut DocumentMut, overrides: &[Override]) {
    for change in overrides {
        if let Some(value) = literal(&change.want) {
            ui_table(doc)[change.key.as_str()] = Item::Value(value);
        }
    }
}

/// Put every key back the way [`plan_overrides`] found it, and take the `[ui]`
/// table with it if zen is what created it.
pub fn restore(doc: &mut DocumentMut, overrides: &[Override]) {
    for change in overrides {
        match change.prior.as_deref().and_then(literal) {
            Some(value) => ui_table(doc)[change.key.as_str()] = Item::Value(value),
            None => {
                if let Some(ui) = doc.get_mut(UI).and_then(Item::as_table_like_mut) {
                    ui.remove(&change.key);
                }
            }
        }
    }
    if doc
        .get(UI)
        .and_then(Item::as_table_like)
        .is_some_and(|ui| ui.is_empty())
    {
        doc.remove(UI);
    }
}

fn ui_table(doc: &mut DocumentMut) -> &mut Item {
    if doc.get(UI).is_none() {
        doc[UI] = Item::Table(toml_edit::Table::new());
    }
    &mut doc[UI]
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// herdr's `config.toml`. The live socket path sits next to it, which makes it
/// the authoritative answer inside a session; the XDG fallbacks are for a plugin
/// invoked by hand from outside one. herdr has no `--config` flag to ask.
pub fn config_path() -> PathBuf {
    #[cfg(test)]
    if let Some(scratch) = crate::state::test_scratch() {
        return scratch.join("herdr/config.toml");
    }
    config_path_from(
        env::var("HERDR_SOCKET_PATH").ok(),
        env::var("XDG_CONFIG_HOME").ok(),
        env::var("HOME").ok(),
    )
}

/// [`config_path`] over explicit values: the socket's directory, then
/// `$XDG_CONFIG_HOME/herdr`, then `~/.config/herdr`, then a relative fallback.
fn config_path_from(socket: Option<String>, xdg: Option<String>, home: Option<String>) -> PathBuf {
    let dir = socket
        .filter(|path| !path.is_empty())
        .and_then(|path| PathBuf::from(path).parent().map(Path::to_path_buf))
        .or_else(|| {
            xdg.filter(|dir| !dir.is_empty())
                .map(|dir| PathBuf::from(dir).join("herdr"))
        })
        .or_else(|| home.map(|home| PathBuf::from(home).join(".config/herdr")))
        .unwrap_or_else(|| PathBuf::from(".config/herdr"));
    dir.join("config.toml")
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    text.parse::<DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))
}

/// Write through a temp file in the same directory, so a crash mid-write cannot
/// leave herdr with half a config.
///
/// This is the one file outside the plugin anything here may write, which makes
/// it the one that most deserves the shared machinery: `replace_atomically`
/// serializes two zen enters against each other — they were racing on a single
/// fixed sibling name — and preserves the mode herdr's config already has,
/// which a state-directory writer would not.
fn write_document(path: &Path, doc: &DocumentMut) -> Result<()> {
    state::replace_atomically(path, doc.to_string().as_bytes())
}

/// Keep one copy of the config as it was before zen ever touched it. Best
/// effort, and never overwritten — the point is the *original*, not the latest.
fn backup_once(path: &Path, backup: Option<PathBuf>) {
    let Some(backup) = backup else {
        return;
    };
    if backup.exists() {
        return;
    }
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::copy(path, &backup).ok();
}

fn reload<R: CommandRunner>(runner: &R) -> bool {
    // `capture`, not `ok`: herdr answers over stdout and the zen toggle opens no
    // pane, so anything printed lands in the user's terminal.
    runner
        .capture("herdr", &["server", "reload-config"])
        .is_some()
}

/// Suppress `level`'s worth of chrome and tell herdr to pick it up. Returns the
/// snapshot the caller must persist and later hand to [`disengage`]; an empty
/// vec means nothing was changed and there is nothing to undo.
pub fn engage<R: CommandRunner>(runner: &R, level: Level) -> Vec<Override> {
    engage_at(runner, level, &config_path(), state::state_file(BACKUP))
}

/// [`engage`] against an explicit config file and backup location.
fn engage_at<R: CommandRunner>(
    runner: &R,
    level: Level,
    path: &Path,
    backup: Option<PathBuf>,
) -> Vec<Override> {
    if level == Level::Off {
        return Vec::new();
    }
    let Ok(mut doc) = read_document(path) else {
        return Vec::new();
    };
    let overrides = plan_overrides(&doc, level);
    if overrides.is_empty() {
        return Vec::new();
    }
    backup_once(path, backup);
    apply(&mut doc, &overrides);
    if write_document(path, &doc).is_err() {
        return Vec::new();
    }
    reload(runner);
    overrides
}

/// Undo [`engage`]. Reports whether the config is back the way it was, so the
/// caller can keep the snapshot when it is not.
pub fn disengage<R: CommandRunner>(runner: &R, overrides: &[Override]) -> bool {
    disengage_at(runner, overrides, &config_path())
}

fn disengage_at<R: CommandRunner>(runner: &R, overrides: &[Override], path: &Path) -> bool {
    if overrides.is_empty() {
        return true;
    }
    let Ok(mut doc) = read_document(path) else {
        return false;
    };
    restore(&mut doc, overrides);
    if write_document(path, &doc).is_err() {
        return false;
    }
    reload(runner);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    /// Each level asks for strictly more than the one below it. `Off` asks for
    /// nothing at all, which is what makes it the safe default.
    #[test]
    fn each_level_asks_for_more_than_the_one_below() {
        let doc = "".parse::<DocumentMut>().unwrap();
        let keys = |level| {
            plan_overrides(&doc, level)
                .into_iter()
                .map(|change| change.key)
                .collect::<std::collections::BTreeSet<_>>()
        };

        let off = keys(Level::Off);
        let panes = keys(Level::Panes);
        let full = keys(Level::Full);

        assert!(off.is_empty(), "Off must change nothing: {off:?}");
        assert!(!panes.is_empty());
        assert!(
            panes.is_subset(&full),
            "Full must include everything Panes does"
        );
        assert!(full.len() > panes.len(), "Full must go further than Panes");
    }

    /// An unknown level degrades to `Off` rather than erroring: this setting
    /// decides how a session looks, and a typo must not stop zen.
    #[test]
    fn an_unknown_level_degrades_to_off() {
        assert_eq!(Level::parse("off"), Level::Off);
        assert_eq!(Level::parse("panes"), Level::Panes);
        assert_eq!(Level::parse("full"), Level::Full);
        assert_eq!(
            Level::parse("  full  "),
            Level::Full,
            "whitespace is trimmed"
        );
        assert_eq!(Level::parse("maximum"), Level::Off);
        assert_eq!(Level::parse(""), Level::Off);
    }

    /// A value that is not valid TOML is skipped rather than written as a
    /// literal — a malformed prior would corrupt the user's config on restore.
    #[test]
    fn a_prior_that_is_not_valid_toml_is_skipped_rather_than_written() {
        let mut doc = "[ui]\nsidebar = true\n".parse::<DocumentMut>().unwrap();
        let bogus = vec![Override {
            key: "sidebar".into(),
            want: "not = valid = toml".into(),
            prior: Some("true".into()),
        }];

        apply(&mut doc, &bogus);
        let written = doc.to_string();
        assert!(
            written.contains("sidebar = true"),
            "a malformed value reached the file: {written}"
        );
    }

    const SAMPLE: &str = "\
# herdr config
[ui]
# Draw borders around split panes.
pane_borders = true
accent = \"#6FD0A8\"

[experimental]
kitty_graphics = true
";

    fn doc(text: &str) -> DocumentMut {
        text.parse().expect("sample parses")
    }

    #[test]
    fn an_unknown_level_is_off() {
        assert_eq!(Level::parse("panes"), Level::Panes);
        assert_eq!(Level::parse(" full "), Level::Full);
        assert_eq!(Level::parse("yes"), Level::Off);
        assert_eq!(Level::parse(""), Level::Off);
    }

    #[test]
    fn off_plans_nothing() {
        assert!(plan_overrides(&doc(SAMPLE), Level::Off).is_empty());
    }

    #[test]
    fn a_present_key_snapshots_its_literal_and_an_absent_one_snapshots_none() {
        let plan = plan_overrides(&doc(SAMPLE), Level::Panes);
        let borders = plan.iter().find(|o| o.key == "pane_borders").unwrap();
        assert_eq!(borders.prior.as_deref(), Some("true"));
        let gaps = plan.iter().find(|o| o.key == "pane_gaps").unwrap();
        assert_eq!(gaps.prior, None);
    }

    #[test]
    fn a_key_already_at_the_wanted_value_is_not_planned() {
        let plan = plan_overrides(&doc("[ui]\npane_borders = false\n"), Level::Panes);
        assert!(!plan.iter().any(|o| o.key == "pane_borders"));
        assert!(plan.iter().any(|o| o.key == "pane_gaps"));
    }

    #[test]
    fn full_adds_the_tab_bar_and_sidebar_keys() {
        let keys: Vec<String> = plan_overrides(&doc(SAMPLE), Level::Full)
            .into_iter()
            .map(|o| o.key)
            .collect();
        for key in [
            "pane_borders",
            "pane_gaps",
            "pane_scrollbars",
            "hide_tab_bar_when_single_tab",
            "sidebar_start_collapsed",
            "sidebar_collapsed_mode",
        ] {
            assert!(keys.contains(&key.to_string()), "{key} missing from full");
        }
    }

    #[test]
    fn apply_then_restore_leaves_the_file_byte_for_byte() {
        let mut document = doc(SAMPLE);
        let plan = plan_overrides(&document, Level::Full);
        apply(&mut document, &plan);
        let applied = document.to_string();
        assert!(applied.contains("pane_borders = false"));
        assert!(applied.contains("sidebar_collapsed_mode = \"hidden\""));
        // The comment and the unrelated keys are still there mid-session.
        assert!(applied.contains("# Draw borders around split panes."));
        assert!(applied.contains("accent = \"#6FD0A8\""));

        restore(&mut document, &plan);
        assert_eq!(document.to_string(), SAMPLE);
    }

    #[test]
    fn a_created_ui_table_is_removed_again() {
        let bare = "[experimental]\nkitty_graphics = true\n";
        let mut document = doc(bare);
        let plan = plan_overrides(&document, Level::Full);
        apply(&mut document, &plan);
        assert!(document.to_string().contains("pane_borders = false"));
        restore(&mut document, &plan);
        assert_eq!(document.to_string(), bare);
    }

    #[test]
    fn a_string_prior_round_trips_as_a_string() {
        let text = "[ui]\nsidebar_collapsed_mode = \"compact\"\n";
        let mut document = doc(text);
        let plan = plan_overrides(&document, Level::Full);
        let mode = plan
            .iter()
            .find(|o| o.key == "sidebar_collapsed_mode")
            .unwrap();
        assert_eq!(mode.prior.as_deref(), Some("\"compact\""));
        apply(&mut document, &plan);
        restore(&mut document, &plan);
        assert!(document.to_string().contains("\"compact\""));
    }

    /// The config sits beside the live socket; outside a session it falls back
    /// through XDG and HOME, and never resolves to nothing.
    #[test]
    fn herdrs_config_is_found_beside_the_socket_then_by_xdg_then_home() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            config_path_from(some("/run/h/herdr.sock"), some("/x"), some("/home/u")),
            PathBuf::from("/run/h/config.toml")
        );
        assert_eq!(
            config_path_from(some(""), some("/x"), some("/home/u")),
            PathBuf::from("/x/herdr/config.toml")
        );
        assert_eq!(
            config_path_from(None, some(""), some("/home/u")),
            PathBuf::from("/home/u/.config/herdr/config.toml")
        );
        assert_eq!(
            config_path_from(None, None, None),
            PathBuf::from(".config/herdr/config.toml")
        );
        assert!(config_path().starts_with(crate::state::test_scratch().unwrap()));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swb-chrome-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Engaging writes the overrides, keeps one backup of the original, and
    /// asks herdr to reload; disengaging puts every value back. All of it on a
    /// scratch copy, at the `panes` level — never herdr's own file.
    #[test]
    fn engaging_and_disengaging_round_trip_a_scratch_config() {
        let dir = scratch("roundtrip");
        let path = dir.join("config.toml");
        let backup = dir.join("state/backup.toml");
        let original = "[ui]\npane_borders = true\n";
        fs::write(&path, original).unwrap();
        let runner = MockRunner::new();

        let overrides = engage_at(&runner, Level::Panes, &path, Some(backup.clone()));
        assert!(!overrides.is_empty());
        assert_ne!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        // A second engage never replaces the backup of the original.
        fs::write(&path, "[ui]\n").unwrap();
        engage_at(&runner, Level::Panes, &path, Some(backup.clone()));
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        fs::write(&path, original).unwrap();
        engage_at(&runner, Level::Panes, &path, None);

        assert!(disengage_at(&runner, &overrides, &path));
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("pane_borders = true"));
        assert!(runner
            .calls()
            .iter()
            .all(|call| call == &["herdr", "server", "reload-config"]));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Nothing to do, nothing readable, or nothing writable: no change is
    /// claimed, and herdr is not asked to reload.
    #[test]
    fn engage_and_disengage_change_nothing_they_cannot_finish() {
        let dir = scratch("refusals");
        let runner = MockRunner::new();
        let missing = dir.join("missing.toml");
        assert!(engage_at(&runner, Level::Off, &missing, None).is_empty());
        assert!(engage_at(&runner, Level::Panes, &missing, None).is_empty());
        assert!(disengage_at(&runner, &[], &missing));

        let path = dir.join("config.toml");
        fs::write(&path, "[ui]\npane_borders = true\n").unwrap();
        let overrides = engage_at(&runner, Level::Panes, &path, None);
        assert!(!disengage_at(&runner, &overrides, &missing));

        // A config already at the zen values needs no overrides at all.
        let calm = dir.join("calm.toml");
        fs::write(&calm, fs::read_to_string(&path).unwrap()).unwrap();
        assert!(engage_at(&runner, Level::Panes, &calm, None).is_empty());

        // A directory where the file should be cannot be written.
        let blocked = dir.join("blocked");
        fs::create_dir_all(blocked.join("config.toml")).unwrap();
        assert!(engage_at(&runner, Level::Panes, &blocked.join("config.toml"), None).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
