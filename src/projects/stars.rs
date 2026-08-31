//! Private, persistent stars for durable Projects entries.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::data::{Entry, Kind};

#[cfg(not(test))]
const STATE_FILE: &str = "project-stars.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StarKind {
    Repo,
    Worktree,
}

/// One persisted star. This is the *file* shape only — the in-memory set is
/// [`StarSet`], and the order records appear in comes from its two sets rather
/// than from sorting these, which is why they carry no `Ord`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StarKey {
    kind: StarKind,
    id: String,
}

/// The star kind an entry maps to, or `None` for the live kinds whose IDs must
/// never reach durable state.
fn star_kind(entry: &Entry) -> Option<StarKind> {
    match entry.kind {
        Kind::Repo => Some(StarKind::Repo),
        Kind::Worktree => Some(StarKind::Worktree),
        Kind::Agent | Kind::Workspace => None,
    }
}

/// The in-memory star set: one ID set per kind rather than a set of composite
/// keys.
///
/// [`contains`](StarSet::contains) sits on the hottest path in the whole picker
/// — it is asked once per entry per keystroke by the reducer, once per entry per
/// group by the tab counts, and once per entry per frame by the list — so it has
/// to answer from a borrowed `&str`. A `BTreeSet<StarKey>` cannot: building the
/// probe key means cloning the entry's ID, which turned a set lookup into an
/// allocation. Splitting by kind makes the ID the whole key and the lookup free.
///
/// [`StarKey`] remains the on-disk shape, so the file this reads and writes is
/// byte-for-byte what it always was.
#[derive(Clone, Debug, Default)]
struct StarSet {
    repos: BTreeSet<String>,
    worktrees: BTreeSet<String>,
}

impl StarSet {
    fn ids(&self, kind: StarKind) -> &BTreeSet<String> {
        match kind {
            StarKind::Repo => &self.repos,
            StarKind::Worktree => &self.worktrees,
        }
    }

    fn ids_mut(&mut self, kind: StarKind) -> &mut BTreeSet<String> {
        match kind {
            StarKind::Repo => &mut self.repos,
            StarKind::Worktree => &mut self.worktrees,
        }
    }

    fn contains(&self, kind: StarKind, id: &str) -> bool {
        self.ids(kind).contains(id)
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.repos.is_empty() && self.worktrees.is_empty()
    }

    /// The persisted form: every key, ordered by kind then ID. `StarKind::Repo`
    /// sorts before `StarKind::Worktree`, so emitting the repo set first
    /// reproduces exactly the order a `BTreeSet<StarKey>` produced.
    fn to_keys(&self) -> Vec<StarKey> {
        let key = |kind: StarKind| {
            self.ids(kind).iter().map(move |id| StarKey {
                kind,
                id: id.clone(),
            })
        };
        key(StarKind::Repo).chain(key(StarKind::Worktree)).collect()
    }

