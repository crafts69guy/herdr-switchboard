//! Private, persistent stars for durable Projects entries.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::data::{Entry, Kind};

#[cfg(not(test))]
const STATE_FILE: &str = "project-stars.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum StarKind {
    Repo,
    Worktree,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct StarKey {
    kind: StarKind,
    id: String,
}

impl StarKey {
    fn from_entry(entry: &Entry) -> Option<Self> {
        let kind = match entry.kind {
            Kind::Repo => StarKind::Repo,
            Kind::Worktree => StarKind::Worktree,
            Kind::Agent | Kind::Workspace => return None,
        };
        Some(Self {
            kind,
            id: entry.id.clone(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Stars {
    keys: BTreeSet<StarKey>,
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
        StarKey::from_entry(entry).is_some()
    }

    pub(super) fn contains(&self, entry: &Entry) -> bool {
        StarKey::from_entry(entry).is_some_and(|key| self.keys.contains(&key))
    }

    /// Persist one desired state and return the complete updated snapshot.
    ///
    /// Re-read immediately before the mutation so two Projects panes opened at
    /// different times do not overwrite changes that one of them already saved.
    pub(super) fn set(self, entry: &Entry, starred: bool) -> Result<Self> {
        let key = StarKey::from_entry(entry).context("entry kind cannot be starred")?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow!("no state directory is available"))?;
        let keys = crate::state::update_private(&path, |current| {
            let mut keys = current.map(parse).unwrap_or_default();
            if starred {
                keys.insert(key);
            } else {
                keys.remove(&key);
            }
            let bytes = serde_json::to_vec_pretty(&keys).context("serialize project stars")?;
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
            keys: entries.iter().filter_map(StarKey::from_entry).collect(),
            path: None,
        }
    }
}

/// Missing, unreadable, and malformed state are all an empty star list. Stars
/// are convenience state and must never prevent Projects from opening.
fn load_from(path: &Path) -> BTreeSet<StarKey> {
    let Ok(bytes) = fs::read(path) else {
        return BTreeSet::new();
    };
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> BTreeSet<StarKey> {
    serde_json::from_slice::<Vec<StarKey>>(bytes)
        .ok()
        .unwrap_or_default()
        .into_iter()
        .filter(|key| !key.id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use ratatui::style::Color;

    use super::*;

    fn test_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is after epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "switchboard-project-stars-{}-{nonce}",
                std::process::id()
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
