//! Persistence for the recoverable zen session and chrome snapshot.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::geometry::Anchor;
use crate::chrome;
use crate::state;

const STATE_FILE: &str = "zen.json";
const CHROME_ABSENT: &str = "-";
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub target: String,
    pub zen_tab: String,
    pub origin_tab: String,
    pub gutters: Vec<String>,
    /// The pane the target sat next to before it left, and how. `None` when the
    /// target was alone in its tab and there is nothing to restore it against.
    pub anchor: Option<Anchor>,
}

// State file
// ---------------------------------------------------------------------------

/// The session is stored as flat lines rather than JSON: it is written and read
/// only here, and hand-rolling it keeps the format as inspectable as the rest of
/// this plugin's state files (`recent.tsv`, `update.tsv`).
pub(super) fn encode(session: &Session) -> String {
    let mut out = format!(
        "target\t{}\nzen_tab\t{}\norigin_tab\t{}\n",
        session.target, session.zen_tab, session.origin_tab
    );
    for gutter in &session.gutters {
        out.push_str(&format!("gutter\t{gutter}\n"));
    }
    if let Some(anchor) = &session.anchor {
        out.push_str(&format!(
            "anchor\t{}\t{}\t{}\t{}\t{}\n",
            anchor.pane, anchor.split, anchor.ratio, anchor.target_first, anchor.exact
        ));
    }
    out
}

/// The chrome snapshot, one `key<TAB>want<TAB>prior` line each. A prior of `-`
/// means "the key was absent"; none of the `[ui]` keys zen touches can hold a
/// bare dash, so the marker cannot collide with a real value.
pub(super) fn encode_chrome(overrides: &[chrome::Override]) -> String {
    overrides
        .iter()
        .map(|change| {
            format!(
                "{}\t{}\t{}\n",
                change.key,
                change.want,
                change.prior.as_deref().unwrap_or(CHROME_ABSENT)
            )
        })
        .collect()
}

pub(super) fn decode_chrome(text: &str) -> Vec<chrome::Override> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let key = parts.next().filter(|k| !k.is_empty())?.to_string();
            let want = parts.next()?.to_string();
            let prior = parts.next().unwrap_or(CHROME_ABSENT);
            Some(chrome::Override {
                key,
                want,
                prior: (prior != CHROME_ABSENT).then(|| prior.to_string()),
            })
        })
        .collect()
}

pub(super) fn decode(text: &str) -> Option<Session> {
    let mut session = Session {
        target: String::new(),
        zen_tab: String::new(),
        origin_tab: String::new(),
        gutters: Vec::new(),
        anchor: None,
    };
    for line in text.lines() {
        let mut parts = line.split('\t');
        match (parts.next(), parts.next()) {
            (Some("target"), Some(v)) => session.target = v.to_string(),
            (Some("zen_tab"), Some(v)) => session.zen_tab = v.to_string(),
            (Some("origin_tab"), Some(v)) => session.origin_tab = v.to_string(),
            (Some("gutter"), Some(v)) => session.gutters.push(v.to_string()),
            (Some("anchor"), Some(pane)) => {
                session.anchor = Some(Anchor {
                    pane: pane.to_string(),
                    split: parts.next().unwrap_or("right").to_string(),
                    ratio: parts.next().and_then(|r| r.parse().ok()).unwrap_or(0.5),
                    target_first: parts.next() == Some("true"),
                    exact: parts.next() == Some("true"),
                })
            }
            _ => {}
        }
    }
    (!session.target.is_empty() && !session.zen_tab.is_empty()).then_some(session)
}

/// Where a zen session is remembered.
///
/// Passed in explicitly rather than reached for through [`state::state_file`],
/// because `enter` and `leave` genuinely write and delete this file: a unit test
/// exercising them against the default path deletes the *user's live session*
/// out from under them mid-run. (It did, once — a `cargo test` between two
/// toggles left a real zen'd pane stranded in an orphan tab.) Tests point it at
/// a temp dir; production uses the state dir.
pub struct SessionStore(Option<PathBuf>);

impl SessionStore {
    pub fn new() -> Self {
        Self(state::state_file(STATE_FILE))
    }

    #[cfg(test)]
    pub(super) fn at(path: PathBuf) -> Self {
        Self(Some(path))
    }

    pub fn load(&self) -> Option<Session> {
        decode(&fs::read_to_string(self.0.as_ref()?).ok()?)
    }

    pub(super) fn save(&self, session: &Session) -> Result<()> {
        let path = self.0.as_ref().context("no state dir for zen")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(path, encode(session)).with_context(|| format!("write {}", path.display()))
    }

    pub(super) fn clear(&self) {
        if let Some(path) = &self.0 {
            fs::remove_file(path).ok();
        }
    }

    /// The chrome snapshot's own file, beside the session's.
    ///
    /// It is deliberately *not* part of the session record: the two have
    /// different lifetimes. A session ends the moment the pane is back, but a
    /// snapshot must outlive a restore herdr refused — that is the only copy of
    /// what the user's `[ui]` keys used to be, and `zen chrome-restore` is what
    /// comes looking for it.
    fn chrome_path(&self) -> Option<PathBuf> {
        Some(self.0.as_ref()?.with_extension("chrome.tsv"))
    }

    pub fn load_chrome(&self) -> Vec<chrome::Override> {
        self.chrome_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| decode_chrome(&text))
            .unwrap_or_default()
    }

    /// Best effort: a snapshot that cannot be written means chrome cannot be
    /// undone later, so [`enter`] checks the result and rolls the change back
    /// rather than leaving the user's config altered with no way home.
    pub(super) fn save_chrome(&self, overrides: &[chrome::Override]) -> Result<()> {
        let path = self.chrome_path().context("no state dir for zen")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&path, encode_chrome(overrides))
            .with_context(|| format!("write {}", path.display()))
    }

    pub(super) fn clear_chrome(&self) {
        if let Some(path) = self.chrome_path() {
            fs::remove_file(path).ok();
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}