    fn from_keys(keys: impl IntoIterator<Item = StarKey>) -> Self {
        let mut set = Self::default();
        for key in keys {
            if !key.id.is_empty() {
                set.ids_mut(key.kind).insert(key.id);
            }
        }
        set
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Stars {
    keys: StarSet,
    path: Option<PathBuf>,
}

impl Stars {
    pub(super) fn load() -> Self {
        #[cfg(test)]
        {
            Self::default()
        }
        #[cfg(not(test))]
        {
            let path = crate::state::state_file(STATE_FILE);
            let keys = path.as_deref().map(load_from).unwrap_or_default();
            Self { keys, path }
        }
    }

    pub(super) fn supports(entry: &Entry) -> bool {
        star_kind(entry).is_some()
    }

    /// Whether `entry` is starred. Allocation-free on purpose — see [`StarSet`].
    pub(super) fn contains(&self, entry: &Entry) -> bool {
        star_kind(entry).is_some_and(|kind| self.keys.contains(kind, &entry.id))
    }

    /// Persist one desired state and return the complete updated snapshot.
    ///
    /// Re-read immediately before the mutation so two Projects panes opened at
    /// different times do not overwrite changes that one of them already saved.
    pub(super) fn set(self, entry: &Entry, starred: bool) -> Result<Self> {
        let kind = star_kind(entry).context("entry kind cannot be starred")?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow!("no state directory is available"))?;
        let keys = crate::state::update_private(&path, |current| {
            let mut keys = current.map(parse).unwrap_or_default();
            if starred {
                keys.ids_mut(kind).insert(entry.id.clone());
            } else {
                keys.ids_mut(kind).remove(&entry.id);
            }
            let bytes =
                serde_json::to_vec_pretty(&keys.to_keys()).context("serialize project stars")?;
            Ok((bytes, keys))
        })?;
        Ok(Self {
            keys,
            path: Some(path),
        })
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        let keys = load_from(&path);
        Self {
            keys,
            path: Some(path),
        }
    }

    #[cfg(test)]
    pub(super) fn memory(entries: &[Entry]) -> Self {
        Self {
            keys: StarSet::from_keys(entries.iter().filter_map(|entry| {
                Some(StarKey {
                    kind: star_kind(entry)?,
                    id: entry.id.clone(),
                })
            })),
            path: None,
        }
    }
}

/// Missing, unreadable, and malformed state are all an empty star list. Stars
/// are convenience state and must never prevent Projects from opening.
fn load_from(path: &Path) -> StarSet {
    let Ok(bytes) = fs::read(path) else {
        return StarSet::default();
    };
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> StarSet {
    StarSet::from_keys(
        serde_json::from_slice::<Vec<StarKey>>(bytes)
            .ok()
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use ratatui::style::Color;

    use super::*;

    /// A path no other test can collide with.
    ///
    /// The nanosecond clock alone was not enough: tests run in parallel threads
    /// and macOS does not hand out a distinct nanosecond per call, so two
    /// fixtures could land on one directory — and one of these tests puts a
    /// *directory* where another expects a file.
    fn test_path(name: &str) -> PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir()
            .join(format!(
                "switchboard-project-stars-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ))
            .join(name)
    }

    fn entry(kind: Kind, id: &str) -> Entry {
        Entry {
            kind,
            id: id.into(),
            dir: None,
            label: id.into(),
            icon: String::new(),
            icon_color: Color::Reset,
            primary: id.into(),
            secondary: String::new(),
            search: id.into(),
        }
    }

    #[test]
    fn only_repositories_and_worktrees_are_supported() {
        assert!(Stars::supports(&entry(Kind::Repo, "github.com/o/repo")));
        assert!(Stars::supports(&entry(Kind::Worktree, "/tmp/repo.feature")));
        assert!(!Stars::supports(&entry(Kind::Agent, "pane-1")));
        assert!(!Stars::supports(&entry(Kind::Workspace, "workspace-1")));
    }

    #[test]
    fn missing_and_malformed_state_are_empty() {
        let path = test_path("stars.json");
        assert!(Stars::at(path.clone()).keys.is_empty());

        fs::create_dir_all(path.parent().expect("test path has a parent")).unwrap();
        fs::write(&path, "not json").unwrap();
        assert!(Stars::at(path.clone()).keys.is_empty());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn set_round_trips_sorted_typed_stars_and_unstars() {
        let path = test_path("stars.json");
        let repo = entry(Kind::Repo, "github.com/z/repo");
        let worktree = entry(Kind::Worktree, "/tmp/a-worktree");

        let stars = Stars::at(path.clone()).set(&worktree, true).unwrap();
        let stars = stars.set(&repo, true).unwrap();
        assert!(stars.contains(&repo));
        assert!(stars.contains(&worktree));

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.find("github.com/z/repo").unwrap() < text.find("/tmp/a-worktree").unwrap());

        let stars = stars.set(&repo, false).unwrap();
        assert!(!stars.contains(&repo));
        assert!(stars.contains(&worktree));
        assert!(fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// The in-memory set is split by kind so a lookup can borrow the entry's ID,
    /// but the file is still a flat array of `{kind, id}` ordered by kind then
    /// ID. This pins both directions: a file written before the split still
    /// loads, and what we write back is byte-identical to what we read.
    #[test]
    fn the_persisted_shape_survives_the_split_by_kind() {
        let path = test_path("stars.json");
        let existing = "[\
            {\"kind\":\"repo\",\"id\":\"github.com/o/a\"},\
            {\"kind\":\"repo\",\"id\":\"github.com/o/b\"},\
            {\"kind\":\"worktree\",\"id\":\"/tmp/wt\"}]";
        fs::create_dir_all(path.parent().expect("test path has a parent")).unwrap();
        fs::write(&path, existing).unwrap();

        let stars = Stars::at(path.clone());
        assert!(stars.contains(&entry(Kind::Repo, "github.com/o/a")));
        assert!(stars.contains(&entry(Kind::Repo, "github.com/o/b")));
        assert!(stars.contains(&entry(Kind::Worktree, "/tmp/wt")));
        // A worktree and a repo that share an ID are different stars.
        assert!(!stars.contains(&entry(Kind::Worktree, "github.com/o/a")));
        assert!(!stars.contains(&entry(Kind::Repo, "/tmp/wt")));

        // Star and unstar the same entry: the file must come back to the shape
        // and order it started in.
        let extra = entry(Kind::Repo, "github.com/o/c");
        let stars = stars.set(&extra, true).unwrap();
        stars.set(&extra, false).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::from_str::<serde_json::Value>(existing).unwrap()
        );
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn concurrent_panes_keep_every_completed_star_update() {
        let path = test_path("stars.json");
        let entries = (0..8)
            .map(|index| entry(Kind::Repo, &format!("github.com/o/repo-{index}")))
            .collect::<Vec<_>>();
        let ready = Arc::new(Barrier::new(entries.len() + 1));
        let workers = entries
            .iter()
            .cloned()
            .map(|entry| {
                let path = path.clone();
                let ready = Arc::clone(&ready);
                thread::spawn(move || {
                    let stars = Stars::at(path);
                    ready.wait();
                    stars.set(&entry, true).unwrap();
                })
            })
            .collect::<Vec<_>>();

        ready.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        let stars = Stars::at(path.clone());
        assert!(entries.iter().all(|entry| stars.contains(entry)));
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn state_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let path = test_path("stars.json");
        Stars::at(path.clone())
            .set(&entry(Kind::Repo, "github.com/o/repo"), true)
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_write_failure_leaves_the_original_snapshot_unchanged() {
        let path = test_path("stars.json");
        let repo = entry(Kind::Repo, "github.com/o/repo");
        fs::create_dir_all(&path).unwrap();

        let stars = Stars::at(path.clone());
        assert!(stars.clone().set(&repo, true).is_err());
        assert!(!stars.contains(&repo));
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
